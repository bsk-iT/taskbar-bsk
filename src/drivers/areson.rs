use super::{simple, Driver, PollCtx};
use crate::{
    device::{Kind, Observation},
    hid::{HidColl, HidDev},
};
use std::{thread::sleep, time::Duration};

// Redragon M913/M915 GetPower on separate feature/input collections.
// FA07/FA08 use ten steps; FA7B/FA7C use whole percentages and belong to Fantech.
fn parse_power(r: &[u8]) -> Option<(u8, bool)> {
    if r.len() < 17 || r.get(..3) != Some(&[9, 4, 0]) || r.iter().fold(0u8, |s, b| s.wrapping_add(*b)) != 0x55 {
        return None;
    }
    let p = *r.get(6)?;
    (p <= 10).then_some(((u16::from(p) * 10) as u8, *r.get(7)? != 0))
}
fn probe(command: &HidColl, input: &HidColl) -> Option<(u8, bool)> {
    let cmd = HidDev::open_feature_only(command)?;
    let reply = HidDev::open(input)?;
    let mut req = [0; 17];
    req[0] = 8;
    req[1] = 4;
    req[16] = 0x49;
    for attempt in 0..2 {
        if attempt != 0 {
            sleep(Duration::from_millis(200));
        }
        reply.flush();
        if !cmd.set_feature(&req) {
            return None;
        }
        if let Some(r) = reply.read(1800) {
            if let Some(p) = parse_power(&r) {
                return Some(p);
            }
        }
    }
    None
}
fn poll(ctx: &mut PollCtx) {
    for (vid, pid) in ctx.products(0x25a7) {
        if !matches!(pid, 0xfa07 | 0xfa08) {
            continue;
        }
        let mut done = false;
        for c in ctx.find(vid, pid).filter(|c| c.iface == 1 && c.page == 0xff02 && c.usage == 2 && c.feat_len >= 17) {
            for i in ctx.find(vid, pid).filter(|c| c.iface == 1 && c.page == 0xff01 && c.usage == 0 && c.in_len >= 17) {
                if let Some((p, ch)) = probe(c, i) {
                    ctx.add(
                        Observation::battery(
                            format!("areson:{vid:04x}:{pid:04x}"),
                            "Redragon wireless mouse",
                            Kind::Mouse,
                            p,
                            ch,
                        )
                        .via("2.4 GHz receiver")
                        .grace(Duration::from_secs(1800)),
                        vid,
                        pid,
                    );
                    done = true;
                    break;
                }
            }
            if done {
                break;
            }
        }
    }
}
pub fn driver() -> Box<dyn Driver> {
    simple("Areson", poll)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn captured_ten_step_power() {
        let mut r = [9, 4, 0, 0, 0, 2, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x3e];
        assert_eq!(parse_power(&r), Some((80, false)));
        for n in 0..17 {
            assert_eq!(parse_power(&r[..n]), None);
        }
        r[7] = 2;
        r[16] = 0x3c;
        assert_eq!(parse_power(&r), Some((80, true)));
        r[6] = 11;
        r[16] = 0x39;
        assert_eq!(parse_power(&r), None);
        r[6] = 10;
        r[16] = 0;
        assert_eq!(parse_power(&r), None);
    }
}
