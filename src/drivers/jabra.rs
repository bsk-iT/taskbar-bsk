use super::{Driver, PollCtx};
use crate::device::{Kind, Observation, State};
use crate::hid::HidDev;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

// GNP report 05: dst/src/sequence/C0|length/class/op/payload. FE is NAK.
// Address 04 is the headset behind Link 380/390; battery 12/02, UTF-8 name 02/00.
const PIDS: &[u16] = &[0x24c7, 0x24c8, 0x24c9, 0x24ca, 0x24e9, 0x2e50, 0x2e51, 0x2e56, 0x2e57];
#[derive(Debug, PartialEq)]
enum Reply<'a> {
    Ok(&'a [u8]),
    Nak,
    Skip,
}
fn parse_reply(r: &[u8], dst: u8, seq: u8, cls: u8, op: u8) -> Reply<'_> {
    if r.len() < 6 || r.get(..4) != Some(&[5, 0, dst, seq]) || !r.get(4).is_some_and(|b| b & 0xc0 == 0xc0) {
        return Reply::Skip;
    }
    if r.get(5) == Some(&0xfe) {
        return Reply::Nak;
    }
    let len = r.get(4).copied().unwrap_or(0) as usize & 0x3f;
    if len < 6 || r.get(5..7) != Some(&[cls, op]) {
        return Reply::Skip;
    }
    match r.get(7..len + 1) {
        Some(p) => Reply::Ok(p),
        None => Reply::Skip,
    }
}
fn parse_name(p: &[u8]) -> Option<String> {
    let n = *p.first()? as usize;
    if n == 0 {
        return None;
    }
    // MultiByteToWideChar(CP_UTF8, 0) replaces malformed UTF-8 instead of rejecting it.
    Some(String::from_utf8_lossy(p.get(1..n + 1)?).into_owned())
}
fn parse_battery(p: &[u8]) -> Option<State> {
    let [flags, level, ..] = p else {
        return None;
    };
    Some(if *level <= 100 { State::Battery { percent: *level, charging: flags & 1 != 0 } } else { State::Unreachable })
}
#[derive(Default)]
struct Jabra {
    seq: u8,
    names: BTreeMap<u16, String>,
}
pub fn driver() -> Box<dyn Driver> {
    Box::new(Jabra { seq: 0x20, ..Jabra::default() })
}
impl Jabra {
    fn query(&mut self, d: &HidDev, cls: u8, op: u8, timeout: u64) -> Option<Result<Vec<u8>, ()>> {
        let seq = self.seq;
        self.seq = self.seq.wrapping_add(1);
        d.flush();
        if !d.write(&[5, 4, 0, seq, 0x46, cls, op], 300) {
            return None;
        }
        let end = Instant::now() + Duration::from_millis(timeout);
        while let Some(left) = end.checked_duration_since(Instant::now()) {
            let r = d.read(left.as_millis() as u32)?;
            match parse_reply(&r, 4, seq, cls, op) {
                Reply::Ok(p) => return Some(Ok(p.to_vec())),
                Reply::Nak => return Some(Err(())),
                Reply::Skip => (),
            }
        }
        None
    }
}
impl Driver for Jabra {
    fn name(&self) -> &'static str {
        "jabra"
    }
    fn poll(&mut self, ctx: &mut PollCtx) {
        for &pid in PIDS {
            if ctx.is_claimed(0x0b0e, pid) {
                continue;
            }
            for c in ctx.find(0x0b0e, pid).filter(|c| c.page == 0xff00 && c.out_len >= 64 && c.in_len >= 64) {
                let Some(d) = HidDev::open(c) else {
                    continue;
                };
                let Some(reply) = self.query(&d, 0x12, 2, 700) else {
                    continue;
                };
                let (state, name) = match reply {
                    Ok(p) => {
                        let Some(state) = parse_battery(&p) else {
                            continue;
                        };
                        if !self.names.contains_key(&pid) {
                            let name = self
                                .query(&d, 2, 0, 400)
                                .and_then(Result::ok)
                                .and_then(|p| parse_name(&p))
                                .unwrap_or_else(|| "Jabra headset".into());
                            self.names.insert(pid, name);
                        }
                        (state, self.names.get(&pid).cloned().unwrap_or_else(|| "Jabra headset".into()))
                    }
                    Err(()) => (State::Off, self.names.remove(&pid).unwrap_or_else(|| "Jabra headset".into())),
                };
                ctx.add(
                    Observation::new(format!("jabra:0b0e:{pid:04x}"), state)
                        .name(name)
                        .kind(Kind::Headset)
                        .via("Jabra Link dongle"),
                    0x0b0e,
                    pid,
                );
                break;
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gnp_frames() {
        let r = [5, 0, 4, 0x20, 0xc8, 0x12, 2, 1, 87];
        assert_eq!(parse_reply(&r, 4, 0x20, 0x12, 2), Reply::Ok(&[1, 87]));
        assert_eq!(parse_reply(&r, 4, 0x21, 0x12, 2), Reply::Skip);
        assert_eq!(parse_reply(&r[..8], 4, 0x20, 0x12, 2), Reply::Skip);
        assert_eq!(parse_reply(&[5, 0, 4, 0x20, 0xc0, 0xfe], 4, 0x20, 0x12, 2), Reply::Nak);
        assert_eq!(parse_battery(&[0x81, 87]), Some(State::Battery { percent: 87, charging: true }));
        assert_eq!(parse_battery(&[0, 101]), Some(State::Unreachable));
        assert_eq!(parse_battery(&[]), None);
    }
    #[test]
    fn length_prefixed_name() {
        assert_eq!(parse_name(&[5, b'J', b'a', b'b', b'r', b'a']), Some("Jabra".into()));
        assert_eq!(parse_name(&[5, b'J']), None);
        assert_eq!(parse_name(&[0]), None);
    }
}
