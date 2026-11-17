use super::{simple, Driver, PollCtx};
use crate::device::{Kind, Observation, State};
use crate::hid::HidDev;
use std::time::{Duration, Instant};

// Adjust Pro: report 02, device type 11 (dongle) / F1 (headset), GET_DEVICE_STATE 21.
// Offsets include report ID; byte 23 is 1 on the USB cable. The stand unlinks the headset.
fn parse_state(r: &[u8], device_type: u8) -> Option<&[u8]> {
    (r.len() >= 24 && r.get(..3) == Some(&[2, device_type, 0x21])).then_some(r)
}

fn get_state(d: &HidDev, device_type: u8) -> Option<Vec<u8>> {
    d.flush();
    if !d.write(&[2, device_type, 0x21], 500) {
        return None;
    }
    let end = Instant::now() + Duration::from_millis(1000);
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        let r = d.read(left.as_millis() as u32)?;
        if parse_state(&r, device_type).is_some() {
            return Some(r);
        }
    }
    None
}

pub fn driver() -> Box<dyn Driver> {
    simple("fractal", poll)
}

fn poll(ctx: &mut PollCtx) {
    for (vid, pid) in ctx.products(0x36bc) {
        if !matches!(pid, 0x0001 | 0x2001) {
            continue;
        }
        let cable = pid == 0x2001;
        for c in ctx.find(vid, pid).filter(|c| c.page == 0xff00 && c.usage == 1 && c.out_len >= 4 && c.in_len >= 24) {
            let Some(d) = HidDev::open(c) else {
                continue;
            };
            let id = "fractal:36bc:0001:scape";
            let via = if cable { "USB cable" } else { "2.4 GHz dongle" };
            if !cable {
                let Some(r) = get_state(&d, 0x11) else {
                    continue;
                };
                if r.get(4) != Some(&1) {
                    ctx.add(
                        Observation::new(id, State::Off).name("Fractal Scape").kind(Kind::Headset).via(via),
                        vid,
                        pid,
                    );
                    break;
                }
            }
            let Some(r) = get_state(&d, 0xf1) else {
                continue;
            };
            let Some(&percent) = r.get(15).filter(|&&p| p <= 100) else {
                continue;
            };
            let s = Observation::battery(id, "Fractal Scape", Kind::Headset, percent, r.get(23) == Some(&1)).via(via);
            ctx.add(s, vid, pid);
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn state_framing() {
        let mut r = [0; 24];
        r[..3].copy_from_slice(&[2, 0xf1, 0x21]);
        r[15] = 73;
        r[23] = 1;
        assert_eq!(parse_state(&r, 0xf1).and_then(|r| r.get(15)), Some(&73));
        assert!(parse_state(&r[..23], 0xf1).is_none());
        assert!(parse_state(&r, 0x11).is_none());
    }
}
