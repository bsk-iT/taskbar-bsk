use super::{Driver, PollCtx};
use crate::device::{Kind, Observation, State};
use crate::hid::HidDev;
use std::time::{Duration, Instant};

// Sony HCI tunnel: report 02, vendor FC00, 96 C3 magic, address 41 to headset.
// Event checksum covers bytes 5..len; command checksum covers bytes 6..12.
#[derive(Debug, PartialEq)]
struct Event<'a> {
    id: u8,
    kind: u8,
    tid: u16,
    payload: &'a [u8],
}
fn parse_event(b: &[u8]) -> Option<Event<'_>> {
    if b.len() < 15 || b.first() != Some(&2) {
        return None;
    }
    let len = *b.get(1)? as usize;
    if !(12..=62).contains(&len) || b.len() < len + 2 {
        return None;
    }
    if b.get(2..4) != Some(&[4, 0xff]) || b.get(5..8) != Some(&[0, 0x96, 0xc3]) || b.get(8)? >> 4 != 1 {
        return None;
    }
    let sum = b.get(5..=len)?.iter().fold(0u8, |sum, &v| sum.wrapping_add(v));
    if Some(&sum) != b.get(len + 1) {
        return None;
    }
    Some(Event {
        id: *b.get(9)?,
        kind: *b.get(10)?,
        tid: u16::from_le_bytes([*b.get(11)?, *b.get(12)?]),
        payload: b.get(13..len + 1)?,
    })
}
fn request(tid: u16) -> [u8; 64] {
    let mut r = [0; 64];
    r[..13].copy_from_slice(&[2, 12, 1, 0, 0xfc, 8, 0x96, 0xc3, 0x41, 4, 1, tid as u8, (tid >> 8) as u8]);
    r[13] = r[6..13].iter().fold(0u8, |s, &v| s.wrapping_add(v));
    r
}
fn lowest_bud_out(p: &[u8]) -> Option<u8> {
    [1, 3].iter().filter_map(|&i| p.get(i).copied()).filter(|&v| v <= 100).min()
}
fn battery_state(p: &[u8], buds: bool) -> Option<State> {
    if buds {
        if p.len() < 6 {
            return None;
        }
        Some(lowest_bud_out(p).map_or(State::Off, |percent| State::Battery { percent, charging: false }))
    } else {
        let [charging, percent, ..] = p else {
            return None;
        };
        Some(match *percent {
            0xff => State::Off,
            0..=100 => State::Battery { percent: *percent, charging: *charging != 0 },
            _ => State::Unreachable,
        })
    }
}
enum Reply {
    Ok(Vec<u8>),
    Silent,
    Failed,
}
struct SonyInzone {
    tid: u16,
}
pub fn driver() -> Box<dyn Driver> {
    Box::new(SonyInzone { tid: 1 })
}
impl SonyInzone {
    fn get_battery(&mut self, d: &HidDev, min_payload: usize) -> Reply {
        self.tid = self.tid.wrapping_add(1);
        if self.tid <= 1 {
            self.tid = 2;
        } // Transaction 1 belongs to unsolicited dongle notifications.
        d.flush();
        if !d.write(&request(self.tid), 500) {
            return Reply::Failed;
        }
        let end = Instant::now() + Duration::from_millis(1500);
        while let Some(left) = end.checked_duration_since(Instant::now()) {
            let Some(r) = d.read(left.as_millis() as u32) else {
                return Reply::Silent;
            };
            if r.len() < 15 {
                return Reply::Silent;
            }
            let Some(e) = parse_event(&r) else {
                continue;
            };
            if e.id == 4 && e.tid == self.tid && e.payload.len() >= min_payload && matches!(e.kind, 0x10 | 0xa0) {
                return Reply::Ok(e.payload.to_vec());
            }
        }
        Reply::Silent
    }
}
impl Driver for SonyInzone {
    fn name(&self) -> &'static str {
        "sony_inzone"
    }
    fn poll(&mut self, ctx: &mut PollCtx) {
        for (pid, name, buds, min_payload) in [
            (0x0fa8, "Sony INZONE H9 II", false, 2),
            (0x0ebf, "Sony INZONE H5", false, 2),
            (0x0ec2, "Sony INZONE Buds", true, 6),
        ] {
            if ctx.is_claimed(0x054c, pid) {
                continue;
            }
            let status = |state| {
                Observation::new(format!("sony_inzone:054c:{pid:04x}"), state)
                    .name(name)
                    .kind(Kind::Headset)
                    .via("2.4 GHz receiver")
            };
            let mut added = false;
            let mut no_reply = false;
            for c in ctx.find(0x054c, pid).filter(|c| c.page == 0xff04 && c.out_len >= 64) {
                let Some(d) = HidDev::open(c) else {
                    continue;
                };
                match self.get_battery(&d, min_payload) {
                    Reply::Silent => no_reply = true,
                    Reply::Failed => (),
                    Reply::Ok(p) => {
                        if let Some(state) = battery_state(&p, buds) {
                            ctx.add(status(state), 0x054c, pid);
                            added = true;
                            break;
                        }
                    }
                }
            }
            // Both buds docked: the dongle accepts the query but never sends a reply.
            if buds && no_reply && !added {
                ctx.add(status(State::Off), 0x054c, pid);
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn event_checksums_and_payload() {
        let mut r = [2, 14, 4, 0xff, 0, 0, 0x96, 0xc3, 0x14, 4, 0x10, 2, 0, 1, 73, 0];
        r[15] = r[5..15].iter().fold(0u8, |s, &v| s.wrapping_add(v));
        assert_eq!(parse_event(&r), Some(Event { id: 4, kind: 0x10, tid: 2, payload: &[1, 73] }));
        r[15] ^= 1;
        assert_eq!(parse_event(&r), None);
        assert_eq!(parse_event(&r[..14]), None);
        assert_eq!(&request(2)[..14], &[2, 12, 1, 0, 0xfc, 8, 0x96, 0xc3, 0x41, 4, 1, 2, 0, 0xa1]);
    }
    #[test]
    fn battery_states() {
        assert_eq!(battery_state(&[1, 73], false), Some(State::Battery { percent: 73, charging: true }));
        assert_eq!(battery_state(&[0, 0xff], false), Some(State::Off));
        assert_eq!(battery_state(&[0, 101], false), Some(State::Unreachable));
        assert_eq!(battery_state(&[0, 80, 0, 75, 0, 90], true), Some(State::Battery { percent: 75, charging: false }));
        assert_eq!(battery_state(&[0, 0xff, 0, 0xff, 0, 90], true), Some(State::Off));
        assert_eq!(battery_state(&[], true), None);
    }
}
