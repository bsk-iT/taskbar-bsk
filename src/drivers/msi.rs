use super::{simple, Driver, PollCtx};
use crate::{
    device::{Kind, Observation, State},
    hid::{self, HidColl, HidDev},
};
use std::time::{Duration, Instant};

// MSI Center GetBattery: 0F 20 only. Never send 41 80, which changes remote-control mode.
fn parse_battery(r: &[u8]) -> Option<(u8, bool)> {
    if r.first() != Some(&0x0f) || r.get(1) != Some(&0x20) {
        return None;
    }
    let b = *r.get(14)?;
    (b & 0x7f <= 100).then_some((b & 0x7f, b & 0x80 != 0))
}
fn query(c: &HidColl) -> Option<(u8, bool)> {
    let d = HidDev::open(c)?;
    d.flush();
    if !d.write(&[0, 0x0f, 0x20], 500) {
        return None;
    }
    let end = Instant::now() + Duration::from_millis(1000);
    loop {
        let left = end.saturating_duration_since(Instant::now()).as_millis() as u32;
        if left == 0 {
            return None;
        }
        let r = d.read(left)?;
        if r.len() >= 15 && r.first() == Some(&0x0f) && r.get(1) == Some(&0x20) {
            return parse_battery(&r);
        }
    }
}
fn poll(ctx: &mut PollCtx) {
    if ctx.is_claimed(0x0db0, 0x9e22) {
        return;
    }
    for c in ctx
        .find(0x0db0, 0x9e22)
        .filter(|c| c.iface == 1 && c.page == 0xff0d && c.usage == 0xb0 && c.out_len >= 65 && c.in_len >= 65)
    {
        let Some((p, ch)) = query(c) else {
            continue;
        };
        let mut name = hid::product(c);
        if name.is_empty() {
            name = "MSI VERSA 300 ELITE WIRELESS".into();
        }
        let state = if p == 0 && !ch { State::Unreachable } else { State::Battery { percent: p, charging: ch } };
        ctx.add(
            Observation::new("msi:0db0:9e22", state)
                .name(name)
                .kind(Kind::Mouse)
                .via("2.4 GHz receiver")
                .grace(Duration::from_secs(1800)),
            0x0db0,
            0x9e22,
        );
        break;
    }
}
pub fn driver() -> Box<dyn Driver> {
    simple("MSI", poll)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn charging_bit_and_bounds() {
        let mut r = [0; 15];
        r[0] = 0x0f;
        r[1] = 0x20;
        r[14] = 0x80 | 72;
        assert_eq!(parse_battery(&r), Some((72, true)));
        for n in 0..15 {
            assert_eq!(parse_battery(&r[..n]), None);
        }
        r[14] = 0;
        assert_eq!(parse_battery(&r), Some((0, false)));
        r[14] = 101;
        assert_eq!(parse_battery(&r), None);
    }
}
