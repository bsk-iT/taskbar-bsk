// WLMouse FFFF report 0: status A1/A2 answers, A0 sleeps, [7]=charging [8]=percent.
// Sources: djnawara/mouse-gauge, Waturus/HaloBattery. A887/A888 use unverified framing.
use super::{simple, Driver, PollCtx};
use crate::{
    device::{Kind, Observation, State},
    hid::HidDev,
};
use std::{thread::sleep, time::Duration};
const MODELS: &[(u16, bool, &str, u16)] = &[
    (0xa879, false, "WLMouse Sword X 8K", 0xa878),
    (0xa884, false, "WLMouse Beast X 8K", 0xa883),
    (0xa860, false, "WLMouse mouse", 0),
    (0xa866, false, "WLMouse mouse", 0),
    (0xa867, false, "WLMouse mouse", 0),
    (0xa856, true, "WLMouse Beast X V2", 0),
    (0xa868, true, "WLMouse Beast X Mini Pro", 0),
    (0xa870, true, "WLMouse Beast X Pro", 0),
    (0xa878, true, "WLMouse Sword X 8K", 0),
    (0xa880, true, "WLMouse Beast MAX", 0),
    (0xa883, true, "WLMouse Beast X 8K", 0),
    (0xa885, true, "WLMouse Beast X Mini", 0),
];
#[derive(Debug, PartialEq)]
enum Reply {
    Battery(u8, bool),
    Asleep,
    Pending,
    Invalid,
}
fn parse_reply(r: &[u8]) -> Reply {
    if r.len() < 9 {
        return Reply::Invalid;
    }
    if r.get(3..5) != Some(&[2, 2][..]) || r.get(6) != Some(&0x83) {
        return Reply::Pending;
    }
    match (r.get(1), r.get(7), r.get(8)) {
        (Some(0xa0), _, _) => Reply::Asleep,
        (Some(0xa1 | 0xa2), Some(ch), Some(p)) if *p <= 100 => Reply::Battery(*p, *ch == 1),
        (Some(0xa1 | 0xa2), _, _) => Reply::Invalid,
        _ => Reply::Pending,
    }
}
fn query(d: &HidDev) -> Reply {
    let mut req = [0; 65];
    req[3] = 2;
    req[4] = 2;
    req[6] = 0x83;
    for _ in 0..2 {
        if !d.set_feature(&req) {
            return Reply::Invalid;
        }
        for _ in 0..6 {
            sleep(Duration::from_millis(60));
            let Some(r) = d.get_feature(0) else { return Reply::Invalid };
            match parse_reply(&r) {
                Reply::Pending => (),
                answer => return answer,
            }
        }
    }
    Reply::Invalid
}
pub fn driver() -> Box<dyn Driver> {
    simple("wlmouse", poll)
}
fn poll(ctx: &mut PollCtx) {
    for &(pid, receiver, name, receiver_pid) in MODELS {
        if ctx.is_claimed(0x36a7, pid) {
            continue;
        }
        let mut colls: Vec<_> = ctx
            .colls
            .iter()
            .filter(|c| c.vid == 0x36a7 && c.pid == pid && c.page == 0xffff && c.feat_len >= 65)
            .collect();
        colls.sort_by_key(|c| c.usage != 0);
        for c in colls {
            let Some(d) = HidDev::open_feature_only(c) else { continue };
            let id = format!("wlmouse:36a7:{pid:04x}");
            let obs = match query(&d) {
                Reply::Battery(p, ch) => Observation::battery(id, name, Kind::Mouse, p, ch),
                Reply::Asleep => Observation::new(id, State::Unreachable).name(name).kind(Kind::Mouse),
                _ => continue,
            }
            .via(if receiver { "2.4 GHz receiver" } else { "USB cable" });
            ctx.add(if receiver { obs.grace(Duration::from_secs(1800)) } else { obs }, 0x36a7, pid);
            if receiver_pid != 0 {
                ctx.claim(0x36a7, receiver_pid);
            }
            break;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn status_frames() {
        let mut r = [0, 0xa1, 0, 2, 2, 0, 0x83, 1, 80];
        assert_eq!(parse_reply(&r), Reply::Battery(80, true));
        r[1] = 0xa2;
        assert_eq!(parse_reply(&r), Reply::Battery(80, true));
        r[8] = 255;
        assert_eq!(parse_reply(&r), Reply::Invalid);
        r[1] = 0xa0;
        assert_eq!(parse_reply(&r), Reply::Asleep);
        assert_eq!(parse_reply(&r[..8]), Reply::Invalid);
        r[6] = 0;
        assert_eq!(parse_reply(&r), Reply::Pending);
    }
}
