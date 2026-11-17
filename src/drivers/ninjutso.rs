// NinjaForce Sora V2: report 05 command 15, [9]=percent (0 means 100),
// [10]=charging [12]=online; cable remains connected even with [12]=0.
// Ten/Ten Air: report 06, commands 13/12/11 read online/percent/charging at [8].
// Sources: NinjaForce bundled createSoraV2 and DrStopor/ninjutso-battery-tray.
use super::{simple, Driver, PollCtx};
use crate::{
    device::{Kind, Observation, State},
    hid::{self, HidColl, HidDev},
};
use std::{thread::sleep, time::Duration};
fn sora_request() -> [u8; 32] {
    let mut r = [0; 32];
    r[0] = 5;
    r[1] = 0x15;
    r[4] = 1;
    r
}
fn parse_sora(r: &[u8], cable: bool) -> Option<State> {
    if r.len() < 13 || r.first() != Some(&5) || r.get(1) != Some(&0x15) {
        return None;
    }
    let pct = *r.get(9)?;
    if pct > 100 {
        return None;
    }
    Some(if cable || *r.get(12)? != 0 {
        State::Battery { percent: if pct == 0 { 100 } else { pct }, charging: *r.get(10)? != 0 }
    } else {
        State::Off
    })
}
fn ten_request(cmd: u8) -> [u8; 16] {
    let mut r = [0; 16];
    r[0] = 6;
    r[1] = cmd;
    r[4] = 1;
    r
}
fn parse_ten(r: &[u8], cmd: u8) -> Option<u8> {
    if r.len() != 16 || r.first() != Some(&6) || r.get(1) != Some(&cmd) {
        return None;
    }
    r.get(8).copied()
}
fn query_ten(d: &HidDev, cmd: u8) -> Option<u8> {
    if !d.set_feature(&ten_request(cmd)) {
        return None;
    }
    for _ in 0..3 {
        sleep(Duration::from_millis(30));
        if let Some(v) = d.get_feature(6).and_then(|r| parse_ten(&r, cmd)) {
            return Some(v);
        }
    }
    None
}
fn sora_matches(c: &HidColl) -> bool {
    c.vid == 0x1915 && matches!(c.pid, 0xae1c | 0xae11) && c.page == 0xffa0 && c.usage == 1 && c.feat_len >= 32
}
fn ten_matches(c: &HidColl) -> bool {
    c.vid == 0x093a && matches!(c.pid, 0xeb01 | 0xe020) && c.page == 0xff01 && c.usage == 1 && c.feat_len == 16
}
pub fn driver() -> Box<dyn Driver> {
    simple("ninjutso", poll)
}
fn poll(ctx: &mut PollCtx) {
    for pid in [0xae1c, 0xae11] {
        if ctx.is_claimed(0x1915, pid) {
            continue;
        }
        let cable = pid == 0xae11;
        let colls = ctx.colls.iter().filter(|c| c.pid == pid && sora_matches(c));
        for c in colls {
            let Some(d) = HidDev::open_feature_only(c) else { continue };
            let mut state = None;
            for attempt in 0..4 {
                if attempt > 0 {
                    sleep(Duration::from_millis(90));
                }
                if !d.set_feature(&sora_request()) {
                    break;
                }
                sleep(Duration::from_millis(15));
                if let Some(parsed) = d.get_feature(5).and_then(|r| parse_sora(&r, cable)) {
                    state = Some(parsed);
                    if parsed != State::Off {
                        break;
                    }
                }
            }
            if let Some(state) = state {
                ctx.add(
                    Observation::new(format!("ninjutso:1915:{pid:04x}"), state)
                        .name("Ninjutso Sora V2")
                        .kind(Kind::Mouse)
                        .via(if cable { "USB cable" } else { "2.4 GHz receiver" }),
                    0x1915,
                    pid,
                );
                break;
            }
        }
    }
    poll_ten(ctx);
}
fn poll_ten(ctx: &mut PollCtx) {
    for (vid, pid) in ctx.products(0x093a) {
        let colls = ctx.colls.iter().filter(|c| c.pid == pid && ten_matches(c));
        for c in colls {
            let Some(d) = HidDev::open_feature_only(c) else { continue };
            if query_ten(&d, 0x13) != Some(1) {
                continue;
            }
            let Some(pct @ 0..=100) = query_ten(&d, 0x12) else { continue };
            let Some(charging) = query_ten(&d, 0x11) else { continue };
            let name = hid::product(c);
            let name = if name.is_empty() { "Ninjutso Ten / Ten Air" } else { &name };
            ctx.add(
                Observation::battery(
                    format!("ninjutso-ten:{vid:04x}:{pid:04x}"),
                    name,
                    Kind::Mouse,
                    pct,
                    charging != 0,
                )
                .via(if pid == 0xe020 { "USB cable" } else { "2.4 GHz receiver" }),
                vid,
                pid,
            );
            break;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn status(pct: u8, charging: bool, online: bool) -> [u8; 32] {
        let mut r = sora_request();
        r[9] = pct;
        r[10] = charging as u8;
        r[12] = online as u8;
        r
    }
    #[test]
    fn upstream_sora_and_cable_fixtures() {
        for (pct, charging, online, cable, expected) in [
            (32, false, true, false, 32),
            (0, true, true, false, 100),
            (31, false, true, false, 31),
            (0x4c, true, false, true, 76),
            (40, false, true, false, 40),
            (41, true, false, true, 41),
            (55, true, false, true, 55),
            (0, true, false, true, 100),
        ] {
            assert_eq!(
                parse_sora(&status(pct, charging, online), cable),
                Some(State::Battery { percent: expected, charging })
            );
        }
        assert_eq!(parse_sora(&status(0, true, false), false), Some(State::Off));
        let mut id = status(32, false, true);
        id[0] = 4;
        let mut cmd = status(32, false, true);
        cmd[1] = 0x28;
        for bad in [id.to_vec(), cmd.to_vec(), vec![0; 12], status(101, false, true).to_vec(), vec![]] {
            assert_eq!(parse_sora(&bad, false), None);
        }
        assert_eq!(&sora_request()[..5], &[5, 0x15, 0, 0, 1]);
        assert!(sora_request()[5..].iter().all(|b| *b == 0));
    }
    #[test]
    fn upstream_ten_fixtures() {
        for (cmd, value) in
            [(0x13, 1), (0x12, 67), (0x11, 0), (0x13, 0), (0x12, 101), (0x12, 20), (0x11, 1), (0x12, 0), (0x12, 100)]
        {
            let mut r = ten_request(cmd);
            r[8] = value;
            assert_eq!(parse_ten(&r, cmd), Some(value));
        }
        let mut wrong = ten_request(0x13);
        wrong[0] = 7;
        for bad in [vec![], vec![6], wrong.to_vec(), ten_request(0x12).to_vec(), vec![0; 17], vec![6, 0]] {
            assert_eq!(parse_ten(&bad, 0x13), None);
        }
        assert_eq!(&ten_request(0x13)[..5], &[6, 0x13, 0, 0, 1]);
    }
    #[test]
    fn upstream_collection_filters() {
        let sora = HidColl { vid: 0x1915, pid: 0xae1c, page: 0xffa0, usage: 1, feat_len: 704, ..Default::default() };
        assert!(sora_matches(&sora));
        for variant in 0..5 {
            let mut c = sora.clone();
            match variant {
                0 => c.vid = 0x1234,
                1 => c.pid = 0xae8c,
                2 => c.page = 1,
                3 => c.usage = 2,
                _ => c.feat_len = 31,
            };
            assert!(!sora_matches(&c));
        }
        let ten = HidColl { vid: 0x093a, pid: 0xeb01, page: 0xff01, usage: 1, feat_len: 16, ..Default::default() };
        assert!(ten_matches(&ten));
        for variant in 0..7 {
            let mut c = ten.clone();
            match variant {
                0 => c.vid = 0x1234,
                1 => c.pid = 0xea01,
                2 => c.page = 0xff00,
                3 => c.page = 0xff02,
                4 => c.usage = 2,
                5 => c.feat_len = 64,
                _ => c.feat_len = 0,
            };
            assert!(!ten_matches(&c));
        }
    }
}
