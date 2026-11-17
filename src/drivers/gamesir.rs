//! GameSir vendor battery queries: Nova FF7A and Cyclone 2 FFF0:0040.
use super::{simple, Driver, PollCtx};
use crate::{
    device::{Kind, Observation, State},
    hid::HidDev,
};
use std::time::{Duration, Instant};

pub fn driver() -> Box<dyn Driver> {
    simple("GameSir", poll)
}
pub fn cyclone_driver() -> Box<dyn Driver> {
    simple("GameSir Cyclone", poll_cyclone)
}

fn nova_reply(r: &[u8]) -> Option<Option<u8>> {
    if r.len() < 19 || r.first() != Some(&1) {
        return None;
    }
    match *r.get(18)? {
        0xff => Some(None),
        v if v <= 100 => Some(Some(v)),
        _ => None,
    }
}
fn cyclone_reply(r: &[u8]) -> Option<(u8, bool)> {
    if r.len() < 37 || r.first() != Some(&0x12) {
        return None;
    }
    let level = *r.get(36)?;
    (level <= 100).then_some((level, r.get(35) == Some(&1)))
}
fn poll(ctx: &mut PollCtx) {
    poll_model(ctx, false);
}
fn poll_cyclone(ctx: &mut PollCtx) {
    poll_model(ctx, true);
}
fn poll_model(ctx: &mut PollCtx, cyclone: bool) {
    for (vid, pid) in ctx.products(0x3537) {
        if if cyclone { pid != 0x100b } else { !matches!(pid, 0x1098 | 0x100f | 0x1040) } {
            continue;
        }
        for c in ctx.colls.iter().filter(|c| c.vid == vid && c.pid == pid) {
            if if cyclone {
                c.page != 0xfff0 || c.usage != 0x40 || c.out_len < 3 || c.in_len < 37
            } else {
                c.page != 0xff7a || c.out_len < 3
            } {
                continue;
            }
            let Some(d) = HidDev::open(c) else { continue };
            d.flush();
            let request: &[u8] = if cyclone { &[0x0f, 3] } else { &[0, 1, 1] };
            if !d.write(request, 500) {
                continue;
            }
            let end = Instant::now() + Duration::from_millis(if cyclone { 1500 } else { 800 });
            let mut result = None;
            while Instant::now() < end {
                let Some(r) = d.read(200) else { continue };
                if cyclone {
                    if let Some((p, ch)) = cyclone_reply(&r) {
                        result = Some((Some(p), ch));
                        break;
                    }
                } else if r.len() >= 19 && r.first() == Some(&1) {
                    result = nova_reply(&r).map(|p| (p, p.is_none()));
                    break;
                }
            }
            let Some((percent, charging)) = result else { continue };
            let id = format!("gamesir:{vid:04x}:{pid:04x}");
            let name = if cyclone { "GameSir Cyclone 2" } else { "GameSir Nova" };
            let obs = match percent {
                Some(p) => Observation::battery(id, name, Kind::Gamepad, p, charging),
                None => Observation::new(id, State::ChargingUnknown).name(name).kind(Kind::Gamepad),
            }
            .via(if cyclone {
                "2.4 GHz receiver or USB"
            } else if pid == 0x100f {
                "USB cable"
            } else {
                "2.4 GHz receiver"
            });
            ctx.add(obs, vid, pid);
            break;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nova_frames() {
        let mut r = [0; 19];
        r[0] = 1;
        r[18] = 67;
        assert_eq!(nova_reply(&r), Some(Some(67)));
        r[18] = 255;
        assert_eq!(nova_reply(&r), Some(None));
        r[18] = 101;
        assert_eq!(nova_reply(&r), None);
        r[0] = 0xf4;
        assert_eq!(nova_reply(&r), None);
        assert_eq!(nova_reply(&r[..18]), None);
    }
    #[test]
    fn cyclone_frames() {
        let mut r = [0; 37];
        r[0] = 0x12;
        r[35] = 1;
        r[36] = 42;
        assert_eq!(cyclone_reply(&r), Some((42, true)));
        r[0] = 0x10;
        assert_eq!(cyclone_reply(&r), None);
        r[0] = 0x12;
        r[36] = 101;
        assert_eq!(cyclone_reply(&r), None);
        assert_eq!(cyclone_reply(&r[..36]), None);
    }
}
