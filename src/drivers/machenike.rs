use super::{simple, Driver, PollCtx};
#[cfg(not(test))]
use crate::hid::{product, HidDev};
use crate::{
    device::{Kind, Observation, State},
    hid::HidColl,
};
use std::{
    thread::sleep,
    time::{Duration, Instant},
};
#[cfg(test)]
use tests::{product, FakeHidDev as HidDev};

// Official L7 Mix 1.0.0.8 read-state: report 51 is exactly 21 bytes, not maximum 1001.
fn collection(c: &HidColl, cable: bool) -> bool {
    c.iface == (if cable { 2 } else { 1 })
        && c.page == 0x8c
        && c.usage == 1
        && c.in_len == 33
        && c.out_len == 33
        && c.feat_len == 1001
}
fn request() -> [u8; 21] {
    let mut r = [0; 21];
    r[0] = 0x51;
    r[1] = 6;
    r
}
fn decode_status(r: &[u8], cable: bool) -> Option<State> {
    if r.len() != 21
        || r.get(..2) != Some(&[0x51, 6])
        || r.get(4..8) != Some(&[0x8a, 0x24, 0x4e, 0xff])
        || (*r.get(10)? & 7) != (if cable { 0 } else { 1 })
    {
        return None;
    }
    // Cable presence is authoritative; its wireless link bit is clear too.
    if !cable && *r.get(10)? & 8 == 0 {
        return Some(State::Off);
    }
    let (p, ch) = (*r.get(11)?, *r.get(12)?);
    if p > 100 || ch > 1 {
        return None;
    }
    Some(State::Battery { percent: p, charging: ch == 1 })
}
fn query(d: &HidDev, cable: bool) -> Option<State> {
    let end = Instant::now() + Duration::from_millis(200);
    if !d.set_feature(&request()) {
        return None;
    }
    loop {
        sleep(Duration::from_millis(10));
        if let Some(r) = d.get_feature(0x51) {
            if let Some(s) = decode_status(&r, cable) {
                return Some(s);
            }
        }
        if Instant::now() >= end {
            return None;
        }
    }
}
fn poll(ctx: &mut PollCtx) {
    let mut have_cable = false;
    for pid in [0xff4e, 0xff4c] {
        if ctx.is_claimed(0x248a, pid) {
            continue;
        }
        let cable = pid == 0xff4e;
        for c in ctx.find(0x248a, pid).filter(|c| collection(c, cable)) {
            if product(c) != "L7 Mix" {
                continue;
            }
            // Reserve on failure too: later Ajazz probes share this VID but use another command.
            ctx.claim(0x248a, pid);
            if have_cable {
                break;
            }
            let mut status = c.clone();
            status.feat_len = 21;
            let Some(d) = HidDev::open_feature_only(&status) else {
                break;
            };
            let Some(state) = query(&d, cable) else {
                break;
            };
            let obs = Observation::new("machenike:248a:ff4e:l7", state)
                .name("Machenike L7 Arctic / L7 Mix")
                .kind(Kind::Mouse)
                .via(if cable { "USB cable" } else { "2.4 GHz receiver" })
                .grace(Duration::from_secs(if cable { 0 } else { 1800 }));
            have_cable = cable;
            ctx.add(obs, 0x248a, pid);
            break;
        }
    }
}
pub fn driver() -> Box<dyn Driver> {
    simple("Machenike", poll)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, collections::VecDeque};
    struct Transport {
        replies: VecDeque<Vec<u8>>,
        opens: usize,
        sends: usize,
        reads: usize,
        open_ok: bool,
        send_ok: bool,
        repeat: bool,
        product: &'static str,
    }
    impl Default for Transport {
        fn default() -> Self {
            Self {
                replies: VecDeque::new(),
                opens: 0,
                sends: 0,
                reads: 0,
                open_ok: true,
                send_ok: true,
                repeat: false,
                product: "L7 Mix",
            }
        }
    }
    thread_local! {static IO:RefCell<Transport>=RefCell::new(Transport::default());}
    pub(super) struct FakeHidDev;
    pub(super) fn product(_: &HidColl) -> String {
        IO.with(|io| io.borrow().product.into())
    }
    impl FakeHidDev {
        pub fn open_feature_only(c: &HidColl) -> Option<Self> {
            assert_eq!(c.feat_len, 21);
            IO.with(|io| {
                let mut io = io.borrow_mut();
                io.opens += 1;
                io.open_ok.then_some(Self)
            })
        }
        pub fn set_feature(&self, r: &[u8]) -> bool {
            assert_eq!(r, request());
            IO.with(|io| {
                let mut io = io.borrow_mut();
                io.sends += 1;
                io.send_ok
            })
        }
        pub fn get_feature(&self, id: u8) -> Option<Vec<u8>> {
            assert_eq!(id, 0x51);
            IO.with(|io| {
                let mut io = io.borrow_mut();
                io.reads += 1;
                if io.repeat {
                    io.replies.front().cloned()
                } else {
                    io.replies.pop_front()
                }
            })
        }
    }
    fn coll(cable: bool) -> HidColl {
        HidColl {
            vid: 0x248a,
            pid: if cable { 0xff4e } else { 0xff4c },
            iface: if cable { 2 } else { 1 },
            page: 0x8c,
            usage: 1,
            in_len: 33,
            out_len: 33,
            feat_len: 1001,
            ..Default::default()
        }
    }
    fn setup(replies: Vec<Vec<u8>>) {
        IO.with(|io| *io.borrow_mut() = Transport { replies: replies.into(), ..Default::default() });
    }
    #[test]
    fn production_poll_captures_reservation_and_dedup() {
        for (r, p, cable, ch) in [(WIRELESS, 64, false, false), (CABLE, 74, true, true)] {
            setup(vec![r.to_vec()]);
            let colls = [coll(cable)];
            let mut ctx = PollCtx::new(&colls);
            poll(&mut ctx);
            assert_eq!(ctx.out.len(), 1);
            assert!(ctx.is_claimed(0x248a, colls[0].pid));
            let s = &ctx.out[0];
            assert_eq!(s.state, State::Battery { percent: p, charging: ch });
            assert_eq!(s.kind, Some(Kind::Mouse));
            assert_eq!(s.name.as_deref(), Some("Machenike L7 Arctic / L7 Mix"));
            assert_eq!(s.via, if cable { "USB cable" } else { "2.4 GHz receiver" });
            poll(&mut ctx);
            IO.with(|io| assert_eq!((io.borrow().opens, io.borrow().sends), (1, 1)));
        }
        setup(vec![CABLE.to_vec()]);
        let colls = [coll(true), coll(false)];
        let mut ctx = PollCtx::new(&colls);
        poll(&mut ctx);
        assert_eq!(ctx.out.len(), 1);
        assert!(ctx.is_claimed(0x248a, 0xff4c));
        assert_eq!(ctx.out[0].id, "machenike:248a:ff4e:l7");
        IO.with(|io| assert_eq!(io.borrow().sends, 1));
        let mut offline = WIRELESS;
        offline[10] = 1;
        offline[12] = 1;
        setup(vec![offline.to_vec()]);
        let colls = [coll(false)];
        let mut ctx = PollCtx::new(&colls);
        poll(&mut ctx);
        assert_eq!(ctx.out[0].state, State::Off);
        for failed_open in [true, false] {
            setup(Vec::new());
            IO.with(|io| {
                let mut io = io.borrow_mut();
                if failed_open {
                    io.open_ok = false;
                } else {
                    io.send_ok = false;
                }
            });
            let mut ctx = PollCtx::new(&colls);
            poll(&mut ctx);
            assert!(ctx.out.is_empty() && ctx.is_claimed(0x248a, 0xff4c));
            IO.with(|io| assert_eq!(io.borrow().sends, usize::from(!failed_open)));
        }
        setup(Vec::new());
        IO.with(|io| io.borrow_mut().product = "Unrelated mouse");
        let mut ctx = PollCtx::new(&colls);
        poll(&mut ctx);
        assert!(!ctx.is_claimed(0x248a, 0xff4c));
        IO.with(|io| assert_eq!(io.borrow().opens, 0));
    }
    #[test]
    fn production_poll_malformed_and_deadline() {
        let mut replies = Vec::new();
        for (i, b) in [(0, 0x52), (1, 4), (6, 0x4d), (10, 15), (11, 101), (12, 255)] {
            let mut r = WIRELESS;
            r[i] = b;
            replies.push(r.to_vec());
        }
        replies.push(vec![0; 12]);
        replies.push(WIRELESS.to_vec());
        setup(replies);
        let colls = [coll(false)];
        let mut ctx = PollCtx::new(&colls);
        poll(&mut ctx);
        assert_eq!(ctx.out[0].state, State::Battery { percent: 64, charging: false });
        IO.with(|io| assert!(io.borrow().reads >= 8));
        for unrelated in [false, true] {
            let mut wrong = WIRELESS;
            wrong[1] = 4;
            setup(if unrelated { vec![wrong.to_vec()] } else { Vec::new() });
            IO.with(|io| io.borrow_mut().repeat = unrelated);
            let mut ctx = PollCtx::new(&colls);
            let start = Instant::now();
            poll(&mut ctx);
            assert!(start.elapsed() < Duration::from_millis(800));
            assert!(ctx.out.is_empty() && ctx.is_claimed(0x248a, 0xff4c));
            IO.with(|io| assert_eq!(io.borrow().sends, 1));
        }
        for field in 0..9 {
            setup(Vec::new());
            let mut c = coll(field == 8);
            match field {
                0 => c.vid = 0x249a,
                1 => c.pid = 0xff4d,
                2 => c.iface = 2,
                3 => c.page = 0xff00,
                4 => c.usage = 2,
                5 => c.in_len = 32,
                6 => c.out_len = 32,
                7 => c.feat_len = 1000,
                _ => c.iface = 1,
            }
            let colls = [c];
            let mut ctx = PollCtx::new(&colls);
            poll(&mut ctx);
            assert!(ctx.out.is_empty());
            IO.with(|io| assert_eq!(io.borrow().opens, 0));
        }
    }
    const WIRELESS: [u8; 21] = [0x51, 6, 1, 0, 0x8a, 0x24, 0x4e, 0xff, 5, 1, 9, 0x40, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    const CABLE: [u8; 21] = [0x51, 6, 1, 0, 0x8a, 0x24, 0x4e, 0xff, 5, 1, 0, 0x4a, 1, 0, 0, 0, 0, 0, 0, 0, 0];
    #[test]
    fn captured_status_and_offline() {
        assert_eq!(decode_status(&WIRELESS, false), Some(State::Battery { percent: 64, charging: false }));
        assert_eq!(decode_status(&CABLE, true), Some(State::Battery { percent: 74, charging: true }));
        let mut r = WIRELESS;
        r[10] = 1;
        r[12] = 1;
        assert_eq!(decode_status(&r, false), Some(State::Off));
        for p in [0, 100] {
            r = WIRELESS;
            r[11] = p;
            assert_eq!(decode_status(&r, false), Some(State::Battery { percent: p, charging: false }));
        }
        for (i, b) in [(0, 0x52), (1, 4), (6, 0x4d), (10, 15), (11, 101), (12, 255)] {
            r = WIRELESS;
            r[i] = b;
            assert_eq!(decode_status(&r, false), None);
        }
        for n in 0..21 {
            assert_eq!(decode_status(&WIRELESS[..n], false), None);
        }
        let mut long = WIRELESS.to_vec();
        long.push(0);
        assert_eq!(decode_status(&long, false), None);
        assert_eq!(request(), [0x51, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    }
    #[test]
    fn strict_collection() {
        let c =
            HidColl { iface: 1, page: 0x8c, usage: 1, in_len: 33, out_len: 33, feat_len: 1001, ..Default::default() };
        assert!(collection(&c, false));
        assert!(!collection(&c, true));
        for field in 0..6 {
            let mut bad = c.clone();
            match field {
                0 => bad.iface = 2,
                1 => bad.page = 0xff00,
                2 => bad.usage = 2,
                3 => bad.in_len = 32,
                4 => bad.out_len = 32,
                _ => bad.feat_len = 1000,
            };
            assert!(!collection(&bad, false));
        }
    }
}
