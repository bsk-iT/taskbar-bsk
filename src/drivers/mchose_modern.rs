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

// M HUB readDeviceInfo (0900): report 4D, data at 8, XOR of bytes 2 through data end.
const PIDS: &[u16] = &[0x1014, 0x1018, 0x1016, 0x102c, 0x102d];
fn collection(c: &HidColl) -> bool {
    matches!(c.page, 0xff01 | 0xff0b) && c.out_len >= 64 && c.in_len >= 21
}
fn request(len: usize) -> Vec<u8> {
    let mut r = vec![0; len.max(9)];
    r[0] = 0x4d;
    r[1] = 1;
    r[2] = 1;
    r[5] = 9;
    r[8] = 8;
    r
}
fn parse_info(r: &[u8]) -> Option<State> {
    if r.len() < 10 || r.first() != Some(&0x4d) || r.get(4..6) != Some(&[0, 9]) {
        return None;
    }
    let len = usize::from(*r.get(3)?);
    let checksum = *r.get(8 + len)?;
    // Receiver's short FF payload is a valid unlinked response, not a checksum failure.
    if len < 14 {
        return Some(State::Unreachable);
    }
    if checksum != r.get(2..8 + len)?.iter().fold(0, |a, b| a ^ b) || r.get(8..10) != Some(&[0x37, 0x38]) {
        return None;
    }
    if r.get(18) == Some(&0) {
        return Some(State::Unreachable);
    }
    let percent = *r.get(20)?;
    if percent > 100 {
        return None;
    }
    Some(State::Battery { percent, charging: r.get(19) == Some(&1) })
}
fn poll(ctx: &mut PollCtx) {
    for &pid in PIDS {
        for c in ctx.find(0x3837, pid).filter(|c| collection(c)) {
            let Some(d) = HidDev::open(c) else {
                continue;
            };
            d.flush();
            let mut state = None;
            for attempt in 0..3 {
                if state.is_some() {
                    break;
                }
                if attempt != 0 {
                    sleep(Duration::from_millis(150));
                }
                if !d.write(&request(c.out_len as usize), 500) {
                    continue;
                }
                let end = Instant::now() + Duration::from_millis(1000);
                loop {
                    let left = end.saturating_duration_since(Instant::now()).as_millis() as u32;
                    if left == 0 {
                        break;
                    }
                    let Some(r) = d.read(left) else {
                        break;
                    };
                    if let Some(parsed) = parse_info(&r) {
                        state = Some(parsed);
                        break;
                    }
                }
            }
            let Some(state) = state else {
                continue;
            };
            let mut name = product(c);
            if name.is_empty() {
                name = "MCHOSE mouse".into();
            }
            ctx.add(
                Observation::new(format!("mchose:3837:{pid:04x}"), state)
                    .name(name)
                    .kind(Kind::Mouse)
                    .via("2.4 GHz receiver")
                    .grace(Duration::from_secs(1800)),
                0x3837,
                pid,
            );
            break;
        }
    }
}
pub fn driver() -> Box<dyn Driver> {
    simple("Mchose modern", poll)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, collections::VecDeque};
    #[derive(Default)]
    struct Transport {
        pending: VecDeque<Vec<u8>>,
        opens: usize,
        writes: usize,
    }
    thread_local! {static IO:RefCell<Transport>=RefCell::new(Transport::default());}
    pub(super) struct FakeHidDev;
    pub(super) fn product(_: &HidColl) -> String {
        "MCHOSE K7 V2 Ultra+".into()
    }
    impl FakeHidDev {
        pub fn open(_: &HidColl) -> Option<Self> {
            IO.with(|io| io.borrow_mut().opens += 1);
            Some(Self)
        }
        pub fn flush(&self) {}
        pub fn write(&self, r: &[u8], _: u32) -> bool {
            assert_eq!(r.len(), 64);
            assert_eq!(&r[..9], &[0x4d, 1, 1, 0, 0, 9, 0, 0, 8]);
            assert!(r[9..].iter().all(|b| *b == 0));
            IO.with(|io| io.borrow_mut().writes += 1);
            true
        }
        pub fn read(&self, timeout: u32) -> Option<Vec<u8>> {
            assert!(timeout > 0 && timeout <= 1000);
            IO.with(|io| io.borrow_mut().pending.pop_front())
        }
    }
    fn coll(pid: u16, page: u16, out_len: u16, in_len: u16) -> HidColl {
        HidColl { vid: 0x3837, pid, page, usage: 1, out_len, in_len, ..Default::default() }
    }
    fn simulate(replies: Vec<Vec<u8>>, c: HidColl) -> PollCtx<'static> {
        IO.with(|io| *io.borrow_mut() = Transport { pending: replies.into(), ..Default::default() });
        let colls = [c];
        let mut ctx = PollCtx::new(&colls);
        poll(&mut ctx);
        let mut result = PollCtx::new(&[]);
        if ctx.is_claimed(colls[0].vid, colls[0].pid) {
            result.claim(colls[0].vid, colls[0].pid);
        }
        result.out = ctx.out;
        result
    }
    #[test]
    fn production_poll_transport() {
        let c = || coll(0x1014, 0xff01, 64, 64);
        let ctx = simulate(vec![reply(2, 0, 29)], c());
        assert!(ctx.is_claimed(0x3837, 0x1014));
        assert_eq!(ctx.out.len(), 1);
        let s = &ctx.out[0];
        assert_eq!(s.state, State::Battery { percent: 29, charging: false });
        assert_eq!(s.name.as_deref(), Some("MCHOSE K7 V2 Ultra+"));
        assert_eq!(s.id, "mchose:3837:1014");
        assert_eq!(s.kind, Some(Kind::Mouse));
        assert_eq!(s.via, "2.4 GHz receiver");
        assert_eq!(s.grace, Duration::from_secs(1800));
        IO.with(|io| assert_eq!(io.borrow().writes, 1));
        let ctx = simulate(vec![reply(2, 1, 100)], c());
        assert_eq!(ctx.out[0].state, State::Battery { percent: 100, charging: true });
        let mut off = vec![0; 64];
        off[0] = 0x4d;
        off[1] = 1;
        off[2] = 1;
        off[3] = 1;
        off[5] = 9;
        off[8] = 0xff;
        let ctx = simulate(vec![off.clone()], c());
        assert_eq!(ctx.out[0].state, State::Unreachable);
        IO.with(|io| assert_eq!(io.borrow().writes, 1));
        off[5] = 0;
        let ctx = simulate(vec![off, reply(2, 0, 31)], c());
        assert_eq!(ctx.out[0].state, State::Battery { percent: 31, charging: false });
        let ctx = simulate(Vec::new(), c());
        assert!(ctx.out.is_empty() && !ctx.is_claimed(0x3837, 0x1014));
        IO.with(|io| assert_eq!(io.borrow().writes, 3));
        let mut checksum_bad = reply(2, 0, 29);
        checksum_bad[24] ^= 0xff;
        let mut vendor_bad = reply(2, 0, 29);
        vendor_bad[8] = 0x34;
        checksum(&mut vendor_bad);
        let mut id_bad = reply(2, 0, 29);
        id_bad[0] = 0x4e;
        let ctx = simulate(vec![checksum_bad, reply(2, 0, 101), vendor_bad, id_bad], c());
        assert!(ctx.out.is_empty());
        IO.with(|io| assert_eq!(io.borrow().writes, 3));
        let mut short = reply(2, 0, 29);
        short[3] = 12;
        let ctx = simulate(vec![short], c());
        assert_eq!(ctx.out[0].state, State::Unreachable);
        for (pid, page, out_len, in_len) in
            [(0x1012, 0xff00, 64, 64), (0x1014, 1, 64, 64), (0x1014, 0xff01, 63, 64), (0x1014, 0xff01, 64, 20)]
        {
            let ctx = simulate(Vec::new(), coll(pid, page, out_len, in_len));
            assert!(ctx.out.is_empty());
            IO.with(|io| assert_eq!(io.borrow().opens, 0));
        }
        for pid in [0x1018, 0x102d] {
            let ctx = simulate(vec![reply(2, 0, 45)], coll(pid, 0xff0b, 64, 64));
            assert_eq!(ctx.out[0].id, format!("mchose:3837:{pid:04x}"));
        }
    }
    fn checksum(r: &mut [u8]) {
        r[24] = r[2..24].iter().fold(0, |a, b| a ^ b);
    }
    fn reply(link: u8, ch: u8, p: u8) -> Vec<u8> {
        let mut r = vec![0; 64];
        r[0] = 0x4d;
        r[1] = 1;
        r[2] = 1;
        r[3] = 16;
        r[5] = 9;
        r[8] = 0x37;
        r[9] = 0x38;
        r[10] = 0x26;
        r[11] = 0x40;
        r[18] = link;
        r[19] = ch;
        r[20] = p;
        checksum(&mut r);
        r
    }
    #[test]
    fn captured_and_malformed_frames() {
        assert_eq!(parse_info(&reply(2, 0, 29)), Some(State::Battery { percent: 29, charging: false }));
        assert_eq!(parse_info(&reply(2, 1, 100)), Some(State::Battery { percent: 100, charging: true }));
        assert_eq!(parse_info(&reply(0, 0, 29)), Some(State::Unreachable));
        let mut off = vec![0; 64];
        off[0] = 0x4d;
        off[1] = 1;
        off[2] = 1;
        off[3] = 1;
        off[5] = 9;
        off[8] = 0xff;
        assert_eq!(parse_info(&off), Some(State::Unreachable));
        off[5] = 0;
        assert_eq!(parse_info(&off), None);
        let mut r = reply(2, 0, 29);
        r[24] ^= 0xff;
        assert_eq!(parse_info(&r), None);
        assert_eq!(parse_info(&reply(2, 0, 101)), None);
        r = reply(2, 0, 29);
        r[8] = 0x34;
        checksum(&mut r);
        assert_eq!(parse_info(&r), None);
        r = reply(2, 0, 29);
        r[0] = 0x4e;
        assert_eq!(parse_info(&r), None);
        r = reply(2, 0, 29);
        r[3] = 12;
        checksum(&mut r);
        assert_eq!(parse_info(&r), Some(State::Unreachable));
        for n in 0..25 {
            assert_eq!(parse_info(&reply(2, 0, 29)[..n]), None);
        }
        assert_eq!(&request(64)[..9], &[0x4d, 1, 1, 0, 0, 9, 0, 0, 8]);
        assert!(request(64)[9..].iter().all(|b| *b == 0));
    }
    #[test]
    fn receiver_collection_filter() {
        for page in [0xff01, 0xff0b] {
            assert!(collection(&HidColl { page, out_len: 64, in_len: 64, ..Default::default() }));
        }
        for (page, out_len, in_len) in [(1, 64, 64), (0xff01, 63, 64), (0xff01, 64, 20)] {
            assert!(!collection(&HidColl { page, out_len, in_len, ..Default::default() }));
        }
        assert!(!PIDS.contains(&0x1012));
        assert!(PIDS.contains(&0x1018));
        assert!(PIDS.contains(&0x102d));
    }
}
