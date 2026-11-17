use super::{simple, Driver, PollCtx};
use crate::{
    device::{Kind, Observation},
    hid::HidDev,
};
use std::time::{Duration, Instant};

// Telink M3 protocol: github.com/Barba2k2/darmoshark-m3-configurator/PROTOCOL.md.
fn id(r: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*r.get(off)?, *r.get(off + 1)?]))
}
fn valid_reply(r: &[u8], opcode: u8) -> bool {
    r.len() >= 12 && r.first() == Some(&0x51) && r.get(1) == Some(&opcode)
}
fn parse_bond(r: &[u8]) -> bool {
    valid_reply(r, 3) && id(r, 3) == Some(0x248a) && id(r, 5) == Some(0xff12) && r.get(7).is_some_and(|b| *b != 0)
}
fn parse_battery(r: &[u8]) -> Option<u8> {
    if !valid_reply(r, 6) || r.get(2) != Some(&1) || id(r, 4) != Some(0x248a) || id(r, 6) != Some(0xff12) {
        return None;
    }
    r.get(11).copied().filter(|p| *p <= 100)
}
fn query(d: &HidDev, opcode: u8, receiver_end: Instant) -> Option<Vec<u8>> {
    let start = Instant::now();
    if start >= receiver_end {
        return None;
    }
    let end = receiver_end.min(start + Duration::from_millis(500));
    d.flush();
    if Instant::now() >= end || !d.set_feature(&[0x51, opcode]) {
        return None;
    }
    while Instant::now() < end {
        let wait = end.saturating_duration_since(Instant::now()).as_millis().min(20) as u32;
        if let Some(r) = d.read(wait) {
            if r.get(..3) == Some(&[0x54, 0xe4, 2]) {
                return None;
            }
        }
        if Instant::now() >= end {
            return None;
        }
        if let Some(r) = d.get_feature(0x51) {
            if valid_reply(&r, opcode) {
                return (Instant::now() < end).then_some(r);
            }
        }
    }
    None
}
fn read_battery(d: &HidDev, end: Instant) -> Option<u8> {
    // Bond query replaces stale battery data before requesting identification.
    if !parse_bond(&query(d, 3, end)?) {
        return None;
    }
    parse_battery(&query(d, 6, end)?)
}
fn poll(ctx: &mut PollCtx) {
    if ctx.is_claimed(0x248a, 0xff30) {
        return;
    }
    for c in ctx.find(0x248a, 0xff30).filter(|c| {
        c.iface == 1 && c.page == 0x8c && c.usage == 1 && c.in_len == 33 && c.out_len == 33 && c.feat_len == 1001
    }) {
        let end = Instant::now() + Duration::from_millis(1000);
        let Some(d) = HidDev::open(c) else {
            continue;
        };
        let first = end.min(Instant::now() + Duration::from_millis(300));
        let p = read_battery(&d, first).or_else(|| if Instant::now() < end { read_battery(&d, end) } else { None });
        let Some(p) = p else {
            continue;
        };
        let path = String::from_utf16_lossy(&c.path);
        ctx.add(
            Observation::battery(format!("darmoshark:248a:ff30:{path}"), "Darmoshark M3", Kind::Mouse, p, false)
                .via("2.4 GHz receiver"),
            0x248a,
            0xff30,
        );
        return;
    }
}
pub fn driver() -> Box<dyn Driver> {
    simple("Darmoshark", poll)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bond_and_identification() {
        let mut bond = [0x51, 3, 0, 0x8a, 0x24, 0x12, 0xff, 1, 0, 0, 0, 0];
        assert!(parse_bond(&bond));
        bond[7] = 0;
        assert!(!parse_bond(&bond));
        let mut r = [0x51, 6, 1, 0, 0x8a, 0x24, 0x12, 0xff, 0, 0, 0, 64];
        assert_eq!(parse_battery(&r), Some(64));
        for n in 0..12 {
            assert_eq!(parse_battery(&r[..n]), None);
            assert!(!parse_bond(&bond[..n]));
        }
        r[11] = 101;
        assert_eq!(parse_battery(&r), None);
        r[11] = 0;
        assert_eq!(parse_battery(&r), Some(0));
        r[6] = 0x13;
        assert_eq!(parse_battery(&r), None);
    }
}
