// xpanel GET frames: [report ID][2+n][80|command][n][payload].
// ULX output/input 04/05, SLX 01/02; first GET 21/57, then wake/all 96.
// Sources: OpenMouse mouse-protocol, FinalmousePollingRateSwitcher.
// Cable PIDs 0102/0310/0311 remain outside upstream's verified scope.
use super::{simple, Driver, PollCtx};
use crate::{
    device::{Kind, Observation, State},
    hid::{HidColl, HidDev},
};
use std::time::{Duration, Instant};

const MODELS: &[(u16, &str, bool, u8, u8, u8)] = &[
    (0x0100, "Finalmouse UltralightX", false, 4, 5, 21),
    (0x0300, "Finalmouse Starlight X", true, 1, 2, 57),
    (0x0301, "Finalmouse Starlight X", true, 1, 2, 57),
];
fn percent_from_mv(mv: u16) -> u8 {
    const CURVE: &[(u16, f64)] =
        &[(3000, 0.2), (3620, 5.0), (3660, 10.0), (3740, 25.0), (3880, 50.0), (4170, 75.0), (4380, 100.0)];
    if mv <= 3000 {
        return 0;
    }
    for pair in CURVE.windows(2) {
        if let [(lo, p), (hi, q)] = pair {
            if mv <= *hi {
                return (p + (q - p) * f64::from(mv - lo) / f64::from(hi - lo) + 0.5).floor() as u8;
            }
        }
    }
    100
}
#[derive(Default, Debug, PartialEq)]
struct Status {
    soc: Option<u8>,
    mv: u16,
    charging: Option<u8>,
    link: Option<u8>,
}
fn valid_mv(mv: u16) -> bool {
    mv > 0 && mv < 5000
}
fn decode(r: &[u8], in_id: u8, st: &mut Status) {
    let Some((&id, rest)) = r.split_first() else { return };
    if id != in_id || r.len() < 4 {
        return;
    }
    let Some(&inner) = rest.first() else { return };
    let inner = usize::from(inner).min(r.len() - 2);
    if inner < 2 {
        return;
    }
    let Some(&len) = r.get(3) else { return };
    let n = usize::from(len).min(inner - 2);
    let Some(p) = r.get(4..4 + n) else { return };
    match (r.get(2), p) {
        (Some(5), [lo, hi, ..]) => {
            let mv = u16::from_le_bytes([*lo, *hi]);
            if valid_mv(mv) {
                st.mv = mv;
            }
        }
        (Some(38), [pct, lo, hi, ..]) if *pct <= 100 => {
            st.soc = Some(*pct);
            let mv = u16::from_le_bytes([*lo, *hi]);
            if valid_mv(mv) {
                st.mv = mv;
            }
        }
        (Some(37), [charging, ..]) => st.charging = Some(*charging),
        (Some(36), [link, ..]) => st.link = Some(*link),
        _ => (),
    }
}
fn get_request(id: u8, cmd: u8) -> [u8; 4] {
    [id, 2, 0x80 | cmd, 0]
}
fn query(d: &HidDev, slx: bool, out_id: u8, in_id: u8, first: u8) -> Option<Status> {
    if !d.write(&get_request(out_id, first), 500) || !d.write(&get_request(out_id, 96), 500) {
        return None;
    }
    let mut st = Status::default();
    let mut end = Instant::now() + Duration::from_millis(1000);
    let mut shortened = false;
    loop {
        let level = st.soc.is_some() || (!slx && st.mv > 0);
        if level && st.charging.is_some() {
            break;
        }
        if level && !shortened {
            end = end.min(Instant::now() + Duration::from_millis(150));
            shortened = true;
        }
        let left = end.saturating_duration_since(Instant::now()).as_millis() as u32;
        if left == 0 {
            break;
        }
        let Some(r) = d.read(left) else { break };
        decode(&r, in_id, &mut st);
    }
    Some(st)
}
fn matches(c: &HidColl, pid: u16, slx: bool) -> bool {
    c.vid == 0x361d && c.pid == pid && c.page == 0xff00 && (slx || c.usage == 1) && c.in_len >= 4 && c.out_len >= 4
}
fn observation(pid: u16, name: &str, st: &Status) -> Option<Observation> {
    let id = format!("finalmouse:361d:{pid:04x}");
    let obs = if let Some(p) = st.soc.or_else(|| (st.mv > 0).then(|| percent_from_mv(st.mv))) {
        Observation::battery(id, name, Kind::Mouse, p, st.charging == Some(1))
    } else if st.link == Some(0) {
        Observation::new(id, State::Unreachable).name(name).kind(Kind::Mouse)
    } else {
        return None;
    };
    Some(obs.millivolts(st.mv).via("2.4 GHz receiver").grace(Duration::from_secs(1800)))
}
pub fn driver() -> Box<dyn Driver> {
    simple("finalmouse", poll)
}
fn poll(ctx: &mut PollCtx) {
    for &(pid, name, slx, out_id, in_id, first) in MODELS {
        if ctx.is_claimed(0x361d, pid) {
            continue;
        }
        let mut colls: Vec<_> = ctx.colls.iter().filter(|c| matches(c, pid, slx)).collect();
        colls.sort_by_key(|c| c.usage != 1);
        for c in colls {
            let Some(d) = HidDev::open(c) else { continue };
            let Some(st) = query(&d, slx, out_id, in_id, first) else { continue };
            if let Some(obs) = observation(pid, name, &st) {
                ctx.add(obs, 0x361d, pid);
                break;
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn reply(id: u8, cmd: u8, p: &[u8]) -> Vec<u8> {
        let mut r = vec![0; 64];
        r[..4].copy_from_slice(&[id, (2 + p.len()) as u8, cmd, p.len() as u8]);
        r[4..4 + p.len()].copy_from_slice(p);
        r
    }
    #[test]
    fn upstream_calibration() {
        for (mv, p) in [(2950, 0), (3000, 0), (3640, 8), (3880, 50), (4276, 88), (4380, 100), (4450, 100), (4170, 75)] {
            assert_eq!(percent_from_mv(mv), p);
        }
        assert_eq!(get_request(4, 21), [4, 2, 0x95, 0]);
        assert_eq!(get_request(4, 96), [4, 2, 0xe0, 0]);
        assert_eq!(get_request(1, 57), [1, 2, 0xb9, 0]);
    }
    #[test]
    fn upstream_bursts() {
        let mut st = Status::default();
        for r in
            [reply(3, 10, b"1.0\0"), reply(5, 3, &[0x40, 6]), reply(5, 5, &3880u16.to_le_bytes()), reply(5, 37, &[1])]
        {
            decode(&r, 5, &mut st);
        }
        assert_eq!(st.mv, 3880);
        assert_eq!(st.charging, Some(1));
        let obs = observation(0x100, "Finalmouse UltralightX", &st).expect("valid fixture");
        assert_eq!(obs.state, State::Battery { percent: 50, charging: true });
        assert_eq!(obs.grace, Duration::from_secs(1800));
        decode(&reply(5, 38, &[64, 0x2c, 0x0f]), 5, &mut st);
        assert_eq!(st.soc, Some(64));
        assert_eq!(st.mv, 3884);
        let mut slx = Status::default();
        for r in [
            reply(5, 5, &4380u16.to_le_bytes()),
            reply(2, 16, &[0x40, 6]),
            reply(2, 5, &3880u16.to_le_bytes()),
            reply(2, 38, &[76, 0x44, 0x0f]),
            reply(2, 37, &[0]),
        ] {
            decode(&r, 2, &mut slx);
        }
        assert_eq!(slx, Status { soc: Some(76), mv: 0x0f44, charging: Some(0), link: None });
        let mut asleep = Status::default();
        decode(&reply(5, 36, &[0]), 5, &mut asleep);
        decode(&reply(5, 5, &[0, 0]), 5, &mut asleep);
        assert_eq!(observation(0x100, "ULX", &asleep).map(|o| o.state), Some(State::Unreachable));
    }
    #[test]
    fn upstream_invalid_frames() {
        let mut truncated = reply(5, 5, &3880u16.to_le_bytes());
        truncated[1] = 3;
        for r in [
            reply(5, 36, &[1]),
            reply(5, 5, &[0x28]),
            reply(5, 5, &6000u16.to_le_bytes()),
            reply(5, 38, &[101, 0x2c, 0x0f]),
            reply(2, 5, &3880u16.to_le_bytes()),
            reply(5, 5, &[]),
            truncated,
            vec![],
        ] {
            let mut st = Status::default();
            decode(&r, 5, &mut st);
            assert!(observation(0x100, "ULX", &st).is_none());
        }
        let base =
            HidColl { vid: 0x361d, pid: 0x100, page: 0xff00, usage: 1, in_len: 64, out_len: 64, ..Default::default() };
        assert!(matches(&base, 0x100, false));
        for variant in 0..5 {
            let mut c = base.clone();
            match variant {
                0 => c.pid = 0x102,
                1 => c.vid = 0x361e,
                2 => c.usage = 2,
                3 => c.page = 1,
                _ => c.out_len = 0,
            };
            assert!(!matches(&c, 0x100, false));
        }
    }
}
