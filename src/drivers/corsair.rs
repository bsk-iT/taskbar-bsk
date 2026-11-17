//! Void C9 64 and read-only Bragi GET properties, without software-mode changes.
use super::{simple, Driver, PollCtx};
use crate::{
    device::{Kind, Observation, State},
    hid::{self, HidColl, HidDev},
};
use std::time::{Duration, Instant};
const VOID: &[u16] = &[
    0x1b1c, 0x1b27, 0x0a14, 0x0a16, 0x0a17, 0x0a1d, 0x0a1a, 0x1b2a, 0x1b23, 0x1b29, 0x0a55, 0x0a51, 0x0a52, 0x0a38,
    0x0a4f, 0x0a2b, 0x0a75, 0x0a56,
];
const BRAGI: &[u16] = &[0x2a08, 0x2a02, 0x0a97, 0x0a64, 0x0a62, 0x0a3e, 0x0a3d, 0x2b00];
fn parse_void(r: &[u8]) -> Option<State> {
    r.get(..5)?;
    let st = *r.get(4)?;
    Some(match st {
        1 | 2 | 4 | 5 => {
            State::Battery { percent: hid::clamp100(i32::from(*r.get(2)? & 0x7f)), charging: st == 4 || st == 5 }
        }
        0 => State::Off,
        _ => State::Unreachable,
    })
}
// Plain framing omits report ID 02 from replies. Unrelated notifications are ignored.
fn parse_bragi(r: &[u8], plain: bool, target: u8) -> Option<Result<u32, ()>> {
    let r = r.get((if plain { 0 } else { 1 })..)?;
    r.get(..7)?;
    if r.first().copied()? != if target == 8 { 0 } else { 1 } || r.get(1) != Some(&2) {
        return None;
    }
    if r.get(2) != Some(&0) {
        return Some(Err(()));
    }
    let bytes: [u8; 4] = r.get(3..7)?.try_into().ok()?;
    Some(Ok(u32::from_le_bytes(bytes)))
}
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Unknown,
    Prefixed,
    Plain,
}
struct Bragi {
    dev: HidDev,
    numbered: bool,
    mode: Mode,
}
impl Bragi {
    fn open(c: &HidColl) -> Option<Self> {
        Some(Self { dev: HidDev::open(c)?, numbered: c.out_len == 64, mode: Mode::Unknown })
    }
    fn get(&mut self, target: u8, prop: u8) -> Option<u32> {
        if self.mode != Mode::Plain {
            if let Some(value) = self.get_framed(false, target, prop) {
                self.mode = Mode::Prefixed;
                return Some(value);
            }
        }
        if self.mode == Mode::Prefixed {
            return None;
        }
        let value = self.get_framed(true, target, prop)?;
        self.mode = Mode::Plain;
        Some(value)
    }
    fn get_framed(&self, plain: bool, target: u8, prop: u8) -> Option<u32> {
        let req = if plain {
            vec![0, target, 2, prop]
        } else if self.numbered {
            vec![2, target, 2, prop]
        } else {
            vec![0, 2, target, 2, prop]
        };
        self.dev.flush();
        if !self.dev.write(&req, 500) {
            return None;
        }
        let end = Instant::now() + Duration::from_millis(300);
        loop {
            let left = end.saturating_duration_since(Instant::now()).as_millis() as u32;
            if left == 0 {
                return None;
            }
            let r = self.dev.read(left)?;
            if let Some(value) = parse_bragi(&r, plain, target) {
                return value.ok();
            }
        }
    }
}
pub fn driver() -> Box<dyn Driver> {
    simple("corsair", poll)
}
fn poll(ctx: &mut PollCtx) {
    for (vid, pid) in ctx.products(0x1b1c) {
        let colls = ctx.colls;
        if VOID.contains(&pid) {
            for c in colls
                .iter()
                .filter(|c| c.vid == vid && c.pid == pid && [0xffc5, 0xff00].contains(&c.page) && c.out_len > 0)
            {
                let Some(d) = HidDev::open(c) else { continue };
                d.flush();
                if !d.write(&[0xc9, 0x64], 500) {
                    continue;
                }
                let Some(state) = d.read(1000).and_then(|r| parse_void(&r)) else { continue };
                let product = hid::product(c);
                ctx.add(
                    Observation::new(format!("corsair:{vid:04x}:{pid:04x}"), state)
                        .name(if product.is_empty() { "Corsair wireless headset".into() } else { product })
                        .kind(Kind::Headset)
                        .via("2.4 GHz receiver"),
                    vid,
                    pid,
                );
                break;
            }
            continue;
        }
        let known = BRAGI.contains(&pid);
        for c in colls.iter().filter(|c| {
            c.vid == vid
                && c.pid == pid
                && [64, 65].contains(&c.out_len)
                && [64, 65].contains(&c.in_len)
                && (c.page == 0xff42 || known && (c.page >= 0xff00 || c.iface == 4))
        }) {
            let Some(mut b) = Bragi::open(c) else { continue };
            let mut any = false;
            let product = hid::product(c);
            for target in [9, 10, 11, 8] {
                if target == 8 && any {
                    break;
                }
                let mut level = b.get(target, 0x0f);
                for _ in 0..2 {
                    if !matches!(level, Some(0) | Some(1001..)) {
                        break;
                    }
                    level = b.get(target, 0x0f);
                }
                let Some(level) = level.filter(|v| *v <= 1000) else { continue };
                let mut name = if product.is_empty() { "Corsair wireless device".to_owned() } else { product.clone() };
                let mut kind = Kind::from_name(&name);
                if target != 8 {
                    if let Some(device_pid) = b.get(target, 0x12) {
                        if device_pid == 0x1b7e {
                            name = "Corsair Dark Core RGB Pro SE".into();
                            kind = Kind::Mouse;
                        } else if device_pid >> 8 == 0x0a {
                            kind = Kind::Headset;
                            if product.contains("Receiver") {
                                name = "Corsair wireless headset".into();
                            }
                        }
                    }
                }
                if kind == Kind::Other && pid >> 8 == 0x0a {
                    kind = Kind::Headset;
                }
                if kind == Kind::Other {
                    kind = hid::kind_from_collections(colls, vid, pid);
                }
                let charging = matches!(b.get(target, 0x10), Some(1 | 3));
                ctx.out.push(
                    Observation::battery(
                        format!("corsair:{vid:04x}:{pid:04x}:{target}"),
                        name,
                        kind,
                        ((level + 5) / 10).min(100) as u8,
                        charging,
                    )
                    .via(if target == 8 { "USB / Bluetooth" } else { "Corsair receiver" }),
                );
                any = true;
            }
            if any {
                ctx.claim(vid, pid);
                break;
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn void_status() {
        for (st, state) in [
            (0, State::Off),
            (1, State::Battery { percent: 70, charging: false }),
            (4, State::Battery { percent: 70, charging: true }),
            (3, State::Unreachable),
        ] {
            assert_eq!(parse_void(&[0, 0, 0xc6, 0, st]), Some(state));
        }
        assert!(parse_void(&[0; 4]).is_none());
    }
    #[test]
    fn bragi_framings_and_errors() {
        assert_eq!(parse_bragi(&[2, 1, 2, 0, 0xe8, 3, 0, 0], false, 9), Some(Ok(1000)));
        assert_eq!(parse_bragi(&[0, 2, 0, 55, 0, 0, 0], true, 8), Some(Ok(55)));
        assert_eq!(parse_bragi(&[1, 2, 5, 0, 0, 0, 0], true, 9), Some(Err(())));
        assert_eq!(parse_bragi(&[0, 2, 0, 55, 0, 0, 0], true, 9), None);
        assert_eq!(parse_bragi(&[2, 1, 2, 0], false, 9), None);
    }
}
