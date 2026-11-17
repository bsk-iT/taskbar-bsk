//! Valve Steam Controller input reports; layout follows Linux hid-steam.
use super::{simple, Driver, PollCtx};
use crate::{
    device::{Kind, Observation},
    hid::{self, HidDev},
};
use std::time::{Duration, Instant};

pub fn driver() -> Box<dyn Driver> {
    simple("Steam Controller", poll)
}

fn parse(report: &[u8], legacy: bool) -> Option<(u8, bool)> {
    if report.len() < 15 {
        return None;
    }
    if legacy {
        if report.get(..4)? != [1, 0, 4, 11] {
            return None;
        }
        let level = *report.get(14)?;
        (level <= 100).then_some((level, false))
    } else {
        let level = *report.get(2)?;
        (report.first() == Some(&0x43) && level <= 100).then_some((level, report.get(1) == Some(&2)))
    }
}

fn poll(ctx: &mut PollCtx) {
    for c in ctx.colls {
        if c.vid != 0x28de || c.page != 0xff00 || c.in_len < 15 {
            continue;
        }
        let via = match c.pid {
            0x1142 | 0x1304 | 0x1305 => "Wireless adapter",
            0x1302 => "USB cable",
            0x1303 => "Bluetooth",
            _ => continue,
        };
        let puck = c.pid == 0x1304;
        if puck && c.usage != 1 {
            continue;
        }
        let Some(dev) = HidDev::open(c) else { continue };
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut received = false;
        let mut battery = None;
        while Instant::now() < deadline {
            match dev.read(if puck && !received { 250 } else { 100 }) {
                Some(r) => {
                    received |= !r.is_empty();
                    if let Some(value) = parse(&r, c.pid == 0x1142) {
                        battery = Some(value);
                        break;
                    }
                }
                None if puck && !received => break,
                None => {}
            }
        }
        let Some((percent, charging)) = battery else { continue };
        let hash = c
            .path
            .iter()
            .take_while(|ch| **ch != 0)
            .fold(2166136261u32, |h, ch| (h ^ u32::from(*ch)).wrapping_mul(16777619));
        let mut name = hid::product(c);
        if matches!(c.pid, 0x1142 | 0x1304 | 0x1305) || name.is_empty() {
            name = "Steam Controller".into();
        }
        ctx.add(
            Observation::battery(format!("steam:28de:{:04x}:{hash}", c.pid), name, Kind::Gamepad, percent, charging)
                .via(via),
            c.vid,
            c.pid,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reports() {
        let mut r = [0; 15];
        r[..4].copy_from_slice(&[1, 0, 4, 11]);
        r[14] = 73;
        assert_eq!(parse(&r, true), Some((73, false)));
        r[14] = 101;
        assert_eq!(parse(&r, true), None);
        r[..3].copy_from_slice(&[0x43, 2, 82]);
        assert_eq!(parse(&r, false), Some((82, true)));
        assert_eq!(parse(&r[..14], false), None);
    }
}
