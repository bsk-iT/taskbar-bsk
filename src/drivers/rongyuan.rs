use super::{simple, Driver, PollCtx};
use crate::{
    device::{Kind, Observation, State},
    hid::{self, HidDev},
};
use std::{thread::sleep, time::Duration};

// BatteryTray/official ATTACK SHARK receiver list. F7 returns keyboard and mouse slots.
const RECEIVERS: &[u32] = &[
    0x0db0197e, 0x0db08e9a, 0x145f0376, 0x145f038d, 0x14a5c045, 0x25aa2008, 0x25aa4008, 0x25aa4018, 0x31514011,
    0x31514014, 0x31514017, 0x31514021, 0x31514027, 0x3151402d, 0x31514038, 0x31515006, 0x31515007, 0x31515008,
    0x31515020, 0x31515026, 0x3151502b, 0x31515031, 0x31515037, 0x31515038, 0x3151503d, 0x3151503f, 0x31515040,
    0x31515042, 0x31515045, 0x3151504b, 0x3151504e, 0x31515050, 0x31515052, 0x31515055, 0x3151505a, 0x342de3e0,
    0x342de3e1, 0x342de3e2, 0x342de3e6, 0x342de3e7, 0x342de3e8, 0x342de3f5, 0x342de3f6, 0x342de3f7, 0x342de3fb,
    0x342de3fc, 0x342de3fd, 0x347aa302, 0x347aa305, 0x374aa205, 0x374aa217, 0x374aa220, 0x374aa223, 0x374aa226,
    0x374aa229, 0x374aa234, 0x374aa237, 0x374aa251, 0x374aa263, 0x374aa268, 0x374aa311, 0x374aa314, 0x374aa319,
    0x374aa322, 0x374aa329, 0x374aa355, 0x379a1804, 0x379a1807, 0x38a90002, 0x38ee0000, 0x38ee0002, 0x38ee0039,
    0x391d1a04, 0x391d1a08, 0x391d1a0e, 0x391d1a13, 0x391d1a25, 0x391d1a2b, 0x391d1a34, 0x391d2017, 0x391d2a05,
];
fn valid_reply(r: &[u8]) -> bool {
    r.get(2..7).is_some_and(|v| v.iter().any(|b| *b != 0))
}
fn parse_slot(r: &[u8], keyboard: bool) -> Option<State> {
    if *r.get(if keyboard { 4 } else { 5 })? != 0 {
        return None;
    }
    let p = *r.get(if keyboard { 2 } else { 3 })?;
    Some(if (1..=100).contains(&p) { State::Battery { percent: p, charging: false } } else { State::Unreachable })
}
fn query(d: &HidDev, len: u16) -> Option<Vec<u8>> {
    let mut cmd = vec![0; len as usize];
    cmd[1] = 0xf7;
    for attempt in 0..3 {
        if attempt != 0 {
            sleep(Duration::from_millis(100));
        }
        if !d.set_feature(&cmd) {
            return None;
        }
        sleep(Duration::from_millis(20));
        if let Some(r) = d.get_feature(0) {
            if valid_reply(&r) {
                return Some(r);
            }
        }
    }
    None
}
fn poll(ctx: &mut PollCtx) {
    let mut products = Vec::new();
    for c in ctx.colls {
        if RECEIVERS.contains(&((u32::from(c.vid) << 16) | u32::from(c.pid)))
            && !ctx.is_claimed(c.vid, c.pid)
            && !products.contains(&(c.vid, c.pid))
        {
            products.push((c.vid, c.pid));
        }
    }
    for (vid, pid) in products {
        for c in ctx.find(vid, pid).filter(|c| c.iface == 2 && c.page == 0xffff && c.usage == 2 && c.feat_len >= 7) {
            let Some(d) = HidDev::open_feature_only(c) else {
                continue;
            };
            let Some(r) = query(&d, c.feat_len) else {
                continue;
            };
            let product = hid::product(c);
            let generic = product.is_empty() || product.contains("Receiver") || product.contains("2.4G");
            for keyboard in [true, false] {
                let Some(state) = parse_slot(&r, keyboard) else {
                    continue;
                };
                let name = if vid == 0x3151 && pid == 0x402d && !keyboard {
                    "Attack Shark R3"
                } else if generic {
                    if keyboard {
                        "Wireless keyboard"
                    } else {
                        "Wireless mouse"
                    }
                } else {
                    &product
                };
                ctx.out.push(
                    Observation::new(
                        format!("rongyuan:{vid:04x}:{pid:04x}:{}", if keyboard { "kb" } else { "ms" }),
                        state,
                    )
                    .name(name)
                    .kind(if keyboard { Kind::Keyboard } else { Kind::Mouse })
                    .via("2.4 GHz receiver"),
                );
            }
            ctx.claim(vid, pid);
            break;
        }
    }
}
pub fn driver() -> Box<dyn Driver> {
    simple("Rongyuan", poll)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slots_and_empty_status() {
        let r = [0, 1, 80, 55, 0, 1, 0];
        assert!(valid_reply(&r));
        assert_eq!(parse_slot(&r, true), Some(State::Battery { percent: 80, charging: false }));
        assert_eq!(parse_slot(&r, false), None);
        assert!(!valid_reply(&[0; 7]));
        assert!(!valid_reply(&r[..6]));
        assert_eq!(parse_slot(&[0, 1, 0, 255, 0, 0, 1], false), Some(State::Unreachable));
        assert_eq!(parse_slot(&[], true), None);
    }
}
