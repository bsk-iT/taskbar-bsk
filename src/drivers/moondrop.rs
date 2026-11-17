use super::{simple, Driver, PollCtx};
use crate::device::{Kind, Observation, State};
use crate::hid::HidDev;
use std::time::{Duration, Instant};

// WalkPlay READ only: 4B 80 cmd 00. Battery 27 returns link/percent at 8/9;
// link 31 is answered by the dongle even when the headset has switched to its cable.
fn parse_reply(r: &[u8], cmd: u8) -> bool {
    r.len() >= 5 && r.get(..3) == Some(&[0x4b, 0x80, cmd])
}
fn parse_battery(r: &[u8]) -> Option<u8> {
    if !parse_reply(r, 0x27) || r.get(3).copied()? < 6 || r.get(8).copied()? == 0 {
        return None;
    }
    r.get(9).copied().filter(|&p| p <= 100)
}
fn query(d: &HidDev, cmd: u8) -> Option<Vec<u8>> {
    d.flush();
    if !d.write(&[0x4b, 0x80, cmd, 0], 500) {
        return None;
    }
    let end = Instant::now() + Duration::from_millis(400);
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        let r = d.read(left.as_millis() as u32)?;
        if r.len() < 5 {
            return None;
        }
        if parse_reply(&r, cmd) {
            return Some(r);
        }
    }
    None
}
pub fn driver() -> Box<dyn Driver> {
    simple("moondrop", poll)
}
fn poll(ctx: &mut PollCtx) {
    const VID: u16 = 0x35d8;
    const PID: u16 = 0x0133;
    if ctx.is_claimed(VID, PID) {
        return;
    }
    let status = |state| {
        Observation::new("moondrop:35d8:0133", state)
            .name("MOONDROP GM01 Pro")
            .kind(Kind::Headset)
            .grace(Duration::from_secs(1800))
    };
    let mut found = false;
    let mut dongle_unlinked = false;
    for c in ctx.find(VID, PID).filter(|c| c.iface == 3 && c.page == 0x000c) {
        found = true;
        if c.out_len < 4 || c.in_len < 10 {
            continue;
        }
        let Some(d) = HidDev::open(c) else {
            continue;
        };
        if let Some(p) = query(&d, 0x27).and_then(|r| parse_battery(&r)) {
            ctx.add(status(State::Battery { percent: p, charging: false }).via("2.4 GHz receiver"), VID, PID);
            return;
        }
        if let Some(r) = query(&d, 0x31) {
            if r.get(3).is_some_and(|&n| n >= 1) && r.get(4) == Some(&0) {
                dongle_unlinked = true;
            }
        }
    }
    if !found {
        return;
    }
    // Windows adds &colNN only for multiple top-level collections; cable interface 3 has one.
    let on_cable = ctx
        .find(VID, PID)
        .filter(|c| c.iface == 3)
        .any(|c| !String::from_utf16_lossy(&c.path).to_lowercase().contains("&col"));
    if on_cable {
        ctx.add(status(State::ChargingUnknown).via("USB cable"), VID, PID);
    } else if dongle_unlinked {
        ctx.add(status(State::Off).via("2.4 GHz receiver"), VID, PID);
    } else {
        ctx.claim(VID, PID);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn walkplay_frames() {
        let mut r = [0x4b, 0x80, 0x27, 6, 0, 0, 0, 0, 1, 50];
        assert_eq!(parse_battery(&r), Some(50));
        r[8] = 0;
        assert_eq!(parse_battery(&r), None);
        r[8] = 1;
        r[9] = 101;
        assert_eq!(parse_battery(&r), None);
        assert_eq!(parse_battery(&r[..9]), None);
        assert!(parse_reply(&[0x4b, 0x80, 0x31, 1, 0], 0x31));
        assert!(!parse_reply(&[0x4b, 1, 0x31, 1, 0], 0x31));
    }
}
