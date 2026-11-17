use super::{simple, Driver, PollCtx};
use crate::{
    device::{Kind, Observation, State},
    hid::{self, HidDev},
};
use std::time::{Duration, Instant};

// sonix-keyboard-studio: report 0, 32 payload bytes; trailing byte is their sum.
fn sum(r: &[u8]) -> u8 {
    r.iter().fold(0u8, |s, b| s.wrapping_add(*b))
}
fn parse_level(r: &[u8]) -> Option<u8> {
    if r.first() != Some(&0x20) || r.get(1) != Some(&1) || *r.get(31)? != sum(r.get(..31)?) {
        return None;
    }
    r.get(3).copied()
}
fn query(d: &HidDev) -> Option<u8> {
    let mut req = [0; 33];
    req[1] = 0x20;
    req[2] = 1;
    req[32] = 0x21;
    d.flush();
    if !d.write(&req, 500) {
        return None;
    }
    let end = Instant::now() + Duration::from_millis(500);
    loop {
        let left = end.saturating_duration_since(Instant::now()).as_millis() as u32;
        if left == 0 {
            return None;
        }
        let r = d.read(left)?;
        if let Some(p) = parse_level(&r) {
            return Some(p);
        }
    }
}
fn poll(ctx: &mut PollCtx) {
    for (vid, pid) in [(0x05ac, 0x024f), (0x0c45, 0xfefe)] {
        if ctx.is_claimed(vid, pid) {
            continue;
        }
        for c in ctx
            .find(vid, pid)
            .filter(|c| c.iface == 3 && c.page == 0xff60 && c.usage == 0x61 && c.out_len == 33 && c.in_len == 33)
        {
            let Some(d) = HidDev::open(c) else {
                continue;
            };
            let Some(p) = query(&d).or_else(|| query(&d)) else {
                continue;
            };
            let mut name = hid::product(c);
            if name.len() > " 2.4G".len() && name.ends_with(" 2.4G") {
                name.truncate(name.len() - " 2.4G".len());
            }
            if name.is_empty() {
                name = "Wireless keyboard".into();
            }
            let state =
                if (1..=100).contains(&p) { State::Battery { percent: p, charging: false } } else { State::Off };
            ctx.add(
                Observation::new(format!("sonix:{vid:04x}:{pid:04x}"), state)
                    .name(name)
                    .kind(Kind::Keyboard)
                    .via("2.4 GHz receiver")
                    .grace(Duration::from_secs(1800)),
                vid,
                pid,
            );
            break;
        }
    }
}
pub fn driver() -> Box<dyn Driver> {
    simple("Sonix", poll)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checksum_and_level() {
        let mut r = [0; 32];
        r[0] = 0x20;
        r[1] = 1;
        r[3] = 80;
        r[31] = sum(&r[..31]);
        assert_eq!(parse_level(&r), Some(80));
        for n in 0..32 {
            assert_eq!(parse_level(&r[..n]), None);
        }
        r[31] ^= 1;
        assert_eq!(parse_level(&r), None);
        r[3] = 0;
        r[31] = sum(&r[..31]);
        assert_eq!(parse_level(&r), Some(0));
    }
}
