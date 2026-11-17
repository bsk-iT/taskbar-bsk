// Endgame 4K FF01:0002 report A1: payload starts at 16, status 03 means busy.
// Status 08/11 means asleep; identity returns VID/PID little endian.
// Sources: johanneszab/vole and Janlhin/Endgame_Gear_Web_Configurator.
use super::{Driver, PollCtx};
use crate::{
    device::{Kind, Observation, State},
    hid::HidDev,
};
use std::{collections::HashMap, thread::sleep, time::Duration};

fn mouse_name(pid: u16) -> Option<&'static str> {
    match pid {
        0x1972 => Some("Endgame Gear OP1w 4k"),
        0x1984 => Some("Endgame Gear OP1w 4k v2"),
        0x1968 => Some("Endgame Gear XM2w 4k"),
        0x1982 => Some("Endgame Gear XM2w 4k v2"),
        _ => None,
    }
}
fn parse_status(r: &[u8]) -> Option<u8> {
    if r.len() >= 20 && r.first() == Some(&0xa1) {
        r.get(1).copied()
    } else {
        None
    }
}
fn parse_name(r: &[u8]) -> Option<&'static str> {
    if r.get(16..18) != Some(&[0x67, 0x33][..]) {
        return None;
    }
    mouse_name(u16::from_le_bytes([*r.get(18)?, *r.get(19)?]))
}
// A1 config channel: B4 battery, 0E identity, 0F recovery probe only on failure.
fn command(d: &HidDev, cmd: u8, target: u8) -> Option<Vec<u8>> {
    let mut req = [0; 64];
    req[0] = 0xa1;
    req[1] = cmd;
    req[2] = target;
    if !d.set_feature(&req) {
        return None;
    }
    let mut last = None;
    for attempt in 0..3 {
        sleep(Duration::from_millis(150 + attempt * 100));
        let r = d.get_feature(0xa1)?;
        let status = parse_status(&r)?;
        if status != 3 {
            return Some(r);
        }
        last = Some(r);
    }
    last
}
#[derive(Default)]
struct Endgame {
    last_name: HashMap<Vec<u16>, &'static str>,
}
pub fn driver() -> Box<dyn Driver> {
    Box::new(Endgame::default())
}
impl Driver for Endgame {
    fn name(&self) -> &'static str {
        "endgame"
    }
    fn poll(&mut self, ctx: &mut PollCtx) {
        if ctx.is_claimed(0x3367, 0x1970) {
            return;
        }
        let colls = ctx
            .colls
            .iter()
            .filter(|c| c.vid == 0x3367 && c.pid == 0x1970 && c.page == 0xff01 && c.usage == 2 && c.feat_len >= 64);
        for c in colls {
            let Some(d) = HidDev::open_feature_only(c) else { continue };
            let mut answer = command(&d, 0xb4, 0);
            if !matches!(answer.as_deref().and_then(parse_status), Some(1 | 8 | 0x11)) {
                let _ = command(&d, 0x0f, 1);
                let _ = command(&d, 0x0f, 0x0f);
                answer = command(&d, 0xb4, 0);
            }
            let Some(r) = answer else { continue };
            let status = parse_status(&r);
            let pct = r.get(16).copied().filter(|p| *p <= 100);
            let awake = status == Some(1) && pct.is_some();
            if !awake && !matches!(status, Some(8 | 0x11)) {
                continue;
            }
            if awake {
                if let Some(info) = command(&d, 0x0e, 0) {
                    if parse_status(&info) == Some(1) {
                        if let Some(name) = parse_name(&info) {
                            self.last_name.insert(c.path.clone(), name);
                        }
                    }
                }
            }
            let name = self.last_name.get(&c.path).copied().unwrap_or("Endgame Gear 4k wireless mouse");
            let obs = match pct.filter(|_| awake) {
                Some(p) => Observation::battery("endgame:3367:1970", name, Kind::Mouse, p, false),
                None => Observation::new("endgame:3367:1970", State::Unreachable).name(name).kind(Kind::Mouse),
            };
            ctx.add(obs.via("2.4 GHz receiver"), 0x3367, 0x1970);
            break;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn status_and_identity() {
        let mut r = [0; 20];
        r[0] = 0xa1;
        r[1] = 1;
        r[16..20].copy_from_slice(&[0x67, 0x33, 0x84, 0x19]);
        assert_eq!(parse_status(&r), Some(1));
        assert_eq!(parse_name(&r), Some("Endgame Gear OP1w 4k v2"));
        assert_eq!(parse_status(&r[..19]), None);
        assert_eq!(parse_name(&r[..19]), None);
    }
}
