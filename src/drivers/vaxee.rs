// VAXEE FF05:0001 report 0E: A5 command 01 01; READ flag is 01, never WRITE 02.
// Battery command 0B returns 0..20 in five-percent steps; 10 reads charging.
// Source: stuffz/mouse-battery-monitor docs/VAXEE.md.
use super::{Driver, PollCtx};
use crate::{
    device::{Kind, Observation, State},
    hid::{HidColl, HidDev},
};
use std::{collections::HashMap, thread::sleep, time::Duration};

fn matches(c: &HidColl) -> bool {
    c.vid == 0x3057 && matches!(c.pid, 0x1002 | 0x2002 | 0x1011) && c.page == 0xff05 && c.usage == 1 && c.feat_len == 64
}
fn request(cmd: u8) -> [u8; 64] {
    let mut r = [0; 64];
    r[..5].copy_from_slice(&[0x0e, 0xa5, cmd, 1, 1]);
    r
}
fn parse_reply(r: &[u8], cmd: u8) -> Option<u8> {
    if r.first() != Some(&0x0e) || r.get(1) != Some(&0xa5) || r.get(2) != Some(&cmd) || r.get(4) != Some(&1) {
        return None;
    }
    r.get(5).copied()
}
fn query(d: &HidDev, cmd: u8) -> Option<u8> {
    if !d.set_feature(&request(cmd)) {
        return None;
    }
    sleep(Duration::from_millis(100));
    parse_reply(&d.get_feature(0x0e)?, cmd)
}
#[derive(Default)]
struct Vaxee {
    last_good: HashMap<Vec<u16>, Observation>,
}
impl Vaxee {
    fn expire(&mut self, colls: &[HidColl], out: &mut Vec<Observation>) {
        self.last_good.retain(|path, obs| {
            if colls.iter().any(|c| &c.path == path) {
                return true;
            }
            let mut gone = obs.clone();
            gone.state = State::Off;
            out.push(gone);
            false
        });
    }
}
pub fn driver() -> Box<dyn Driver> {
    Box::new(Vaxee::default())
}
impl Driver for Vaxee {
    fn name(&self) -> &'static str {
        "vaxee"
    }
    fn poll(&mut self, ctx: &mut PollCtx) {
        self.expire(ctx.colls, &mut ctx.out);
        for (vid, pid) in ctx.products(0x3057) {
            let colls = ctx.colls.iter().filter(|c| c.pid == pid && matches(c));
            for c in colls {
                let Some(d) = HidDev::open_feature_only(c) else { continue };
                let mut battery = None;
                for _ in 0..2 {
                    battery = query(&d, 0x0b).filter(|b| *b <= 20);
                    if battery.is_some() {
                        break;
                    }
                }
                let Some(battery) = battery else { continue };
                let name = match pid {
                    0x1011 => "ZYGEN NP-01S V2 Wireless",
                    0x2002 => "VAXEE 8K Wireless",
                    _ => "VAXEE wireless mouse",
                };
                let charging = query(&d, 0x10).is_some_and(|b| b != 0);
                let path = String::from_utf16_lossy(c.path.split(|u| *u == 0).next().unwrap_or_default());
                let obs = Observation::battery(
                    format!("vaxee:{vid:04x}:{pid:04x}:{path}"),
                    name,
                    Kind::Mouse,
                    battery * 5,
                    charging,
                )
                .via(if pid == 0x1011 { "USB cable" } else { "2.4 GHz receiver" })
                .grace(Duration::from_secs(1800));
                self.last_good.insert(c.path.clone(), obs.clone());
                ctx.add(obs, vid, pid);
                break;
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn reply(cmd: u8, value: u8) -> [u8; 64] {
        let mut r = request(cmd);
        r[5] = value;
        r
    }
    #[test]
    fn upstream_reply_fixtures() {
        for (raw, pct) in [(13, 65), (12, 60), (14, 70), (20, 100), (0, 0), (7, 35)] {
            assert_eq!(parse_reply(&reply(11, raw), 11).filter(|b| *b <= 20).map(|b| b * 5), Some(pct));
        }
        assert_eq!(parse_reply(&reply(11, 21), 11).filter(|b| *b <= 20), None);
        for offset in [0, 1, 2, 4] {
            let mut bad = reply(11, 10);
            bad[offset] = 0;
            assert_eq!(parse_reply(&bad, 11), None);
        }
        assert_eq!(parse_reply(&reply(11, 10), 16), None);
        assert_eq!(parse_reply(&[0; 5], 11), None);
        assert_eq!(parse_reply(&reply(16, 1), 16), Some(1));
        assert_eq!(&request(11)[..6], &[14, 165, 11, 1, 1, 0]);
        assert!(request(16)[5..].iter().all(|b| *b == 0));
    }
    #[test]
    fn collection_filter_and_unplug() {
        let c = HidColl {
            vid: 0x3057,
            pid: 0x1002,
            page: 0xff05,
            usage: 1,
            feat_len: 64,
            path: vec![1, 0],
            ..Default::default()
        };
        assert!(matches(&c));
        for variant in 0..5 {
            let mut bad = c.clone();
            match variant {
                0 => bad.vid = 0x1234,
                1 => bad.pid = 2,
                2 => bad.page = 1,
                3 => bad.usage = 2,
                _ => bad.feat_len = 63,
            };
            assert!(!matches(&bad));
        }
        let mut driver = Vaxee::default();
        let mut out = Vec::new();
        driver.last_good.insert(
            c.path.clone(),
            Observation::battery("vaxee:3057:1002:test", "VAXEE wireless mouse", Kind::Mouse, 65, true),
        );
        driver.expire(&[c], &mut out);
        assert!(out.is_empty());
        driver.expire(&[], &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].state, State::Off);
        driver.expire(&[], &mut out);
        assert_eq!(out.len(), 1);
    }
}
