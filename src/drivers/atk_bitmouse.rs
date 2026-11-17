// NearLink BITMOUSE on FF05:0001: report 08, 64 bytes, checksum sums bytes 2..63.
// Target 01, commands 07/0F read battery/charging; FF reply status means error.
// Sources: HaloBattery PR #350, qwr79/mouse-battery.
use super::{simple, Driver, PollCtx};
use crate::{
    device::{Kind, Observation},
    hid::HidDev,
};
use std::time::{Duration, Instant};

fn request(cmd: u8) -> [u8; 64] {
    let mut r = [0; 64];
    r[0] = 8;
    r[2] = 0x72;
    r[3] = 2;
    r[5] = 1;
    r[6] = cmd;
    r[7] = 1;
    r[1] = r.iter().skip(2).fold(0u8, |s, b| s.wrapping_add(*b));
    r
}
fn parse_reply(r: &[u8], cmd: u8) -> Option<Result<u8, ()>> {
    if r.len() < 8 || r.first() != Some(&8) || r.get(1) != Some(&0x72) || r.get(5) != Some(&cmd) {
        return None;
    }
    Some(if r.get(2) == Some(&0xff) { Err(()) } else { Ok(*r.get(7)?) })
}
fn query(d: &HidDev, cmd: u8) -> Option<u8> {
    d.flush();
    if !d.write(&request(cmd), 500) {
        return None;
    }
    let end = Instant::now() + Duration::from_millis(700);
    loop {
        let left = end.saturating_duration_since(Instant::now()).as_millis() as u32;
        if left == 0 {
            return None;
        }
        let r = d.read(left)?;
        if let Some(answer) = parse_reply(&r, cmd) {
            return answer.ok();
        }
    }
}
pub fn driver() -> Box<dyn Driver> {
    simple("atk_bitmouse", poll)
}
fn poll(ctx: &mut PollCtx) {
    if ctx.is_claimed(0x373b, 0x1216) {
        return;
    }
    let colls = ctx.colls.iter().filter(|c| {
        c.vid == 0x373b && c.pid == 0x1216 && c.page == 0xff05 && c.usage == 1 && c.out_len >= 64 && c.in_len >= 8
    });
    for c in colls {
        let Some(d) = HidDev::open(c) else { continue };
        let Some(level @ 1..=100) = query(&d, 7) else { continue };
        let charging = query(&d, 15) == Some(1);
        ctx.add(
            Observation::battery("atknk:373b:1216", "ATK / VXE NK mouse", Kind::Mouse, level, charging)
                .via("2.4 GHz receiver")
                .grace(Duration::from_secs(1800)),
            0x373b,
            0x1216,
        );
        break;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bitmouse_frames() {
        assert_eq!(&request(7)[..8], &[8, 0x7d, 0x72, 2, 0, 1, 7, 1]);
        assert_eq!(parse_reply(&[8, 0x72, 0, 0, 0, 7, 1, 80], 7), Some(Ok(80)));
        assert_eq!(parse_reply(&[8, 0x72, 255, 0, 0, 7, 1, 80], 7), Some(Err(())));
        assert_eq!(parse_reply(&[8, 0x72], 7), None);
    }
}
