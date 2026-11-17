use super::{simple, Driver, PollCtx};
use crate::device::{Kind, Observation};
#[cfg(not(test))]
use crate::hid::HidDev;
use std::time::{Duration, Instant};
#[cfg(test)]
use tests::FakeHidDev as HidDev;

// EM901X AIM-WL GetPower: feature 08 on FF02:0002, input 09 on FF01:0000.
fn request() -> [u8; 17] {
    let mut r = [0; 17];
    r[0] = 8;
    r[1] = 4;
    r[16] = 0x49; // 0x55 minus bytes 0..15.
    r
}
fn parse_battery(r: &[u8]) -> Option<(u8, bool)> {
    if r.len() != 17 || r.first() != Some(&9) || r.get(1) != Some(&4) || r.get(2) != Some(&0) {
        return None;
    }
    let percent = *r.get(6)?;
    (percent <= 100).then_some((percent, *r.get(7)? != 0))
}
fn poll(ctx: &mut PollCtx) {
    let (vid, pid) = (0x260d, 0x1074);
    if ctx.is_claimed(vid, pid) {
        return;
    }
    let mut inputs = ctx.find(vid, pid).filter(|c| c.iface == 1 && c.page == 0xff01 && c.usage == 0 && c.in_len == 17);
    let mut features =
        ctx.find(vid, pid).filter(|c| c.iface == 1 && c.page == 0xff02 && c.usage == 2 && c.feat_len == 17);
    let (Some(i), Some(f)) = (inputs.next(), features.next()) else {
        return;
    };
    // Never pair arbitrary collections from multiple receivers.
    if inputs.next().is_some() || features.next().is_some() {
        return;
    }
    let Some(input) = HidDev::open(i) else {
        return;
    };
    let Some(feature) = HidDev::open_feature_only(f) else {
        return;
    };
    input.flush();
    if !feature.set_feature(&request()) {
        return;
    }
    let end = Instant::now() + Duration::from_millis(1000);
    for _ in 0..32 {
        let left = end.saturating_duration_since(Instant::now()).as_millis() as u32;
        if left == 0 {
            return;
        }
        let Some(r) = input.read(left) else {
            return;
        };
        if let Some((p, ch)) = parse_battery(&r) {
            ctx.add(
                Observation::battery("dareu:260d:1074", "DAREU EM901X", Kind::Mouse, p, ch).via("2.4 GHz receiver"),
                vid,
                pid,
            );
            return;
        }
    }
}
pub fn driver() -> Box<dyn Driver> {
    simple("Dareu", poll)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{device::State, hid::HidColl};
    use std::{cell::RefCell, collections::VecDeque};
    struct Transport {
        replies: VecDeque<Vec<u8>>,
        opens: usize,
        sends: usize,
        reads: usize,
        open_ok: bool,
        feature_ok: bool,
        send_ok: bool,
        delay: u64,
    }
    impl Default for Transport {
        fn default() -> Self {
            Self {
                replies: VecDeque::new(),
                opens: 0,
                sends: 0,
                reads: 0,
                open_ok: true,
                feature_ok: true,
                send_ok: true,
                delay: 0,
            }
        }
    }
    thread_local! { static IO:RefCell<Transport>=RefCell::new(Transport::default()); }
    pub(super) struct FakeHidDev {
        input: bool,
    }
    impl FakeHidDev {
        pub fn open(c: &HidColl) -> Option<Self> {
            assert_eq!(c.path, vec![1]);
            IO.with(|io| {
                let mut io = io.borrow_mut();
                io.opens += 1;
                io.open_ok.then_some(Self { input: true })
            })
        }
        pub fn open_feature_only(c: &HidColl) -> Option<Self> {
            assert_eq!(c.path, vec![2]);
            IO.with(|io| {
                let mut io = io.borrow_mut();
                io.opens += 1;
                io.feature_ok.then_some(Self { input: false })
            })
        }
        pub fn flush(&self) {
            assert!(self.input);
        }
        pub fn set_feature(&self, r: &[u8]) -> bool {
            assert!(!self.input);
            assert_eq!(r, request());
            IO.with(|io| {
                let mut io = io.borrow_mut();
                io.sends += 1;
                io.send_ok
            })
        }
        pub fn read(&self, timeout: u32) -> Option<Vec<u8>> {
            assert!(self.input && timeout > 0 && timeout <= 1000);
            IO.with(|io| {
                let mut io = io.borrow_mut();
                assert_eq!(io.sends, 1);
                io.reads += 1;
                if io.delay != 0 {
                    std::thread::sleep(Duration::from_millis(io.delay.min(u64::from(timeout))));
                }
                io.replies.pop_front()
            })
        }
    }
    fn collections() -> Vec<HidColl> {
        vec![
            HidColl {
                vid: 0x260d,
                pid: 0x1074,
                iface: 1,
                page: 0xff01,
                usage: 0,
                in_len: 17,
                path: vec![1],
                ..Default::default()
            },
            HidColl {
                vid: 0x260d,
                pid: 0x1074,
                iface: 1,
                page: 0xff02,
                usage: 2,
                feat_len: 17,
                path: vec![2],
                ..Default::default()
            },
        ]
    }
    fn setup(replies: Vec<Vec<u8>>) {
        IO.with(|io| *io.borrow_mut() = Transport { replies: replies.into(), ..Default::default() });
    }
    #[test]
    fn production_poll_transport() {
        let colls = collections();
        setup(vec![reply(85, 0)]);
        let mut ctx = PollCtx::new(&colls);
        poll(&mut ctx);
        assert!(ctx.is_claimed(0x260d, 0x1074));
        assert_eq!(ctx.out.len(), 1);
        assert_eq!(ctx.out[0].id, "dareu:260d:1074");
        assert_eq!(ctx.out[0].name.as_deref(), Some("DAREU EM901X"));
        assert_eq!(ctx.out[0].kind, Some(Kind::Mouse));
        assert_eq!(ctx.out[0].via, "2.4 GHz receiver");
        assert_eq!(ctx.out[0].state, State::Battery { percent: 85, charging: false });
        poll(&mut ctx);
        IO.with(|io| {
            let io = io.borrow();
            assert_eq!((io.opens, io.sends, io.reads), (2, 1, 1));
        });
        for p in [0, 100] {
            setup(vec![reply(p, 2)]);
            let mut ctx = PollCtx::new(&colls);
            poll(&mut ctx);
            assert_eq!(ctx.out[0].state, State::Battery { percent: p, charging: true });
        }
        let mut bad = Vec::new();
        for (i, b) in [(0, 8), (1, 3), (2, 1)] {
            let mut r = reply(85, 0);
            r[i] = b;
            bad.push(r);
        }
        bad.push(reply(85, 0)[..7].to_vec());
        let mut long = reply(85, 0);
        long.push(0);
        bad.push(long);
        bad.push(reply(101, 0));
        bad.push(reply(42, 1));
        setup(bad);
        let mut ctx = PollCtx::new(&colls);
        poll(&mut ctx);
        assert_eq!(ctx.out[0].state, State::Battery { percent: 42, charging: true });
        IO.with(|io| assert_eq!(io.borrow().reads, 7));
        let mut wrong = reply(85, 0);
        wrong[1] = 3;
        setup(vec![wrong; 33]);
        let mut ctx = PollCtx::new(&colls);
        poll(&mut ctx);
        assert!(ctx.out.is_empty());
        IO.with(|io| assert_eq!(io.borrow().reads, 32));
        for failure in 0..4 {
            setup(Vec::new());
            IO.with(|io| {
                let mut io = io.borrow_mut();
                match failure {
                    1 => io.send_ok = false,
                    2 => io.open_ok = false,
                    3 => io.feature_ok = false,
                    _ => {}
                }
            });
            let mut ctx = PollCtx::new(&colls);
            poll(&mut ctx);
            assert!(ctx.out.is_empty() && !ctx.is_claimed(0x260d, 0x1074));
        }
    }
    #[test]
    fn strict_pairing_and_deadline() {
        for field in 0..10 {
            setup(Vec::new());
            let mut colls = collections();
            match field {
                0 => colls[0].pid = 0x1090,
                1 => colls[1].vid = 0x3554,
                2 => colls[0].page = 0xff03,
                3 => colls[1].usage = 0,
                4 => colls[0].in_len = 8,
                5 => colls[1].feat_len = 65,
                6 => colls[0].iface = 2,
                7 => colls.push(colls[0].clone()),
                8 => colls.push(colls[1].clone()),
                _ => {
                    colls.pop();
                }
            }
            let mut ctx = PollCtx::new(&colls);
            poll(&mut ctx);
            assert!(ctx.out.is_empty());
            IO.with(|io| assert_eq!(io.borrow().opens, 0));
        }
        let mut wrong = reply(85, 0);
        wrong[1] = 3;
        setup(vec![wrong; 32]);
        IO.with(|io| io.borrow_mut().delay = 80);
        let colls = collections();
        let mut ctx = PollCtx::new(&colls);
        let start = Instant::now();
        poll(&mut ctx);
        assert!(start.elapsed() >= Duration::from_millis(900) && start.elapsed() < Duration::from_millis(1500));
        assert!(ctx.out.is_empty());
        IO.with(|io| assert!(io.borrow().reads < 32));
    }
    fn reply(p: u8, ch: u8) -> Vec<u8> {
        vec![9, 4, 0, 0, 0, 2, p, ch, 0, 0, 0, 0, 0, 0, 0, 0, 0xf1]
    }
    #[test]
    fn captured_reply_and_bounds() {
        assert_eq!(parse_battery(&reply(85, 0)), Some((85, false)));
        for p in [0, 100] {
            assert_eq!(parse_battery(&reply(p, 2)), Some((p, true)));
        }
        for p in [101, 255] {
            assert_eq!(parse_battery(&reply(p, 0)), None);
        }
        for (i, b) in [(0, 8), (1, 3), (2, 1)] {
            let mut r = reply(85, 0);
            r[i] = b;
            assert_eq!(parse_battery(&r), None);
        }
        let r = reply(42, 1);
        for n in 0..17 {
            assert_eq!(parse_battery(&r[..n]), None);
        }
        let mut long = r.clone();
        long.push(0);
        assert_eq!(parse_battery(&long), None);
        assert_eq!(parse_battery(&r), Some((42, true)));
        assert_eq!(request(), [8, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x49]);
    }
}
