//! Vader 5 Pro architecture/info queries only; SDL hidapi_flydigi layout.
use super::{simple, Driver, PollCtx};
use crate::{
    device::{Kind, Observation},
    hid::HidDev,
};
use std::time::{Duration, Instant};
pub fn driver() -> Box<dyn Driver> {
    simple("Flydigi", poll)
}
fn body(r: &[u8], command: u8) -> Option<&[u8]> {
    let r = if r.starts_with(&[4, 0x5a, 0xa5]) { r.get(1..)? } else { r };
    (r.len() >= 5 && r.starts_with(&[0x5a, 0xa5, command])).then_some(r)
}
fn reply(d: &HidDev, command: u8) -> Option<Vec<u8>> {
    let end = Instant::now() + Duration::from_millis(800);
    while Instant::now() < end {
        let remaining = end.saturating_duration_since(Instant::now()).as_millis() as u32;
        if remaining == 0 {
            return None;
        }
        let mut r = d.read(remaining)?;
        if body(&r, command).is_some() {
            if r.starts_with(&[4, 0x5a, 0xa5]) {
                r.remove(0);
            }
            return Some(r);
        }
    }
    None
}
fn parse(r: &[u8], modern: bool) -> Option<(u8, bool, bool)> {
    if r.len() < 12 || r.get(5) != Some(&130) {
        return None;
    }
    let value = *r.get(11)?;
    let level = value & 15;
    let state = value >> 4;
    let connection = *r.get(6)?;
    if state > 2 || level > 5 || if modern { connection > 1 } else { connection != 1 && connection != 2 } {
        return None;
    }
    Some((if state == 2 { 100 } else { level * 20 }, state == 1, connection == if modern { 1 } else { 2 }))
}
fn poll(ctx: &mut PollCtx) {
    if ctx.is_claimed(0x37d7, 0x2401) {
        return;
    }
    for c in ctx.colls.iter().filter(|c| {
        c.vid == 0x37d7 && c.pid == 0x2401 && c.page == 0xffa0 && c.usage == 1 && c.in_len >= 12 && c.out_len >= 6
    }) {
        let Some(d) = HidDev::open(c) else { continue };
        d.flush();
        if !d.write(&[0, 0x5a, 0xa5, 7, 0, 0], 500) {
            continue;
        }
        let modern = reply(&d, 7).is_some_and(|r| r.get(3) == Some(&1) && r.get(4) == Some(&0));
        if !d.write(&[0, 0x5a, 0xa5, 1, 2, 0], 500) {
            continue;
        }
        let Some(r) = reply(&d, 1) else { continue };
        let Some((p, ch, wireless)) = parse(&r, modern) else { continue };
        let path = String::from_utf16_lossy(c.path.strip_suffix(&[0]).unwrap_or(&c.path));
        ctx.add(
            Observation::battery(format!("flydigi:37d7:2401:{path}"), "Flydigi Vader 5 Pro", Kind::Gamepad, p, ch)
                .via(if wireless { "2.4 GHz receiver" } else { "USB cable" }),
            c.vid,
            c.pid,
        );
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn headers_and_levels() {
        assert_eq!(body(&[4, 0x5a, 0xa5, 7, 1, 0], 7), Some(&[0x5a, 0xa5, 7, 1, 0][..]));
        assert_eq!(body(&[0x5a, 0xa5, 1], 1), None);
        let mut r = [0; 12];
        r[5] = 130;
        r[6] = 1;
        r[11] = 0x13;
        assert_eq!(parse(&r, true), Some((60, true, true)));
        assert_eq!(parse(&r, false), Some((60, true, false)));
        r[11] = 0x25;
        assert_eq!(parse(&r, true), Some((100, false, true)));
        r[11] = 0x26;
        assert_eq!(parse(&r, true), None);
        assert_eq!(parse(&r[..11], true), None);
    }
}
