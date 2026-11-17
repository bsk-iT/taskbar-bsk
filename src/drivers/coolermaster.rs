//! MM712/MM731 share dongle 01b7; model 50/70 distinguishes them.
//! Status 47 81: [10] percent, [11] charging, [12] cable/radio.
use super::{simple, Driver, PollCtx};
use crate::{
    device::{Kind, Observation, State},
    hid::HidDev,
};
use std::time::{Duration, Instant};

pub fn driver() -> Box<dyn Driver> {
    simple("coolermaster", poll)
}

fn parse_status(pid: u16, r: &[u8]) -> Option<Observation> {
    let r = r.get(..13)?;
    if r.get(..2)? != [0x47, 0x81] {
        return None;
    }
    let model = *r.get(2)?;
    let name = match (pid, model) {
        (0x0169, _) | (0x01b7, 0x50) => "Cooler Master MM712",
        (0x0167, _) | (0x01b7, 0x70) => "Cooler Master MM731",
        _ => "Cooler Master wireless mouse",
    };
    let percent = *r.get(10)?;
    let state = if (1..=100).contains(&percent) {
        State::Battery { percent, charging: *r.get(11)? != 0 }
    } else {
        State::Unreachable
    };
    Some(
        Observation::new(format!("coolermaster:2516:{pid:04x}"), state)
            .name(name)
            .kind(Kind::Mouse)
            .via(if *r.get(12)? == 0 { "USB cable" } else { "2.4 GHz receiver" })
            .grace(Duration::from_secs(1800)),
    )
}

fn poll(ctx: &mut PollCtx) {
    for (vid, pid) in ctx.products(0x2516) {
        if ![0x01b7, 0x0169, 0x0167].contains(&pid) {
            continue;
        }
        let colls = ctx.colls;
        for c in colls.iter().filter(|c| c.vid == vid && c.pid == pid && c.iface == 3 && c.page == 0xff0a) {
            let Some(d) = HidDev::open(c) else { continue };
            d.flush();
            if !d.write(&[0, 0x46, 0x81, 0], 500) {
                continue;
            }
            let end = Instant::now() + Duration::from_millis(500);
            let status = loop {
                let left = end.saturating_duration_since(Instant::now()).as_millis() as u32;
                if left == 0 {
                    break None;
                }
                let Some(r) = d.read(left) else { break None };
                if r.len() < 13 {
                    break None;
                }
                if let Some(s) = parse_status(pid, &r) {
                    break Some(s);
                }
            };
            let Some(s) = status else { continue };
            if let Some(old) = ctx.out.iter_mut().find(|old| old.name == s.name) {
                if let (State::Battery { charging, .. }, State::Battery { charging: new, .. }) =
                    (&mut old.state, s.state)
                {
                    *charging |= new;
                }
                ctx.claim(vid, pid);
            } else {
                ctx.add(s, vid, pid);
            }
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn status_bytes() {
        let mut r = [0x47, 0x81, 0x50, 0, 0, 0, 0, 0, 0, 0, 70, 1, 1];
        let s = parse_status(0x01b7, &r).unwrap();
        assert_eq!(s.name.as_deref(), Some("Cooler Master MM712"));
        assert_eq!(s.state, State::Battery { percent: 70, charging: true });
        r[10] = 0;
        assert_eq!(parse_status(0x01b7, &r).unwrap().state, State::Unreachable);
        assert!(parse_status(0x01b7, &r[..12]).is_none());
        r[0] = 0;
        assert!(parse_status(0x01b7, &r).is_none());
    }
}
