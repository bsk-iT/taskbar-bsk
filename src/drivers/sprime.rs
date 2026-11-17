// Sprime web driver: report 05 command 15, [9]=percent [10]=charging [11]=full [12]=online.
// AC11/AC12/AC13 cables and AC1C/AD8C receivers only; never probe bootloader AC0B.
use super::{simple, Driver, PollCtx};
use crate::{
    device::{Kind, Observation, State},
    hid::HidDev,
};
use std::{thread::sleep, time::Duration};
// PM1 report 05 command 15 shares Sora's frame, but byte 11 means fully charged.
fn parse_reply(r: &[u8], cable: bool) -> Option<State> {
    if r.len() < 13 || r.first() != Some(&5) || r.get(1) != Some(&0x15) {
        return None;
    }
    let pct = *r.get(9)?;
    if pct > 100 {
        return None;
    }
    let full = *r.get(11)? != 0;
    Some(if cable || *r.get(12)? != 0 {
        State::Battery { percent: if full { 100 } else { pct }, charging: *r.get(10)? != 0 && !full }
    } else {
        State::Off
    })
}
pub fn driver() -> Box<dyn Driver> {
    simple("sprime", poll)
}
fn poll(ctx: &mut PollCtx) {
    for (vid, pid) in ctx.products(0x1915) {
        let cable = matches!(pid, 0xac11..=0xac13);
        if !cable && !matches!(pid, 0xac1c | 0xad8c) {
            continue;
        }
        let colls = ctx
            .colls
            .iter()
            .filter(|c| c.vid == vid && c.pid == pid && c.page == 0xffa0 && c.usage == 1 && c.feat_len >= 32);
        for c in colls {
            let Some(d) = HidDev::open_feature_only(c) else { continue };
            let mut req = [0; 32];
            req[0] = 5;
            req[1] = 0x15;
            req[4] = 1;
            let mut state = None;
            for attempt in 0..4 {
                if attempt > 0 {
                    sleep(Duration::from_millis(90));
                }
                if !d.set_feature(&req) {
                    break;
                }
                sleep(Duration::from_millis(15));
                if let Some(parsed) = d.get_feature(5).and_then(|r| parse_reply(&r, cable)) {
                    state = Some(parsed);
                    if parsed != State::Off {
                        break;
                    }
                }
            }
            if let Some(state) = state {
                ctx.add(
                    Observation::new(format!("sprime:{vid:04x}:{pid:04x}"), state)
                        .name("Sprime PM1")
                        .kind(Kind::Mouse)
                        .via(if cable { "USB cable" } else { "2.4 GHz receiver" }),
                    vid,
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
    fn pm1_status() {
        let mut r = [0; 32];
        r[0] = 5;
        r[1] = 0x15;
        r[9] = 50;
        r[10] = 1;
        r[12] = 1;
        assert_eq!(parse_reply(&r, false), Some(State::Battery { percent: 50, charging: true }));
        r[11] = 1;
        assert_eq!(parse_reply(&r, false), Some(State::Battery { percent: 100, charging: false }));
        r[12] = 0;
        assert_eq!(parse_reply(&r, false), Some(State::Off));
        assert_eq!(parse_reply(&r, true), Some(State::Battery { percent: 100, charging: false }));
        r[9] = 101;
        assert_eq!(parse_reply(&r, true), None);
        assert_eq!(parse_reply(&r[..12], false), None);
    }
}
