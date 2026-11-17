use super::{simple, Driver, PollCtx};
use crate::device::{Kind, Observation, State};
#[cfg(not(test))]
use crate::hid::{product, HidDev};
use std::time::{Duration, Instant};
#[cfg(test)]
use tests::{product, FakeHidDev as HidDev};

// Rapoo Hub Nordic54L report 07; PyFlat legacy BB B0 only on verified VT3 Pro/VT1 Pro Max PIDs.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Connection {
    Unknown,
    Receiver,
    Bluetooth,
    Cable,
}
#[derive(Clone, Copy, Debug, PartialEq)]
enum Profile {
    Nordic,
    Legacy,
}
#[derive(Clone, Copy, Debug, PartialEq)]
struct Battery {
    percent: Option<u8>,
    charging: bool,
    connection: Connection,
    mouse_pid: u16,
}
const MODELS: &[(u16, u16, &str)] = &[
    (0x4610, 0x1410, "Rapoo VT0"),
    (0x4620, 0x1420, "Rapoo VT0 Max"),
    (0x4615, 0x1415, "Rapoo VT1"),
    (0x4644, 0x1444, "Rapoo VT1 Max"),
    (0x4622, 0x1422, "Rapoo VT2"),
    (0x4623, 0x1423, "Rapoo VT2 Max"),
    (0x4612, 0x1412, "Rapoo VT3"),
    (0x4617, 0x1417, "Rapoo VT3 Max (Gen 2)"),
    (0x4b06, 0x1b06, "Rapoo VT3 Max V2"),
    (0x4611, 0x1411, "Rapoo VT3S"),
    (0x4621, 0x1421, "Rapoo VT3S Max"),
    (0x4613, 0x1413, "Rapoo VT7"),
    (0x4616, 0x1416, "Rapoo VT7 Max"),
    (0x4614, 0x1414, "Rapoo VT9"),
    (0x4618, 0x1418, "Rapoo VT9 Max"),
    (0x4415, 0x1215, "Rapoo VT3 Pro"),
    (0x4433, 0x1233, "Rapoo VT1 Pro Max"),
];
fn model(pid: u16) -> Option<&'static (u16, u16, &'static str)> {
    MODELS.iter().find(|m| m.0 == pid || m.1 == pid)
}
fn legacy(pid: u16) -> bool {
    matches!(pid, 0x1215 | 0x4415 | 0x1233 | 0x4433)
}
fn decode_battery(r: &[u8], profile: Profile) -> Option<Battery> {
    let (state, percent, connection, mouse_pid) = match (profile, r.first().copied()?) {
        (Profile::Nordic, 7) => {
            let mode = *r.get(1)? & 15;
            let connection = match mode {
                0 => Connection::Receiver,
                1 => Connection::Bluetooth,
                2 => Connection::Cable,
                _ => return None,
            };
            let pid = match (r.get(13), r.get(14)) {
                (Some(a), Some(b)) => u16::from_le_bytes([*a, *b]),
                _ => 0,
            };
            (*r.get(7)?, *r.get(8)?, connection, pid)
        }
        (Profile::Legacy, 0xbb) if r.get(1) == Some(&0xb0) => (*r.get(5)?, *r.get(6)?, Connection::Unknown, 0),
        _ => return None,
    };
    if !matches!(state, 1 | 2) || (percent > 100 && !(profile == Profile::Legacy && state == 2 && percent == 0x7f)) {
        return None;
    }
    Some(Battery { percent: (percent <= 100).then_some(percent), charging: state == 2, connection, mouse_pid })
}
fn read_battery(d: &HidDev, profile: Profile) -> Option<Battery> {
    // Passive push can take 4.03 seconds. Never flush useful queued status or send commands.
    let end = Instant::now() + Duration::from_millis(5000);
    loop {
        let left = end.saturating_duration_since(Instant::now()).as_millis() as u32;
        if left == 0 {
            return None;
        }
        let r = d.read(left)?;
        if let Some(b) = decode_battery(&r, profile) {
            return Some(b);
        }
    }
}
fn observation(pid: u16, product: impl FnOnce() -> String, b: Battery) -> Observation {
    let model = model(if b.mouse_pid != 0 { b.mouse_pid } else { pid });
    let identity = model.map_or(if b.mouse_pid != 0 { b.mouse_pid } else { pid }, |m| m.0);
    let mut name = model.map_or_else(product, |m| m.2.to_owned());
    if name.is_empty() || name == "Rapoo Gaming Device" {
        name = "Rapoo wireless mouse".into();
    }
    let via = match b.connection {
        Connection::Cable => "USB cable",
        Connection::Bluetooth => "Bluetooth",
        Connection::Unknown if model.is_some_and(|m| m.0 == pid) => "USB cable",
        _ => "2.4 GHz receiver",
    };
    let unknown = if b.charging { State::ChargingUnknown } else { State::Unreachable };
    let state = b.percent.map_or(unknown, |percent| State::Battery { percent, charging: b.charging });
    Observation::new(format!("rapoo:24ae:{identity:04x}"), state)
        .name(name)
        .kind(Kind::Mouse)
        .via(via)
        .grace(Duration::from_secs(1800))
}
fn merge(existing: &mut Observation, mut incoming: Observation) {
    if incoming.via == "USB cable" {
        if !matches!(incoming.state, State::Battery { .. }) {
            if let State::Battery { percent, .. } = existing.state {
                incoming.state = State::Battery { percent, charging: incoming.state == State::ChargingUnknown };
            }
        }
        *existing = incoming;
    } else if !matches!(existing.state, State::Battery { .. }) {
        if let State::Battery { percent, .. } = incoming.state {
            existing.state = State::Battery { percent, charging: existing.state == State::ChargingUnknown };
        }
    }
}
fn poll(ctx: &mut PollCtx) {
    for (vid, pid) in ctx.products(0x24ae) {
        for c in ctx
            .find(vid, pid)
            .filter(|c| c.page == 0xff00 && c.usage == 2 && (c.in_len == 19 || (c.in_len == 7 && legacy(pid))))
        {
            let Some(d) = HidDev::open(c) else {
                continue;
            };
            let profile = if c.in_len == 7 { Profile::Legacy } else { Profile::Nordic };
            let Some(b) = read_battery(&d, profile) else {
                continue;
            };
            let obs = observation(pid, || product(c), b);
            if let Some(existing) = ctx.out.iter_mut().find(|o| o.id == obs.id) {
                merge(existing, obs);
                ctx.claim(vid, pid);
            } else {
                ctx.add(obs, vid, pid);
            }
            break;
        }
    }
}
pub fn driver() -> Box<dyn Driver> {
    simple("Rapoo", poll)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::hid::HidColl;
    use std::{
        cell::RefCell,
        collections::{BTreeMap, VecDeque},
    };
    #[derive(Default)]
    struct Transport {
        reports: BTreeMap<Vec<u16>, VecDeque<Vec<u8>>>,
        opens: usize,
    }
    thread_local! {static IO:RefCell<Transport>=RefCell::new(Transport::default());}
    pub(super) struct FakeHidDev {
        path: Vec<u16>,
    }
    pub(super) fn product(_: &HidColl) -> String {
        "Rapoo Gaming Device".into()
    }
    impl FakeHidDev {
        pub fn open(c: &HidColl) -> Option<Self> {
            IO.with(|io| io.borrow_mut().opens += 1);
            Some(Self { path: c.path.clone() })
        }
        pub fn read(&self, timeout: u32) -> Option<Vec<u8>> {
            assert!(timeout > 0 && timeout <= 5000);
            IO.with(|io| io.borrow_mut().reports.get_mut(&self.path).and_then(VecDeque::pop_front))
        }
    }
    fn coll(pid: u16, path: u16, len: u16) -> HidColl {
        HidColl { vid: 0x24ae, pid, page: 0xff00, usage: 2, in_len: len, path: vec![path], ..Default::default() }
    }
    fn setup(reports: Vec<(u16, Vec<Vec<u8>>)>) {
        IO.with(|io| {
            *io.borrow_mut() =
                Transport { reports: reports.into_iter().map(|(p, r)| (vec![p], r.into())).collect(), opens: 0 }
        });
    }
    fn status(cable: bool, p: u8) -> Vec<u8> {
        let mut r = CAPTURE.to_vec();
        r[1] = if cable { 0x22 } else { 0x20 };
        r[7] = if cable { 2 } else { 1 };
        r[8] = p;
        r
    }
    #[test]
    fn production_poll_discovery_identity_and_dedup() {
        let receiver = coll(0x1417, 1, 19);
        let cable = coll(0x4617, 2, 19);
        let colls = [receiver.clone(), cable];
        setup(vec![(1, vec![vec![0xbc, 0], status(false, 25)]), (2, vec![status(true, 27)])]);
        let mut ctx = PollCtx::new(&colls);
        poll(&mut ctx);
        assert_eq!(ctx.out.len(), 1);
        let s = &ctx.out[0];
        assert_eq!(s.id, "rapoo:24ae:4617");
        assert_eq!(s.state, State::Battery { percent: 27, charging: true });
        assert_eq!(s.via, "USB cable");
        assert_eq!(s.kind, Some(Kind::Mouse));
        assert_eq!(s.grace, Duration::from_secs(1800));
        assert!(ctx.is_claimed(0x24ae, 0x1417) && ctx.is_claimed(0x24ae, 0x4617));
        let colls = [receiver.clone()];
        setup(vec![(1, vec![status(false, 25)])]);
        let mut ctx = PollCtx::new(&colls);
        poll(&mut ctx);
        assert_eq!(ctx.out[0].id, "rapoo:24ae:4617");
        assert_eq!(ctx.out[0].state, State::Battery { percent: 25, charging: false });
        assert_eq!(ctx.out[0].via, "2.4 GHz receiver");
        setup(Vec::new());
        let mut ctx = PollCtx::new(&colls);
        poll(&mut ctx);
        assert!(ctx.out.is_empty() && !ctx.is_claimed(0x24ae, 0x1417));
        let old = vec![0xbb, 0xb0, 0x81, 0x20, 3, 1, 37];
        let unknown = vec![0xbb, 0xb0, 0x81, 0x20, 3, 2, 0x7f];
        let colls = [coll(0x1215, 3, 7), coll(0x4415, 4, 7)];
        setup(vec![(3, vec![old.clone()]), (4, vec![unknown.clone()])]);
        let mut ctx = PollCtx::new(&colls);
        poll(&mut ctx);
        assert_eq!(ctx.out.len(), 1);
        assert_eq!(ctx.out[0].id, "rapoo:24ae:4415");
        assert_eq!(ctx.out[0].state, State::Battery { percent: 37, charging: true });
        assert_eq!(ctx.out[0].via, "USB cable");
        let colls = [coll(0x4415, 4, 7)];
        setup(vec![(4, vec![unknown])]);
        let mut ctx = PollCtx::new(&colls);
        poll(&mut ctx);
        assert_eq!(ctx.out[0].state, State::ChargingUnknown);
        let prior = ctx.out;
        let colls = [coll(0x1215, 3, 7)];
        setup(vec![(3, vec![old])]);
        let mut ctx = PollCtx::new(&colls);
        ctx.out = prior;
        poll(&mut ctx);
        assert_eq!(ctx.out.len(), 1);
        assert_eq!(ctx.out[0].state, State::Battery { percent: 37, charging: true });
        assert_eq!(ctx.out[0].via, "USB cable");
        for (pid, p, ch, via) in [(0x1233, 100, false, "2.4 GHz receiver"), (0x4433, 95, true, "USB cable")] {
            let colls = [coll(pid, 5, 7)];
            setup(vec![(5, vec![vec![0xbb, 0xb0, 0x71, 0x84, 3, if ch { 2 } else { 1 }, p]])]);
            let mut ctx = PollCtx::new(&colls);
            poll(&mut ctx);
            assert_eq!(ctx.out[0].id, "rapoo:24ae:4433");
            assert_eq!(ctx.out[0].name.as_deref(), Some("Rapoo VT1 Pro Max"));
            assert_eq!(ctx.out[0].state, State::Battery { percent: p, charging: ch });
            assert_eq!(ctx.out[0].via, via);
        }
        let colls = [receiver];
        setup(vec![(1, vec![vec![0xbb, 0xb0, 0x81, 0x20, 3, 2, 50]])]);
        let mut ctx = PollCtx::new(&colls);
        poll(&mut ctx);
        assert!(ctx.out.is_empty());
        let colls = [coll(0x7abc, 6, 19)];
        let mut report = status(false, 62);
        report[13] = 0;
        report[14] = 0;
        setup(vec![(6, vec![report])]);
        let mut ctx = PollCtx::new(&colls);
        poll(&mut ctx);
        assert_eq!(ctx.out[0].id, "rapoo:24ae:7abc");
        assert_eq!(ctx.out[0].name.as_deref(), Some("Rapoo wireless mouse"));
    }
    #[test]
    fn production_poll_filters_and_claims() {
        let mut wrong_vendor = coll(0x1417, 1, 19);
        wrong_vendor.vid = 0x1234;
        let mut config = coll(0x1417, 1, 19);
        config.usage = 14;
        let mut wrong_page = coll(0x1417, 1, 19);
        wrong_page.page = 1;
        let colls = [wrong_vendor, config, wrong_page, coll(0x1215, 2, 10), coll(0x1c00, 3, 7)];
        setup(Vec::new());
        let mut ctx = PollCtx::new(&colls);
        poll(&mut ctx);
        assert!(ctx.out.is_empty());
        IO.with(|io| assert_eq!(io.borrow().opens, 0));
        let colls = [coll(0x1417, 1, 19)];
        let mut ctx = PollCtx::new(&colls);
        ctx.claim(0x24ae, 0x1417);
        poll(&mut ctx);
        IO.with(|io| assert_eq!(io.borrow().opens, 0));
    }
    const CAPTURE: [u8; 19] = [7, 0x20, 3, 0x20, 3, 0x20, 3, 1, 0x19, 0, 4, 0x84, 0, 0x17, 0x46, 0, 2, 3, 0xbc];
    fn decode(r: &[u8], p: Profile) -> Battery {
        decode_battery(r, p).expect("valid test fixture")
    }
    #[test]
    fn captured_profiles_and_bounds() {
        let b = decode(&CAPTURE, Profile::Nordic);
        assert_eq!(
            b,
            Battery { percent: Some(25), charging: false, mouse_pid: 0x4617, connection: Connection::Receiver }
        );
        let mut r = CAPTURE;
        r[1] = 0x22;
        r[7] = 2;
        r[18] = 0;
        assert_eq!(decode(&r, Profile::Nordic), Battery { charging: true, connection: Connection::Cable, ..b });
        r[8] = 100;
        assert_eq!(decode(&r, Profile::Nordic).percent, Some(100));
        r[1] = 0x21;
        r[7] = 1;
        r[8] = 0;
        assert_eq!(decode(&r, Profile::Nordic), Battery { percent: Some(0), connection: Connection::Bluetooth, ..b });
        r[1] = 0x23;
        assert_eq!(decode_battery(&r, Profile::Nordic), None);
        let mut old = [0xbb, 0xb0, 0x81, 0x20, 3, 1, 37];
        assert_eq!(decode_battery(&old, Profile::Nordic), None);
        assert_eq!(decode_battery(&CAPTURE, Profile::Legacy), None);
        assert_eq!(
            decode(&old, Profile::Legacy),
            Battery { percent: Some(37), charging: false, connection: Connection::Unknown, mouse_pid: 0 }
        );
        old[5] = 2;
        old[6] = 0x7f;
        assert_eq!(decode(&old, Profile::Legacy).percent, None);
        assert!(decode(&old, Profile::Legacy).charging);
        old[6] = 100;
        assert_eq!(decode(&old, Profile::Legacy).percent, Some(100));
        old[5] = 1;
        assert!(!decode(&old, Profile::Legacy).charging);
        assert_eq!(decode(&[0xbb, 0xb0, 0x71, 0x84, 3, 1, 100], Profile::Legacy).percent, Some(100));
        assert_eq!(decode(&[0xbb, 0xb0, 0x71, 0x84, 3, 2, 95], Profile::Legacy).percent, Some(95));
        for n in 0..9 {
            assert_eq!(decode_battery(&CAPTURE[..n], Profile::Nordic), None);
        }
        for n in 0..7 {
            assert_eq!(decode_battery(&old[..n], Profile::Legacy), None);
        }
        assert_eq!(decode(&CAPTURE[..9], Profile::Nordic).mouse_pid, 0);
        for state in [0, 3, 255] {
            r = CAPTURE;
            r[7] = state;
            old[5] = state;
            assert_eq!(decode_battery(&r, Profile::Nordic), None);
            assert_eq!(decode_battery(&old, Profile::Legacy), None);
        }
        for percent in [101, 126, 127, 128, 255] {
            r = CAPTURE;
            r[8] = percent;
            old[5] = 1;
            old[6] = percent;
            assert_eq!(decode_battery(&r, Profile::Nordic), None);
            assert_eq!(decode_battery(&old, Profile::Legacy), None);
        }
        old[0] = 0xbc;
        assert_eq!(decode_battery(&old, Profile::Legacy), None);
        old[0] = 0xbb;
        old[1] = 0;
        assert_eq!(decode_battery(&old, Profile::Legacy), None);
        r = CAPTURE;
        r[7] = 2;
        r[8] = 0x7f;
        assert_eq!(decode_battery(&r, Profile::Nordic), None);
        r = CAPTURE;
        r[0] = 8;
        assert_eq!(decode_battery(&r, Profile::Nordic), None);
    }
    #[test]
    fn stable_identity_and_cable_merge() {
        let b = decode(&CAPTURE, Profile::Nordic);
        let receiver = observation(0x1417, String::new, b);
        let cable = Battery { percent: Some(27), charging: true, connection: Connection::Cable, ..b };
        let mut entry = receiver.clone();
        merge(&mut entry, observation(0x4617, String::new, cable));
        assert_eq!(entry.id, receiver.id);
        assert_eq!(entry.via, "USB cable");
        assert_eq!(entry.state, State::Battery { percent: 27, charging: true });
        assert_eq!(entry.grace, Duration::from_secs(1800));
        let old = decode(&[0xbb, 0xb0, 0x81, 0x20, 3, 1, 37], Profile::Legacy);
        let unknown = decode(&[0xbb, 0xb0, 0x81, 0x20, 3, 2, 0x7f], Profile::Legacy);
        let mut entry = observation(0x1215, String::new, old);
        merge(&mut entry, observation(0x4415, String::new, unknown));
        assert_eq!(entry.state, State::Battery { percent: 37, charging: true });
        assert_eq!(entry.via, "USB cable");
        let mut entry = observation(0x4415, String::new, unknown);
        assert_eq!(entry.state, State::ChargingUnknown);
        merge(&mut entry, observation(0x1215, String::new, old));
        assert_eq!(entry.state, State::Battery { percent: 37, charging: true });
        assert_eq!(entry.via, "USB cable");
        let unnamed = Battery { mouse_pid: 0, ..b };
        let obs = observation(0x7abc, || "Rapoo Gaming Device".into(), unnamed);
        assert_eq!(obs.name.as_deref(), Some("Rapoo wireless mouse"));
        assert_eq!(obs.id, "rapoo:24ae:7abc");
        assert!(legacy(0x1233));
        assert!(legacy(0x4433));
        assert!(!legacy(0x1c00));
    }
}
