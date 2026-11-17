//! Sony controllers stream battery status. Calibration report 05 enables full Bluetooth reports.
use super::{simple_every, Driver, PollCtx, FAST};
use crate::device::{Kind, Observation};
use crate::hid::{clamp100, HidDev};
use std::time::{Duration, Instant};

const SONY: u16 = 0x054c;
const DUALSHOCK4: &[u16] = &[0x05c4, 0x09cc, 0x0ba0];

pub fn driver() -> Box<dyn Driver> {
    simple_every("playstation", FAST, poll)
}

fn poll(ctx: &mut PollCtx) {
    for c in ctx.colls {
        if c.vid != SONY
            || !(DUALSHOCK4.contains(&c.pid) || [0x0ce6, 0x0df2].contains(&c.pid))
            || c.page != 1
            || c.usage != 5
        {
            continue;
        }
        let Some(dev) = HidDev::open(c) else { continue };
        let bt = c.in_len > 64;
        if bt {
            let _ = dev.get_feature(0x05);
        }
        let end = Instant::now() + Duration::from_millis(500);
        while Instant::now() < end {
            let Some(r) = dev.read(100) else { continue };
            if c.pid == 0x0ba0 && r.first() == Some(&1) && r.get(30) == Some(&0) {
                break;
            }
            let Some((pct, charging)) = parse_status(&r, !DUALSHOCK4.contains(&c.pid), bt) else { continue };
            let hash = c
                .path
                .iter()
                .take_while(|&&u| u != 0)
                .fold(2166136261u32, |h, &u| (h ^ u32::from(u)).wrapping_mul(16777619));
            let name = match c.pid {
                0x0ce6 => "DualSense",
                0x0df2 => "DualSense Edge",
                _ => "DualShock 4",
            };
            ctx.add(
                Observation::battery(format!("ps:054c:{:04x}:{hash}", c.pid), name, Kind::Gamepad, pct, charging).via(
                    if bt {
                        "Bluetooth"
                    } else if c.pid == 0x0ba0 {
                        "Wireless adapter"
                    } else {
                        "USB cable"
                    },
                ),
                c.vid,
                c.pid,
            );
            break;
        }
    }
}

fn parse_status(r: &[u8], dualsense: bool, bt: bool) -> Option<(u8, bool)> {
    let at = match (dualsense, *r.first()?) {
        (_, 1) if bt => return None,
        (true, 1) => 53,
        (true, 0x31) => 54,
        (false, 1) => 30,
        (false, 0x11) => 32,
        _ => return None,
    };
    let status = *r.get(at)?;
    Some(if dualsense { dualsense_status(status) } else { dualshock4_status(status) })
}

fn dualsense_status(s: u8) -> (u8, bool) {
    let percent = clamp100(i32::from(s & 15) * 10 + 5);
    if s >> 4 == 2 {
        (100, true)
    } else {
        (percent, s >> 4 == 1)
    }
}

fn dualshock4_status(s: u8) -> (u8, bool) {
    let level = i32::from(s & 15);
    let cable = s & 16 != 0;
    (clamp100(level * 10 + if cable { 0 } else { 5 }), cable && level < 11)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dualsense() {
        assert_eq!(dualsense_status(0x08), (85, false));
        assert_eq!(dualsense_status(0x13), (35, true));
        assert_eq!(dualsense_status(0x2a), (100, true));
        assert_eq!(dualsense_status(0x0a), (100, false));
        assert_eq!(dualsense_status(0xff), (100, false));
    }
    #[test]
    fn dualshock4() {
        assert_eq!(dualshock4_status(0x05), (55, false));
        assert_eq!(dualshock4_status(0x14), (40, true));
        assert_eq!(dualshock4_status(0x1b), (100, false));
    }
    #[test]
    fn report_offsets() {
        for (sense, id, at) in [(true, 1, 53), (true, 0x31, 54), (false, 1, 30), (false, 0x11, 32)] {
            let mut r = vec![0; at + 1];
            r[0] = id;
            r[at] = 5;
            assert_eq!(parse_status(&r, sense, id != 1), Some((55, false)));
            assert_eq!(parse_status(&r[..at], sense, false), None);
        }
        assert_eq!(parse_status(&[1; 64], true, true), None);
        assert_eq!(parse_status(&[], false, false), None);
    }
}
