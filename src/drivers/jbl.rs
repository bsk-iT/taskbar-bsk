use super::{simple, Driver, PollCtx};
use crate::device::{Kind, Observation};
use crate::hid::HidDev;
use std::time::{Duration, Instant};

// QuantumENGINE HIDV3: feature 88 requests DD00 / GET_DEVICE_INFO / battery feature 000D.
const REQUEST: &[u8] = &[0x88, 0, 0xdd, 1, 0, 1, 0, 2, 0, 0x0d, 0];
const MODELS: &[(u16, &str, bool)] = &[
    (0x2125, "JBL Quantum 650 Wireless", true),
    (0x2126, "JBL Quantum 650 Wireless", false),
    (0x213b, "JBL Quantum 650X Wireless", true),
    (0x213d, "JBL Quantum 650X Wireless", false),
    (0x2127, "JBL Quantum 950 Wireless", true),
    (0x2128, "JBL Quantum 950 Wireless", false),
    (0x213e, "JBL Quantum 950X Wireless", true),
    (0x2140, "JBL Quantum 950X Wireless", false),
    (0x218d, "JBL Quantum 520 Wireless", true),
    (0x218e, "JBL Quantum 520 Wireless", false),
    (0x21a7, "JBL Quantum 520X Wireless", true),
    (0x21a9, "JBL Quantum 520X Wireless", false),
    (0x219a, "JBL Quantum 850 Wireless", true),
    (0x219b, "JBL Quantum 850 Wireless", false),
];

#[derive(Debug, PartialEq)]
enum Reply {
    Battery(u8, bool),
    Skip,
    Failed,
}
fn parse_battery(r: &[u8]) -> Reply {
    if r.len() < 16 {
        return Reply::Failed;
    }
    if r.get(..6) != Some(&[0x86, 0, 0xdd, 1, 0, 1]) {
        return Reply::Skip;
    }
    if r.get(10).is_some_and(|b| b & 0x80 != 0) {
        return Reply::Failed;
    }
    if r.get(11..13) != Some(&[0x0d, 0]) || r.get(13..15) == Some(&[0, 0]) {
        return Reply::Skip;
    }
    match r.get(15) {
        Some(&info) if info & 0x7f <= 100 => Reply::Battery(info & 0x7f, info & 0x80 != 0),
        _ => Reply::Failed,
    }
}
fn get_battery(d: &HidDev) -> Option<(u8, bool)> {
    d.flush();
    if !d.set_feature(REQUEST) {
        return None;
    }
    let end = Instant::now() + Duration::from_millis(800);
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        match parse_battery(&d.read(left.as_millis() as u32)?) {
            Reply::Battery(p, c) => return Some((p, c)),
            Reply::Failed => return None,
            Reply::Skip => (),
        }
    }
    None
}
pub fn driver() -> Box<dyn Driver> {
    simple("jbl", poll)
}
fn poll(ctx: &mut PollCtx) {
    for &(pid, name, dongle) in MODELS {
        if ctx.is_claimed(0x0ecb, pid) {
            continue;
        }
        for c in ctx.find(0x0ecb, pid).filter(|c| c.page == 0xff12 && c.feat_len >= 11 && c.in_len >= 16) {
            let Some(d) = HidDev::open(c) else {
                continue;
            };
            let Some((p, charging)) = get_battery(&d) else {
                continue;
            };
            ctx.add(
                Observation::battery(format!("jbl:0ecb:{pid:04x}"), name, Kind::Headset, p, charging).via(if dongle {
                    "2.4 GHz receiver"
                } else {
                    "USB cable"
                }),
                0x0ecb,
                pid,
            );
            break;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn battery_frames() {
        let mut r = [0x86, 0, 0xdd, 1, 0, 1, 0, 0, 0, 0, 0, 0x0d, 0, 1, 0, 0xc9];
        assert_eq!(parse_battery(&r), Reply::Battery(73, true));
        r[15] = 101;
        assert_eq!(parse_battery(&r), Reply::Failed);
        r[10] = 0x80;
        assert_eq!(parse_battery(&r), Reply::Failed);
        r[10] = 0;
        r[11] = 1;
        assert_eq!(parse_battery(&r), Reply::Skip);
        assert_eq!(parse_battery(&r[..15]), Reply::Failed);
    }
}
