//! Headsets follow HeadsetControl; mice follow rivalcfg.
use super::{simple, Driver, PollCtx};
use crate::{
    device::{Kind, Observation, State},
    hid::{self, HidColl, HidDev},
};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq)]
enum Protocol {
    Arctis1,
    Arctis7,
    Arctis9,
    Pro,
    NovaPro,
    Nova,
    Aerox,
    Rival,
    RivalPro,
}
struct Model {
    pids: &'static [u16],
    accept: fn(&HidColl) -> bool,
    protocol: Protocol,
}
const MODELS: &[Model] = &[
    Model {
        pids: &[0x12b3, 0x12b6, 0x12d7],
        accept: |c| c.page == 0xff43 && c.usage == 0x0202,
        protocol: Protocol::Arctis1,
    },
    Model {
        pids: &[0x1260, 0x12ad, 0x1252],
        accept: |c| c.page == 0xff43 && c.usage == 0x0202,
        protocol: Protocol::Arctis7,
    },
    Model { pids: &[0x12c2], accept: |c| c.iface == 0 && c.out_len > 0, protocol: Protocol::Arctis9 },
    Model { pids: &[0x1290], accept: |c| c.iface == 0 && c.out_len > 0, protocol: Protocol::Pro },
    Model { pids: &[0x12e0, 0x225d], accept: |c| c.iface == 4 && c.out_len > 0, protocol: Protocol::NovaPro },
    Model {
        pids: &[
            0x220e, 0x2212, 0x2216, 0x2236, 0x2269, 0x226d, 0x2232, 0x2253, 0x230a, 0x2202, 0x22a1, 0x227e, 0x2206,
            0x2258, 0x229e, 0x22ad, 0x223a, 0x22a9, 0x227a, 0x22a4, 0x22a5, 0x220a, 0x22a7,
        ],
        accept: |c| c.page == 0xffc0 && c.usage == 1,
        protocol: Protocol::Nova,
    },
    Model {
        pids: &[0x1838, 0x183a, 0x1852, 0x1854, 0x1858, 0x185a, 0x1840, 0x1842],
        accept: |c| c.iface == 3 && c.out_len > 0,
        protocol: Protocol::Aerox,
    },
    Model { pids: &[0x1830, 0x1872], accept: |c| c.iface == 3 && c.out_len > 0, protocol: Protocol::Rival },
    Model { pids: &[0x172b, 0x1726], accept: |c| c.iface == 0 && c.out_len > 0, protocol: Protocol::Rival },
    Model { pids: &[0x1880], accept: |c| c.iface == 2 && c.out_len > 0, protocol: Protocol::RivalPro },
];
fn wired(pid: u16) -> bool {
    [0x183a, 0x1854, 0x185a, 0x1842, 0x172b].contains(&pid)
}
fn request(p: Protocol, pid: u16) -> (&'static [u8], u32) {
    match p {
        Protocol::Arctis1 => (&[6, 0x12], 1000),
        Protocol::Arctis7 => (&[6, 0x18], 1000),
        Protocol::Arctis9 => (&[0, 0x20], 1000),
        Protocol::Pro => (&[0, 0x41, 0xaa], 1000),
        Protocol::NovaPro => (&[6, 0xb0], 1000),
        Protocol::Nova => (&[0, 0xb0], 1000),
        Protocol::Aerox => (if wired(pid) { &[0, 0x92] } else { &[0, 0xd2] }, 300),
        Protocol::Rival => (&[0, 0xaa, 1], 300),
        Protocol::RivalPro => (&[0, 0x92], 500),
    }
}
fn parse(p: Protocol, pid: u16, r: &[u8]) -> Option<Observation> {
    let mut connected = true;
    let mut charging = false;
    let mut pct = None;
    let mut mouse = false;
    let name = match p {
        Protocol::Arctis1 => {
            let r = r.get(..4)?;
            connected = *r.get(2)? != 1;
            if connected {
                pct = Some(hid::clamp100(i32::from(*r.get(3)?)));
            }
            "SteelSeries Arctis 1 Wireless"
        }
        Protocol::Arctis7 => {
            let v = *r.get(2)?;
            pct = Some(hid::clamp100(v.into()));
            connected = v != 0;
            if pid == 0x1252 {
                "SteelSeries Arctis Pro"
            } else {
                "SteelSeries Arctis 7"
            }
        }
        Protocol::Arctis9 => {
            r.get(..5)?;
            pct = Some(hid::map_to_100(i32::from(*r.get(3)?), 0x64, 0x9a));
            charging = *r.get(4)? == 1;
            "SteelSeries Arctis 9"
        }
        Protocol::Pro => {
            connected = *r.first()? != 2;
            "SteelSeries Arctis Pro Wireless"
        }
        Protocol::NovaPro => {
            r.get(..16)?;
            connected = *r.get(15)? != 1;
            charging = *r.get(15)? == 2;
            if connected {
                pct = Some(hid::map_to_100(i32::from(*r.get(6)?), 0, 8));
            }
            "SteelSeries Arctis Nova Pro Wireless"
        }
        Protocol::Nova => {
            r.get(..7)?;
            if [0x220e, 0x2212, 0x2216, 0x2236].contains(&pid) {
                connected = *r.get(1)? != 1;
                pct = Some(hid::map_to_100(i32::from(*r.get(2)?), 0, 4));
                charging = *r.get(3)? == 1;
                "SteelSeries Arctis 7+"
            } else if [0x2269, 0x226d, 0x2232, 0x2253].contains(&pid) {
                connected = *r.get(1)? != 2;
                pct = Some(hid::clamp100(i32::from(*r.get(3)?)));
                charging = *r.get(4)? == 1;
                if [0x2232, 0x2253].contains(&pid) {
                    "SteelSeries Arctis Nova 5"
                } else {
                    "SteelSeries Arctis Nova 3P Wireless"
                }
            } else if pid == 0x230a {
                if *r.get(3)? == 3 {
                    pct = r.get(5).copied();
                }
                if *r.get(4)? == 3 {
                    let v = *r.get(6)?;
                    pct = Some(pct.map_or(v, |old| old.min(v)));
                }
                connected = pct.is_some();
                pct = pct.map(|v| hid::clamp100(v.into()));
                "SteelSeries Arctis GameBuds"
            } else {
                connected = *r.get(3)? != 0;
                charging = matches!(r.get(3), Some(1 | 2));
                let v = i32::from(*r.get(2)?);
                pct = Some(if [0x2202, 0x2206, 0x220a, 0x223a, 0x227a, 0x22a4].contains(&pid) {
                    hid::map_to_100(v, 0, 4)
                } else {
                    hid::clamp100(v)
                });
                "SteelSeries Arctis Nova 7"
            }
        }
        Protocol::Aerox => {
            mouse = true;
            let v = *r.get(1)?;
            let raw = v & 0x7f;
            if raw == 0 {
                connected = false;
            } else if raw <= 21 {
                pct = Some((raw - 1) * 5);
                charging = v & 0x80 != 0;
            }
            match pid {
                0x1838 | 0x183a => "SteelSeries Aerox 3 Wireless",
                0x1852 | 0x1854 => "SteelSeries Aerox 5 Wireless",
                0x1858 | 0x185a => "SteelSeries Aerox 9 Wireless",
                _ => "SteelSeries Prime Wireless",
            }
        }
        Protocol::Rival => {
            r.get(..3)?;
            mouse = true;
            pct = Some(hid::clamp100(i32::from(*r.first()?)));
            charging = *r.get(2)? != 0;
            match pid {
                0x172b | 0x1726 => "SteelSeries Rival 650 Wireless",
                0x1872 => "SteelSeries Rival 3 Wireless Gen 2",
                _ => "SteelSeries Rival 3 Wireless",
            }
        }
        Protocol::RivalPro => {
            if r.get(..2)? != [0x92, 0] {
                return None;
            }
            let v = *r.get(2)?;
            if v > 100 {
                return None;
            }
            pct = Some(v);
            mouse = true;
            "SteelSeries Rival Pro"
        }
    };
    // The original Arctis 7 reports a zero battery together with its offline flag.
    let state = if !connected && !(p == Protocol::Arctis7) {
        State::Off
    } else if let Some(percent) = pct {
        State::Battery { percent, charging }
    } else {
        State::Unreachable
    };
    let key_pid = if [0x172b, 0x1726].contains(&pid) { 0x172b } else { pid };
    Some(
        Observation::new(format!("steelseries:1038:{key_pid:04x}"), state)
            .name(name)
            .kind(if mouse { Kind::Mouse } else { Kind::Headset })
            .via(if p == Protocol::RivalPro {
                "UWB base station"
            } else if mouse && wired(pid) {
                "USB cable"
            } else {
                "2.4 GHz receiver"
            }),
    )
}
trait Transport {
    fn flush(&self);
    fn write(&self, req: &[u8], timeout: u32) -> bool;
    fn read(&self, timeout: u32) -> Option<Vec<u8>>;
}
impl Transport for HidDev {
    fn flush(&self) {
        HidDev::flush(self);
    }
    fn write(&self, req: &[u8], timeout: u32) -> bool {
        HidDev::write(self, req, timeout)
    }
    fn read(&self, timeout: u32) -> Option<Vec<u8>> {
        HidDev::read(self, timeout)
    }
}
fn write_read(d: &impl Transport, req: &[u8], timeout: u32) -> Option<Vec<u8>> {
    d.flush();
    if !d.write(req, 500) {
        return None;
    }
    d.read(timeout).filter(|r| !r.is_empty())
}
fn pro_command(d: &impl Transport, cmd: u8) -> Option<Vec<u8>> {
    write_read(d, &[0, cmd, 0xaa], 1000).or_else(|| write_read(d, &[cmd, 0xaa], 1000))
}
fn query(d: &impl Transport, p: Protocol, pid: u16) -> Option<Observation> {
    if p == Protocol::Pro {
        let r = pro_command(d, 0x41)?;
        let mut s = parse(p, pid, &r)?;
        if s.state != State::Off {
            if let Some(v) = pro_command(d, 0x40).and_then(|r| r.first().copied()).filter(|v| *v <= 4) {
                s.state = State::Battery { percent: hid::map_to_100(v.into(), 0, 4), charging: false };
            }
        }
        return Some(s);
    }
    let (req, timeout) = request(p, pid);
    if p != Protocol::RivalPro {
        return parse(p, pid, &write_read(d, req, timeout)?);
    }
    d.flush();
    if !d.write(req, 500) {
        return None;
    }
    let end = Instant::now() + Duration::from_millis(500);
    loop {
        let left = end.saturating_duration_since(Instant::now()).as_millis() as u32;
        if left == 0 {
            return None;
        }
        let r = d.read(left)?;
        if r.len() < 3 {
            return None;
        }
        if r.get(..2) == Some(&[0x92, 0][..]) {
            return parse(p, pid, &r);
        }
    }
}
pub fn driver() -> Box<dyn Driver> {
    simple("steelseries", poll)
}
fn poll(ctx: &mut PollCtx) {
    poll_with(ctx, HidDev::open);
}
fn poll_with<T: Transport>(ctx: &mut PollCtx, mut open: impl FnMut(&HidColl) -> Option<T>) {
    for (vid, pid) in ctx.products(0x1038) {
        let Some(m) = MODELS.iter().find(|m| m.pids.contains(&pid)) else { continue };
        let colls = ctx.colls;
        for c in colls.iter().filter(|c| c.vid == vid && c.pid == pid && (m.accept)(c)) {
            let Some(d) = open(c) else { continue };
            let Some(s) = query(&d, m.protocol, pid) else { continue };
            if let Some(old) = ctx.out.iter_mut().find(|old| old.id == s.id) {
                if s.via == "USB cable" {
                    *old = s;
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
    use std::{cell::Cell, rc::Rc};
    struct Mock {
        reply: Vec<u8>,
        request: Vec<u8>,
        write_ok: bool,
        page: u16,
        writes: Rc<Cell<usize>>,
        reads: Rc<Cell<usize>>,
    }
    impl Transport for Mock {
        fn flush(&self) {}
        fn write(&self, req: &[u8], timeout: u32) -> bool {
            assert_eq!(req, self.request);
            assert!(timeout > 0 && timeout <= 500);
            self.writes.set(self.writes.get() + 1);
            self.write_ok && self.page == 0xffc0
        }
        fn read(&self, timeout: u32) -> Option<Vec<u8>> {
            assert!(timeout > 0 && timeout <= 1000);
            self.reads.set(self.reads.get() + 1);
            (!self.reply.is_empty()).then(|| self.reply.clone())
        }
    }
    fn collection(pid: u16) -> HidColl {
        HidColl {
            vid: 0x1038,
            pid,
            iface: 4,
            page: 0xffc0,
            usage: 1,
            in_len: 64,
            out_len: 64,
            feat_len: 1024,
            ..HidColl::default()
        }
    }
    #[test]
    fn nova_pro_transport_and_filtering() {
        for pid in [0x225d, 0x12e0] {
            for scenario in 0..10 {
                let mut c = collection(pid);
                let mut reply = vec![0; 64];
                reply[0] = 6;
                reply[1] = 0xb0;
                reply[6] = 4;
                reply[15] = 8;
                let mut colls = vec![c.clone()];
                match scenario {
                    1 => {
                        let mut other = c.clone();
                        other.page = 0xff00;
                        colls.push(other);
                    }
                    2 => {
                        let mut other = c.clone();
                        other.page = 0xff00;
                        colls.insert(0, other);
                    }
                    3 => reply.clear(),
                    4 => reply.truncate(15),
                    7 => {
                        c.vid = 0x1039;
                        colls = vec![c];
                    }
                    8 => {
                        c.pid = 0x225e;
                        colls = vec![c];
                    }
                    9 => {
                        c.iface = 3;
                        colls = vec![c];
                    }
                    _ => {}
                }
                let opens = Cell::new(0);
                let writes = Rc::new(Cell::new(0));
                let reads = Rc::new(Cell::new(0));
                let mut ctx = PollCtx::new(&colls);
                let mut open = |c: &HidColl| {
                    opens.set(opens.get() + 1);
                    (scenario != 6).then(|| Mock {
                        reply: reply.clone(),
                        request: vec![6, 0xb0],
                        write_ok: scenario != 5,
                        page: c.page,
                        writes: writes.clone(),
                        reads: reads.clone(),
                    })
                };
                poll_with(&mut ctx, &mut open);
                if scenario <= 2 {
                    assert_eq!(ctx.out.len(), 1);
                    assert!(ctx.is_claimed(0x1038, pid));
                    assert_eq!(ctx.out[0].state, State::Battery { percent: 50, charging: false });
                    assert_eq!(ctx.out[0].name.as_deref(), Some("SteelSeries Arctis Nova Pro Wireless"));
                    assert_eq!(ctx.out[0].id, format!("steelseries:1038:{pid:04x}"));
                    assert_eq!(opens.get(), if scenario == 2 { 2 } else { 1 });
                    assert_eq!(writes.get(), opens.get());
                    assert_eq!(reads.get(), 1);
                    poll_with(&mut ctx, &mut open);
                    assert_eq!(writes.get(), opens.get());
                } else {
                    assert!(ctx.out.is_empty());
                    assert!(!ctx.is_claimed(0x1038, pid));
                    assert_eq!(reads.get(), if scenario == 3 || scenario == 4 { 1 } else { 0 });
                    assert_eq!(opens.get(), if scenario >= 7 { 0 } else { 1 });
                }
            }
            let mut c = collection(pid);
            c.out_len = 0;
            let colls = [c];
            let mut ctx = PollCtx::new(&colls);
            poll_with::<Mock>(&mut ctx, |_| panic!("filtered collection opened"));
            assert!(ctx.out.is_empty());
        }
    }
    #[test]
    fn mouse_transport_failures() {
        for pid in [0x1838, 0x183a, 0x1852, 0x1854, 0x1858, 0x185a, 0x1840, 0x1842] {
            for (reply, write_ok) in [(vec![0xd2, 0x13], false), (vec![], true), (vec![0xd2], true)] {
                let mock = Mock {
                    reply,
                    request: request(Protocol::Aerox, pid).0.to_vec(),
                    write_ok,
                    page: 0xffc0,
                    writes: Rc::new(Cell::new(0)),
                    reads: Rc::new(Cell::new(0)),
                };
                assert!(query(&mock, Protocol::Aerox, pid).is_none());
                assert_eq!(mock.reads.get(), usize::from(write_ok));
            }
        }
    }
    #[test]
    fn aerox_eight_models_and_all_levels() {
        let samples = [
            (1, Some(0), false),
            (0x13, Some(90), false),
            (0x15, Some(100), false),
            (0x81, Some(0), true),
            (0x93, Some(90), true),
            (0x95, Some(100), true),
            (0xff, None, false),
            (0x7f, None, false),
            (0x16, None, false),
            (0x96, None, false),
            (0, None, false),
            (0x80, None, false),
        ];
        for &pid in MODELS[6].pids {
            for (raw, pct, charging) in samples {
                let s = parse(Protocol::Aerox, pid, &[0xd2, raw]).unwrap();
                let expected = if raw & 0x7f == 0 {
                    State::Off
                } else if let Some(percent) = pct {
                    State::Battery { percent, charging }
                } else {
                    State::Unreachable
                };
                assert_eq!(s.state, expected);
                assert_eq!(s.kind, Some(Kind::Mouse));
                assert_eq!(s.via, if wired(pid) { "USB cable" } else { "2.4 GHz receiver" });
                assert_eq!(request(Protocol::Aerox, pid).0, if wired(pid) { &[0, 0x92][..] } else { &[0, 0xd2][..] });
            }
            for r in [&[][..], &[0xd2][..]] {
                assert!(parse(Protocol::Aerox, pid, r).is_none());
            }
        }
    }
    #[test]
    fn nova_pro_upstream_samples() {
        let mut r = [0; 64];
        r[0] = 6;
        r[1] = 0xb0;
        r[6] = 4;
        r[15] = 8;
        for pid in [0x225d, 0x12e0] {
            assert_eq!(
                parse(Protocol::NovaPro, pid, &r).unwrap().state,
                State::Battery { percent: 50, charging: false }
            );
            for (level, status, pct, charging) in [(0, 2, 0, true), (8, 2, 100, true)] {
                r[6] = level;
                r[15] = status;
                assert_eq!(parse(Protocol::NovaPro, pid, &r).unwrap().state, State::Battery { percent: pct, charging });
            }
            r[15] = 1;
            assert_eq!(parse(Protocol::NovaPro, pid, &r).unwrap().state, State::Off);
            assert!(parse(Protocol::NovaPro, pid, &r[..15]).is_none());
            assert!(parse(Protocol::NovaPro, pid, &[]).is_none());
            r[6] = 4;
            r[15] = 8;
        }
    }
    #[test]
    fn remaining_reply_layouts() {
        for (p, pid, r) in [
            (Protocol::Arctis1, 0x12b3, vec![6, 0x12, 0, 70]),
            (Protocol::Arctis7, 0x1260, vec![6, 0x18, 70]),
            (Protocol::Arctis9, 0x12c2, vec![0, 0, 0, 0x7f, 1]),
            (Protocol::Pro, 0x1290, vec![4]),
            (Protocol::Nova, 0x220e, vec![0, 0, 2, 1, 0, 0, 0]),
            (Protocol::Rival, 0x1726, vec![70, 0, 1]),
            (Protocol::RivalPro, 0x1880, vec![0x92, 0, 70]),
        ] {
            assert!(parse(p, pid, &r).is_some());
            assert!(parse(p, pid, &[]).is_none());
        }
        assert!(parse(Protocol::RivalPro, 0x1880, &[0x92, 0, 101]).is_none());
    }
}
