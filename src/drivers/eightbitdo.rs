//! Ultimate 2 D-input firmware >= 1.03 streams battery in report 01, byte 14.
use super::{simple, Driver, PollCtx};
use crate::{
    device::{Kind, Observation},
    hid::HidDev,
};
use std::time::{Duration, Instant};
pub fn driver() -> Box<dyn Driver> {
    simple("8BitDo", poll)
}
fn parse(r: &[u8]) -> Option<(u8, bool)> {
    if r.len() < 34 || r.first() != Some(&1) {
        return None;
    }
    let value = *r.get(14)?;
    let level = value & 0x7f;
    (level <= 100).then_some((level, value & 0x80 != 0 && level < 100))
}
fn poll(ctx: &mut PollCtx) {
    for (vid, pid) in ctx.products(0x2dc8) {
        if pid != 0x6012 {
            continue;
        }
        for c in
            ctx.colls.iter().filter(|c| c.vid == vid && c.pid == pid && c.page == 1 && c.usage == 5 && c.in_len >= 34)
        {
            let Some(d) = HidDev::open_read_only(c) else { continue };
            let end = Instant::now() + Duration::from_millis(600);
            let mut battery = None;
            while Instant::now() < end {
                if let Some(r) = d.read(100) {
                    if let Some(p) = parse(&r) {
                        battery = Some(p);
                        break;
                    }
                }
            }
            let Some((p, ch)) = battery else { continue };
            let path = String::from_utf16_lossy(&c.path);
            ctx.add(
                Observation::battery(format!("8bitdo:{vid:04x}:{pid:04x}"), "8BitDo Ultimate 2", Kind::Gamepad, p, ch)
                    .via(if path.contains("00001124") { "Bluetooth" } else { "2.4 GHz dongle or USB" }),
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
    #[test]
    fn battery_frames() {
        let mut r = [0; 34];
        r[0] = 1;
        r[14] = 0xe3;
        assert_eq!(parse(&r), Some((99, true)));
        r[14] = 0xe4;
        assert_eq!(parse(&r), Some((100, false)));
        r[14] = 0xe5;
        assert_eq!(parse(&r), None);
        assert_eq!(parse(&r[..12]), None);
    }
}
