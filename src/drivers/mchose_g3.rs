use super::{simple, Driver, PollCtx};
use crate::{
    device::{Kind, Observation},
    hid::{self, HidColl, HidDev},
};
use std::time::{Duration, Instant};

// Jstdi001/MCHOSE-Monitor: G3 V2 uses FF01:0010, not the other M HUB protocols.
fn parse_battery(r: &[u8]) -> Option<(u8, bool)> {
    if r.first() != Some(&0xaa) || r.get(1) != Some(&0x30) {
        return None;
    }
    let p = *r.get(8)?;
    (1..=100).contains(&p).then_some((p, *r.get(9)? == 1))
}
fn query(c: &HidColl) -> Option<(u8, bool)> {
    let d = HidDev::open(c)?;
    d.flush();
    if !d.write(&[0, 0x55, 0x30, 0xa5, 0x0b, 0x2e, 1, 1, 1, 0, 0], 500) {
        return None;
    }
    let end = Instant::now() + Duration::from_millis(1000);
    loop {
        let left = end.saturating_duration_since(Instant::now()).as_millis() as u32;
        if left == 0 {
            return None;
        }
        let r = d.read(left)?;
        if r.len() >= 10 && r.first() == Some(&0xaa) && r.get(1) == Some(&0x30) {
            return parse_battery(&r);
        }
    }
}
fn poll(ctx: &mut PollCtx) {
    let mut found = false;
    // A live cable reading takes precedence and reserves the receiver without querying it.
    for pid in [0x418c, 0x4245] {
        if ctx.is_claimed(0x3837, pid) {
            continue;
        }
        for c in
            ctx.find(0x3837, pid).filter(|c| c.page == 0xff01 && c.usage == 0x10 && c.out_len >= 65 && c.in_len >= 65)
        {
            if found {
                ctx.claim(0x3837, pid);
                break;
            }
            let Some((p, ch)) = query(c) else {
                continue;
            };
            let mut name = hid::product(c);
            if name.is_empty() {
                name = "MCHOSE G3 V2".into();
            }
            ctx.add(
                Observation::battery("mchose:3837:418c:g3v2", name, Kind::Mouse, p, ch)
                    .via(if pid == 0x418c { "USB cable" } else { "2.4 GHz receiver" })
                    .grace(Duration::from_secs(1800)),
                0x3837,
                pid,
            );
            found = true;
            break;
        }
    }
}
pub fn driver() -> Box<dyn Driver> {
    simple("Mchose G3", poll)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn battery_bounds() {
        let mut r = [0xaa, 0x30, 0, 0, 0, 0, 0, 0, 80, 1];
        assert_eq!(parse_battery(&r), Some((80, true)));
        for n in 0..10 {
            assert_eq!(parse_battery(&r[..n]), None);
        }
        for p in [0, 101, 255] {
            r[8] = p;
            assert_eq!(parse_battery(&r), None);
        }
        r[8] = 100;
        r[9] = 2;
        assert_eq!(parse_battery(&r), Some((100, false)));
    }
}
