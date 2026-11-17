//! HyperX/Kingston headset protocols and NGENUITY Haste 2 battery queries.
use super::{Driver, PollCtx};
use crate::{
    device::{Kind, Observation, State},
    hid::{self, HidColl, HidDev},
};
use std::{
    thread::sleep,
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
enum Protocol {
    Cloud3,
    Cloud2,
    Kingston,
    Alpha,
    Alpha2,
    Haste,
    Flight,
    Cloud3S,
    Stinger,
}
struct Model {
    vid: u16,
    pids: &'static [u16],
    accept: fn(&HidColl) -> bool,
    protocol: Protocol,
}
const MODELS: &[Model] = &[
    Model {
        vid: 0x03f0,
        pids: &[0x05b7, 0x0c9d],
        accept: |c| c.out_len == 62 && c.in_len == 62,
        protocol: Protocol::Cloud3,
    },
    Model {
        vid: 0x03f0,
        pids: &[0x069f, 0x0995],
        accept: |c| c.page >= 0xff00 && c.out_len >= 62 && c.in_len >= 62,
        protocol: Protocol::Cloud3,
    },
    Model {
        vid: 0x03f0,
        pids: &[0x06be, 0x02cc],
        accept: |c| c.out_len == 64 && c.in_len == 64,
        protocol: Protocol::Cloud3S,
    },
    Model {
        vid: 0x03f0,
        pids: &[0x0696, 0x018b, 0x0d93],
        accept: |c| c.page == 0xff90 && c.usage == 0x0303,
        protocol: Protocol::Cloud2,
    },
    Model { vid: 0x0951, pids: &[0x1718], accept: |c| c.out_len == 62, protocol: Protocol::Kingston },
    Model { vid: 0x03f0, pids: &[0x098d], accept: |c| c.page >= 0xff00 && c.out_len >= 3, protocol: Protocol::Alpha },
    Model {
        vid: 0x03f0,
        pids: &[0x08be],
        accept: |c| c.page == 0xff13 && c.usage == 0xff00,
        protocol: Protocol::Alpha2,
    },
    Model {
        vid: 0x03f0,
        pids: &[0x0ab5, 0x0d97, 0x0f98],
        accept: |c| c.page >= 0xff00 && c.out_len >= 2 && c.in_len >= 8,
        protocol: Protocol::Haste,
    },
    Model {
        vid: 0x0951,
        pids: &[0x16c4, 0x1723, 0x1749],
        accept: |c| c.page >= 0xff00 && c.out_len >= 3,
        protocol: Protocol::Flight,
    },
    Model { vid: 0x03f0, pids: &[0x0e90], accept: |c| c.page >= 0xff00 && c.out_len >= 3, protocol: Protocol::Flight },
    Model {
        vid: 0x03f0,
        pids: &[0x0a93],
        accept: |c| c.page == 0xff00 && c.usage == 1 && c.feat_len >= 32,
        protocol: Protocol::Stinger,
    },
    Model {
        vid: 0x0951,
        pids: &[0x170b, 0x1713],
        accept: |c| c.page == 0xff00 && c.usage == 1 && c.feat_len >= 32,
        protocol: Protocol::Stinger,
    },
];
fn base(vid: u16, pid: u16, name: &str) -> Observation {
    Observation::new(format!("hyperx:{vid:04x}:{pid:04x}"), State::Unreachable)
        .name(name)
        .kind(Kind::Headset)
        .via("2.4 GHz receiver")
}
fn query(d: &HidDev, req: &[u8], timeout: u32, matches: impl Fn(&[u8]) -> bool) -> Option<Vec<u8>> {
    if !d.write(req, 500) {
        return None;
    }
    let end = Instant::now() + Duration::from_millis(timeout.into());
    loop {
        let left = end.saturating_duration_since(Instant::now()).as_millis() as u32;
        if left == 0 {
            return None;
        }
        let r = d.read(left)?;
        if matches(&r) {
            return Some(r);
        }
    }
}
fn matches_reply(p: Protocol, cmd: u8, sub: u8, r: &[u8]) -> bool {
    match p {
        Protocol::Cloud3 => r.len() >= 5 && r.get(..2) == Some(&[0x66, cmd][..]),
        Protocol::Cloud2 => r.len() >= 8 && r.get(..4) == Some(&[6, 0xff, 0xbb, cmd][..]),
        Protocol::Kingston => r.len() >= 8 && r.first() == Some(&0x0b) && r.get(2..4) == Some(&[0xbb, cmd][..]),
        Protocol::Alpha => r.len() >= 4 && r.first() == Some(&0x21) && r.get(2) == Some(&cmd),
        Protocol::Alpha2 | Protocol::Haste => r.len() >= 8 && r.get(..2) == Some(&[cmd.wrapping_add(1), sub][..]),
        Protocol::Cloud3S => {
            r.len() >= 7 && r.first() == Some(&0x0c) && r.get(5) == Some(&cmd) && r.get(6) != Some(&0xff)
        }
        Protocol::Flight => r.len() >= 5,
        Protocol::Stinger => false,
    }
}
fn ask(d: &HidDev, p: Protocol, cmd: u8, sub: u8) -> Option<Vec<u8>> {
    let req = match p {
        Protocol::Cloud3 => vec![0x66, cmd],
        Protocol::Cloud2 => vec![6, 0xff, 0xbb, cmd],
        Protocol::Kingston => vec![6, 0, 2, 0, 0x9a, 0, 0, 0x68, 0x4a, 0x8e, 0x0a, 0, 0, 0, 0xbb, cmd, 0],
        Protocol::Alpha => vec![0x21, 0xbb, cmd],
        Protocol::Alpha2 | Protocol::Haste => vec![cmd, sub],
        Protocol::Cloud3S => vec![0x0c, 2, 3, 1, 0, cmd],
        Protocol::Flight => vec![0x21, 0xff, 5],
        Protocol::Stinger => return None,
    };
    query(d, &req, if matches!(p, Protocol::Cloud3) { 800 } else { 1000 }, |r| matches_reply(p, cmd, sub, r))
}
fn parse_pair(r: &[u8], full_is_charging: bool) -> Option<(u8, bool, u16)> {
    r.get(..8)?;
    let pct = *r.get(2)?;
    if pct > 100 {
        return None;
    }
    let charge = *r.get(3)?;
    Some((pct, charge == 1 || (full_is_charging && charge == 2), u16::from_le_bytes([*r.get(6)?, *r.get(7)?])))
}
fn set_battery(s: &mut Observation, pct: u8, charging: bool) {
    s.state = State::Battery { percent: pct, charging };
}
#[derive(Default)]
struct HyperX {
    link_up: Option<Instant>,
    polled: bool,
}
impl HyperX {
    fn haste(&mut self, pid: u16, now: Instant, mut q: impl FnMut(u8, u8) -> Option<Vec<u8>>) -> Option<Observation> {
        let link = q(0x52, 1);
        let linked = link.as_ref().and_then(|r| r.get(2)).is_none_or(|v| *v != 0);
        // Reconnect floods can hide the link reply; a battery reply still establishes presence.
        let battery = if linked { q(0x50, 2).and_then(|r| parse_pair(&r, true)) } else { None };
        if link.is_none() && battery.is_none() {
            return None;
        }
        if !linked {
            self.link_up = None;
        } else if self.link_up.is_none() {
            self.link_up = Some(if self.polled { now } else { now - Duration::from_secs(6) });
        }
        self.polled = true;
        let settled = self.link_up.is_some_and(|at| now.duration_since(at) >= Duration::from_secs(6));
        let mut s = base(
            0x03f0,
            pid,
            if pid == 0x0ab5 { "HyperX Pulsefire Haste 2 Core Wireless" } else { "HyperX Pulsefire Haste 2 Wireless" },
        )
        .kind(Kind::Mouse);
        if !linked {
            s.state = State::Off;
        }
        if settled {
            if let Some((pct, charge, mv)) = battery {
                set_battery(&mut s, pct, charge);
                s.millivolts = Some(mv);
            }
        }
        Some(s)
    }
    fn poll_one(&mut self, c: &HidColl, p: Protocol) -> Option<Observation> {
        let d = HidDev::open(c)?;
        if matches!(p, Protocol::Haste) {
            return self.haste(c.pid, Instant::now(), |cmd, sub| ask(&d, p, cmd, sub));
        }
        if matches!(p, Protocol::Stinger) {
            return stinger(&d, c.vid, c.pid);
        }
        let q = |cmd| ask(&d, p, cmd, 0);
        let mut s;
        match p {
            Protocol::Cloud3 => {
                let r = q(0x82)?;
                s = base(
                    c.vid,
                    c.pid,
                    if [0x069f, 0x0995].contains(&c.pid) {
                        "HyperX Cloud II Core Wireless"
                    } else {
                        "HyperX Cloud III Wireless"
                    },
                );
                if r.get(2) == Some(&0) {
                    s.state = State::Off;
                    return Some(s);
                }
                let charge = q(0x8a).is_some_and(|r| r.get(2) == Some(&1));
                if let Some(r) = q(0x89) {
                    if let Some(&pct) = r.get(4).filter(|v| **v <= 100) {
                        set_battery(&mut s, pct, charge);
                        s.millivolts = Some(u16::from_be_bytes([*r.get(2)?, *r.get(3)?]));
                    }
                }
            }
            Protocol::Cloud2 | Protocol::Kingston => {
                let r = q(2)?;
                let pct = *r.get(7)?;
                if pct > 100 {
                    return None;
                }
                s = base(c.vid, c.pid, "HyperX Cloud II Wireless");
                set_battery(&mut s, pct, q(3).is_some_and(|r| r.get(4) == Some(&1)));
                if matches!(p, Protocol::Cloud2) {
                    s.millivolts = Some(u16::from_be_bytes([*r.get(5)?, *r.get(6)?]));
                }
            }
            Protocol::Alpha => {
                let r = q(3)?;
                s = base(c.vid, c.pid, "HyperX Cloud Alpha Wireless");
                if r.get(3) == Some(&1) {
                    s.state = State::Off;
                    return Some(s);
                }
                let charge = q(0x0c).is_some_and(|r| r.get(3) == Some(&1));
                if let Some(pct) = q(0x0b).and_then(|r| r.get(3).copied()).filter(|v| *v <= 100) {
                    set_battery(&mut s, pct, charge);
                }
            }
            Protocol::Alpha2 => {
                let r = ask(&d, p, 0x52, 1)?;
                s = base(c.vid, c.pid, "HyperX Cloud Alpha 2 Wireless");
                if r.get(2) == Some(&0) {
                    s.state = State::Off;
                    return Some(s);
                }
                if let Some((pct, charge, mv)) = ask(&d, p, 0x50, 2).and_then(|r| parse_pair(&r, false)) {
                    set_battery(&mut s, pct, charge);
                    s.millivolts = Some(mv);
                }
            }
            Protocol::Flight => {
                let r = q(5)?;
                let mv = u16::from_be_bytes([*r.get(3)?, *r.get(4)?]);
                s = base(c.vid, c.pid, "HyperX Cloud Flight").millivolts(mv);
                if let Some(pct) = flight_percent(mv) {
                    set_battery(&mut s, pct, false);
                }
            }
            Protocol::Cloud3S => {
                let r = q(2)?;
                s = base(c.vid, c.pid, "HyperX Cloud III S Wireless");
                if r.get(6) != Some(&2) {
                    s.state = State::Off;
                    return Some(s);
                }
                let pct = q(6).and_then(|r| r.get(6).copied()).filter(|v| *v <= 100);
                let charge = q(0x48).is_some_and(|r| matches!(r.get(6), Some(1 | 2)));
                if let Some(pct) = pct {
                    set_battery(&mut s, pct, charge);
                }
            }
            Protocol::Haste | Protocol::Stinger => return None,
        }
        Some(s)
    }
}
fn flight_percent(mv: u16) -> Option<u8> {
    if mv > 0x100b {
        return None;
    }
    let v = f64::from(mv);
    Some(if mv <= 3648 {
        hid::clamp100((0.00125 * v) as i32)
    } else if mv > 3975 {
        100
    } else {
        hid::clamp100(
            (0.00000002547505 * v.powi(4) - 0.0003900299 * v.powi(3) + 2.238321 * v * v - 5706.256 * v + 5452299.0)
                .round() as i32,
        )
    })
}
const CONNECTION: &[u8] = &[0xff, 7, 0, 0xfd, 4, 0, 0, 2, 0x82, 3, 5, 0x29, 0x0f, 0, 0, 3, 0x68, 0x10, 0, 0];
const BATTERY: &[u8] = &[0xff, 7, 0, 0xfd, 4, 0x0c, 0xf1, 2, 1, 4, 0xf0, 0x0c];
const CHARGING: &[(i32, i32)] = &[
    (4130, 100),
    (4090, 90),
    (4050, 80),
    (4000, 70),
    (3967, 60),
    (3946, 50),
    (3930, 40),
    (3916, 30),
    (3871, 20),
    (3831, 10),
    (3400, 0),
];
const DISCHARGING: &[(i32, i32)] = &[
    (4121, 100),
    (4042, 90),
    (3954, 80),
    (3905, 70),
    (3838, 60),
    (3785, 50),
    (3763, 40),
    (3738, 30),
    (3700, 20),
    (3651, 10),
    (3517, 0),
];
fn fresh(r: &[u8]) -> bool {
    r.get(1..3) != Some(&[1, 0][..])
}
fn stinger_battery(r: &[u8]) -> Option<(u8, bool, u16)> {
    r.get(..13)?;
    let code = *r.get(9)?;
    if ![3, 5, 6].contains(&code) {
        return None;
    }
    let mv = u16::from_le_bytes([*r.get(11)?, *r.get(12)?]);
    let pct = if code == 6 { 100 } else { hid::interpolate(mv.into(), if code == 5 { CHARGING } else { DISCHARGING }) };
    Some((pct, code == 5, mv))
}
fn stinger(d: &HidDev, vid: u16, pid: u16) -> Option<Observation> {
    let ps = [0x0a93, 0x1713].contains(&pid);
    let exchange = |cmd: &[u8], good: fn(&[u8]) -> bool| -> (bool, Option<Vec<u8>>) {
        if !d.set_feature(cmd) {
            return (false, None);
        }
        let mut last = None;
        for _ in 0..if ps { 10 } else { 1 } {
            sleep(Duration::from_millis(20));
            let Some(r) = d.get_feature(0xff) else { return (false, last) };
            if r.len() < 13 {
                return (false, Some(r));
            }
            let ok = !ps || good(&r);
            last = Some(r);
            if ok {
                return (true, last);
            }
        }
        (false, last)
    };
    let (ok, r) = exchange(CONNECTION, fresh);
    let r = r?;
    if !ok && !(ps && r.len() >= 13 && !fresh(&r)) {
        return None;
    }
    let mut s = base(vid, pid, "HyperX Cloud Stinger Core Wireless");
    if r.get(11) != Some(&1) {
        s.state = State::Off;
        return Some(s);
    }
    let (ok, r) = exchange(BATTERY, |r| r.get(..3) == Some(&[0xff, 0x12, 5][..]));
    if ok {
        if let Some((pct, charge, mv)) = r.and_then(|r| stinger_battery(&r)) {
            set_battery(&mut s, pct, charge);
            s.millivolts = Some(mv);
        }
    }
    Some(s)
}
impl Driver for HyperX {
    fn name(&self) -> &'static str {
        "hyperx"
    }
    fn poll(&mut self, ctx: &mut PollCtx) {
        poll_with(self, ctx, Self::poll_one);
    }
}
fn poll_with(
    h: &mut HyperX,
    ctx: &mut PollCtx,
    mut query: impl FnMut(&mut HyperX, &HidColl, Protocol) -> Option<Observation>,
) {
    for m in MODELS {
        for &pid in m.pids {
            if ctx.is_claimed(m.vid, pid) {
                continue;
            }
            let colls = ctx.colls;
            for c in colls.iter().filter(|c| c.vid == m.vid && c.pid == pid && (m.accept)(c)) {
                if let Some(s) = query(h, c, m.protocol) {
                    ctx.add(s, m.vid, pid);
                    break;
                }
            }
        }
    }
}
pub fn driver() -> Box<dyn Driver> {
    Box::new(HyperX::default())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn haste_driver_claims_filtering_and_noise() {
        let core =
            HidColl { vid: 0x03f0, pid: 0x0ab5, page: 0xff00, usage: 1, in_len: 64, out_len: 64, ..HidColl::default() };
        let mut h = HyperX::default();
        let now = Instant::now();
        let colls = [core.clone()];
        let mut ctx = PollCtx::new(&colls);
        let mut writes = Vec::new();
        poll_with(&mut h, &mut ctx, |h, c, p| {
            h.haste(c.pid, now, |cmd, sub| {
                writes.push((cmd, sub));
                let reports = if cmd == 0x52 {
                    vec![report(0x44, 1, 0, 0), report(0x45, 2, 0, 0), report(0x53, 1, 1, 0)]
                } else {
                    vec![report(0xfb, 1, 1, 0), report(0x51, 2, 70, 0)]
                };
                reports.into_iter().find(|r| matches_reply(p, cmd, sub, r))
            })
        });
        assert_eq!(writes, [(0x52, 1), (0x50, 2)]);
        assert_eq!(ctx.out.len(), 1);
        assert!(ctx.is_claimed(0x03f0, 0x0ab5));
        assert_eq!(ctx.out[0].id, "hyperx:03f0:0ab5");
        assert_eq!(ctx.out[0].name.as_deref(), Some("HyperX Pulsefire Haste 2 Core Wireless"));
        assert_eq!(ctx.out[0].via, "2.4 GHz receiver");
        assert_eq!(ctx.out[0].state, State::Battery { percent: 70, charging: false });
        poll_with(&mut h, &mut ctx, |_, _, _| panic!("claimed product queried"));
        for invalid in 0..3 {
            let mut c = core.clone();
            match invalid {
                0 => c.page = 1,
                1 => c.in_len = 4,
                _ => c.out_len = 1,
            }
            let colls = [c];
            let mut ctx = PollCtx::new(&colls);
            poll_with(&mut h, &mut ctx, |_, _, _| panic!("filtered collection queried"));
            assert!(ctx.out.is_empty());
            assert!(!ctx.is_claimed(0x03f0, 0x0ab5));
        }
        let mut ctx = PollCtx::new(&colls);
        poll_with(&mut h, &mut ctx, |h, c, _| h.haste(c.pid, now, |_, _| None));
        assert!(ctx.out.is_empty());
        assert!(!ctx.is_claimed(0x03f0, 0x0ab5));
    }
    fn report(cmd: u8, sub: u8, value: u8, charge: u8) -> Vec<u8> {
        let mut r = vec![0; 64];
        r[0] = cmd;
        r[1] = sub;
        r[2] = value;
        r[3] = charge;
        r
    }
    fn sample(
        h: &mut HyperX,
        pid: u16,
        now: Instant,
        link: Option<u8>,
        battery: Option<(u8, u8)>,
    ) -> (Option<Observation>, Vec<(u8, u8)>) {
        let mut writes = Vec::new();
        let s = h.haste(pid, now, |cmd, sub| {
            writes.push((cmd, sub));
            if cmd == 0x52 {
                link.map(|v| report(0x53, 1, v, 0))
            } else {
                battery.map(|(v, c)| report(0x51, 2, v, c))
            }
        });
        (s, writes)
    }
    #[test]
    fn haste_upstream_sequences() {
        let mut h = HyperX::default();
        let now = Instant::now();
        for (pid, pct, ch, charging) in [
            (0x0ab5, 70, 0, false),
            (0x0ab5, 55, 1, true),
            (0x0ab5, 100, 2, true),
            (0x0ab5, 40, 3, false),
            (0x0d97, 33, 0, false),
            (0x0f98, 34, 0, false),
        ] {
            let (s, w) = sample(&mut h, pid, now, Some(1), Some((pct, ch)));
            let s = s.unwrap();
            assert_eq!(w, [(0x52, 1), (0x50, 2)]);
            assert_eq!(s.state, State::Battery { percent: pct, charging });
            assert_eq!(s.kind, Some(Kind::Mouse));
        }
        assert!(matches!(
            sample(&mut h, 0x0ab5, now, None, Some((70, 0))).0.unwrap().state,
            State::Battery { percent: 70, .. }
        ));
        assert_eq!(sample(&mut h, 0x0ab5, now, Some(1), Some((101, 0))).0.unwrap().state, State::Unreachable);
        let (s, w) = sample(&mut h, 0x0ab5, now, None, None);
        assert!(s.is_none());
        assert_eq!(w, [(0x52, 1), (0x50, 2)]);
        let (s, w) = sample(&mut h, 0x0ab5, now, Some(0), Some((70, 0)));
        assert_eq!(s.unwrap().state, State::Off);
        assert_eq!(w, [(0x52, 1)]);
        assert_eq!(sample(&mut h, 0x0ab5, now, Some(1), Some((40, 0))).0.unwrap().state, State::Unreachable);
        assert!(matches!(
            sample(&mut h, 0x0ab5, now + Duration::from_secs(6), Some(1), Some((70, 0))).0.unwrap().state,
            State::Battery { percent: 70, .. }
        ));
    }
    #[test]
    fn haste_framing_noise_voltage_and_bounds() {
        for r in [report(0x44, 1, 0, 0), report(0x45, 2, 0, 0), report(0xfb, 1, 1, 0)] {
            assert!(!matches_reply(Protocol::Haste, 0x50, 2, &r));
        }
        let mut r = report(0x51, 2, 80, 0);
        r[6] = 0xa4;
        r[7] = 0x0f;
        assert!(matches_reply(Protocol::Haste, 0x50, 2, &r));
        assert_eq!(parse_pair(&r, true), Some((80, false, 0x0fa4)));
        assert!(parse_pair(&r[..7], true).is_none());
        r[2] = 101;
        assert!(parse_pair(&r, true).is_none());
    }
    #[test]
    fn headset_reply_layouts() {
        for (p, cmd, r) in [
            (Protocol::Cloud3, 0x89, vec![0x66, 0x89, 0x0f, 0xa4, 80]),
            (Protocol::Cloud2, 2, vec![6, 0xff, 0xbb, 2, 0, 0x0f, 0xa4, 80]),
            (Protocol::Kingston, 2, vec![0x0b, 0, 0xbb, 2, 0, 0, 0, 80]),
            (Protocol::Alpha, 3, vec![0x21, 0, 3, 0]),
            (Protocol::Cloud3S, 6, vec![0x0c, 0, 0, 0, 0, 6, 80]),
            (Protocol::Flight, 5, vec![0, 0, 0, 0x0f, 0xa4]),
        ] {
            assert!(matches_reply(p, cmd, 0, &r));
            assert!(!matches_reply(p, cmd, 0, &[]));
        }
        let mut r = [0; 13];
        r[9] = 6;
        assert_eq!(stinger_battery(&r), Some((100, false, 0)));
        r[9] = 4;
        assert!(stinger_battery(&r).is_none());
        assert_eq!(flight_percent(0x100c), None);
        assert_eq!(flight_percent(3976), Some(100));
        assert_eq!(flight_percent(3648), Some(4));
    }
}
