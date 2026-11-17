use super::{simple, Driver, PollCtx};
use crate::{
    device::{Kind, Observation, State},
    hid::{self, HidDev},
};
use std::{thread::sleep, time::Duration};

// M HUB: report 12 / command 06, every payload byte inverted, including padding.
// Explicit Realtek IDs exclude G3 V2 and keyboards that speak different protocols.
const REALTEK_PIDS: &[u16] = &[0x100c, 0x100b, 0x100a, 0x1001, 0x1012];
#[derive(Debug, PartialEq)]
enum Reply {
    Asleep,
    Online { percent: u8, charging: bool, flags: u8 },
}
fn parse_status(r: &[u8], vid: u16, alt: u16) -> Option<Reply> {
    if r.first() != Some(&0x12) || r.get(1) != Some(&0xf9) {
        return None;
    }
    let data = r.get(2..13)?;
    if data.iter().all(|b| *b == 0xff) {
        return Some(Reply::Asleep);
    }
    let field = u16::from_le_bytes([*data.first()? ^ 0xff, *data.get(1)? ^ 0xff]);
    if field != vid && field != alt {
        return None;
    }
    let percent = *data.get(9)? ^ 0xff;
    if percent > 100 {
        return None;
    }
    Some(Reply::Online { percent, charging: (*data.get(10)? ^ 0xff) != 0, flags: *data.get(8)? ^ 0xff })
}
fn poll_product(ctx: &mut PollCtx, vid: u16, pid: u16, field: u16, alt: u16) {
    for c in ctx.find(vid, pid).filter(|c| c.page == 0xff01 && c.usage == 1 && c.feat_len >= 65) {
        let Some(d) = HidDev::open_feature_only(c) else {
            continue;
        };
        let mut req = vec![0xff; c.feat_len as usize];
        req[0] = 0x12;
        req[1] = 0xf9;
        let mut state = None;
        for attempt in 0..5 {
            if matches!(state, Some(Reply::Online { .. })) {
                break;
            }
            if attempt != 0 {
                sleep(Duration::from_millis(100));
            }
            if !d.set_feature(&req) {
                break;
            }
            sleep(Duration::from_millis(120));
            if let Some(r) = d.get_feature(0x12) {
                if let Some(parsed) = parse_status(&r, field, alt) {
                    state = Some(parsed);
                }
            }
        }
        let Some(state) = state else {
            continue;
        };
        let mut name = hid::product(c);
        if name.is_empty() {
            name = "MCHOSE mouse".into();
        }
        let (state, via) = match state {
            Reply::Asleep => (State::Unreachable, "2.4 GHz receiver"),
            Reply::Online { percent, charging, flags } => (
                State::Battery { percent, charging },
                if flags == 0 && charging { "USB cable" } else { "2.4 GHz receiver" },
            ),
        };
        ctx.add(
            Observation::new(format!("mchose:{vid:04x}:{pid:04x}"), state).name(name).kind(Kind::Mouse).via(via),
            vid,
            pid,
        );
        break;
    }
}
fn poll(ctx: &mut PollCtx) {
    for (vid, pid) in ctx.products(0x5253) {
        poll_product(ctx, vid, pid, vid, vid);
    }
    for &pid in REALTEK_PIDS {
        if !ctx.is_claimed(0x3837, pid) {
            poll_product(ctx, 0x3837, pid, 0x41e4, 0x3837);
        }
    }
}
pub fn driver() -> Box<dyn Driver> {
    simple("Mchose", poll)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inverted_payload() {
        let mut r = [0xff; 13];
        r[0] = 0x12;
        r[1] = 0xf9;
        assert_eq!(parse_status(&r, 0x5253, 0x5253), Some(Reply::Asleep));
        for (i, b) in [0x53, 0x52, 0, 0, 0, 0, 0, 0, 9, 80, 1].into_iter().enumerate() {
            r[i + 2] = b ^ 0xff;
        }
        assert_eq!(parse_status(&r, 0x5253, 0x5253), Some(Reply::Online { percent: 80, charging: true, flags: 9 }));
        for n in 0..13 {
            assert_eq!(parse_status(&r[..n], 0x5253, 0x5253), None);
        }
        assert_eq!(parse_status(&r, 0x41e4, 0x3837), None);
        r[11] = 101 ^ 0xff;
        assert_eq!(parse_status(&r, 0x5253, 0x5253), None);
    }
}
