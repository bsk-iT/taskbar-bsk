use super::{simple, Driver, PollCtx};
use crate::{
    device::{Kind, Observation, State},
    hid::HidDev,
};
use std::{
    thread::sleep,
    time::{Duration, Instant},
};

// RK web driver's QiWang remote system read: unencrypted output report 0.
#[derive(Debug, PartialEq)]
enum Reply {
    Asleep,
    Online { percent: u8, mv: u16, plugged: bool },
}
fn parse_battery(r: &[u8]) -> Option<Reply> {
    if r.len() < 11 || (*r.first()? & 0x1f) != 1 || (*r.get(1)? & 0x0f) != 1 || r.get(2) != Some(&2) {
        return None;
    }
    if (*r.get(3)? >> 4) & 1 == 0 {
        return Some(Reply::Asleep);
    }
    let percent = u16::from_le_bytes([*r.get(6)?, *r.get(7)?]);
    if percent > 100 {
        return None;
    }
    Some(Reply::Online {
        percent: percent as u8,
        mv: u16::from_le_bytes([*r.get(4)?, *r.get(5)?]),
        plugged: *r.get(10)? != 0,
    })
}
fn cable(pid: u16) -> bool {
    matches!(pid, 0x00a5 | 0x00e5)
}
fn poll_mg6(ctx: &mut PollCtx, vid: u16, pid: u16) {
    let page = if cable(pid) { 0xfff2 } else { 0xff00 };
    for c in ctx.find(vid, pid).filter(|c| c.page == page && c.usage == 1 && c.out_len == 65 && c.in_len == 65) {
        let Some(d) = HidDev::open(c) else {
            continue;
        };
        let mut answered = false;
        let mut online = None;
        for attempt in 0..3 {
            if online.is_some() {
                break;
            }
            if attempt != 0 {
                sleep(Duration::from_millis(100));
            }
            d.flush();
            if !d.write(&[0, 1, 1, 2, 1], 500) {
                break;
            }
            let end = Instant::now() + Duration::from_millis(600);
            loop {
                let left = end.saturating_duration_since(Instant::now()).as_millis() as u32;
                if left == 0 {
                    break;
                }
                let Some(r) = d.read(left) else {
                    break;
                };
                if r.len() < 11 {
                    break;
                }
                if (*r.first().unwrap_or(&0) & 0x1f) != 1
                    || (r.get(1).copied().unwrap_or(0) & 15) != 1
                    || r.get(2) != Some(&2)
                {
                    continue;
                }
                answered = true;
                if let Some(Reply::Online { percent, mv, plugged }) = parse_battery(&r) {
                    online = Some((percent, mv, plugged));
                }
                break;
            }
        }
        if !answered {
            continue;
        }
        let mut obs = Observation::new(format!("rk:{vid:04x}:{pid:04x}"), State::Unreachable)
            .name("RK MG6 Pro")
            .kind(Kind::Mouse)
            .via(if cable(pid) { "USB cable" } else { "2.4 GHz receiver" });
        if let Some((p, mv, plugged)) = online {
            obs.state = State::Battery { percent: p, charging: plugged || cable(pid) };
            if (3000..=4500).contains(&mv) {
                obs.millivolts = Some(mv);
            }
        }
        ctx.add(obs, vid, pid);
        return;
    }
}
fn poll(ctx: &mut PollCtx) {
    for (vid, pid) in ctx.products(0x2e3c) {
        if matches!(pid, 0x00a5 | 0x00a6 | 0x00e5 | 0x00e6) {
            poll_mg6(ctx, vid, pid);
        }
    }
}
pub fn driver() -> Box<dyn Driver> {
    simple("Royal Kludge", poll)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn status_and_sleep() {
        let mut r = [1, 1, 2, 0x10, 0x3c, 0x0f, 75, 0, 0, 0, 1];
        assert_eq!(parse_battery(&r), Some(Reply::Online { percent: 75, mv: 3900, plugged: true }));
        for n in 0..11 {
            assert_eq!(parse_battery(&r[..n]), None);
        }
        r[3] = 0;
        assert_eq!(parse_battery(&r), Some(Reply::Asleep));
        r[3] = 0x10;
        r[6] = 101;
        assert_eq!(parse_battery(&r), None);
        r[6] = 0;
        assert!(matches!(parse_battery(&r), Some(Reply::Online { percent: 0, .. })));
    }
}
