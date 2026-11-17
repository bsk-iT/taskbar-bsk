use super::{simple, Driver, PollCtx};
use crate::device::{Kind, Observation};
use crate::hid::{HidColl, HidDev};
use std::time::{Duration, Instant};

pub fn driver() -> Box<dyn Driver> {
    simple("Astro", poll)
}

trait Io {
    fn flush(&self);
    fn write(&self, request: &[u8]) -> bool;
    fn read(&self, timeout_ms: u32) -> Option<Vec<u8>>;
}

impl Io for HidDev {
    fn flush(&self) {
        HidDev::flush(self);
    }
    fn write(&self, request: &[u8]) -> bool {
        HidDev::write(self, request, 500)
    }
    fn read(&self, timeout_ms: u32) -> Option<Vec<u8>> {
        HidDev::read(self, timeout_ms)
    }
}

fn matches(c: &HidColl) -> bool {
    c.vid == 0x046d
        && matches!(c.pid, 0x0b0b | 0x0b1c)
        && c.page == 0xff32
        && c.usage == 0x74
        && c.in_len == 64
        && c.out_len == 64
}

fn request() -> [u8; 64] {
    let mut out = [0; 64];
    out[0] = 2;
    out[1] = 12;
    out[2] = 3;
    out[4] = 6;
    out[5] = 12;
    out
}

fn parse_battery(reply: &[u8]) -> Option<(u8, bool)> {
    let &[2, 12, len, 0, 6, status, level, _, dock, ..] = reply else { return None };
    (len >= 6 && usize::from(len) + 3 <= reply.len() && matches!(status, 0 | 12) && level <= 100)
        .then_some((level, dock != 0))
}

fn poll(ctx: &mut PollCtx) {
    poll_with(ctx, HidDev::open);
}

fn poll_with<D: Io>(ctx: &mut PollCtx, mut open: impl FnMut(&HidColl) -> Option<D>) {
    for c in ctx.colls {
        if !matches(c) || ctx.is_claimed(c.vid, c.pid) {
            continue;
        }
        let Some(d) = open(c) else { continue };
        d.flush();
        // No payload: only read battery; never change audio or headset settings.
        if !d.write(&request()) {
            continue;
        }
        let deadline = Instant::now() + Duration::from_millis(1000);
        for _ in 0..8 {
            let left = deadline.saturating_duration_since(Instant::now()).as_millis() as u32;
            if left == 0 {
                break;
            }
            let Some(reply) = d.read(left) else { break };
            let Some((percent, charging)) = parse_battery(&reply) else { continue };
            let name = if c.pid == 0x0b0b { "ASTRO A50 X" } else { "ASTRO A50 Gen 5" };
            let id = format!(
                "astro:{:04x}:{:04x}:{}",
                c.vid,
                c.pid,
                String::from_utf16_lossy(&c.path).trim_end_matches('\0')
            );
            ctx.add(
                Observation::battery(id, name, Kind::Headset, percent, charging).via("USB base station"),
                c.vid,
                c.pid,
            );
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery_replies() {
        assert_eq!(parse_battery(&[2, 12, 6, 0, 6, 12, 99, 100, 0]), Some((99, false)));
        assert_eq!(parse_battery(&[2, 12, 6, 0, 6, 12, 0, 100, 1]), Some((0, true)));
        assert_eq!(parse_battery(&[2, 12, 6, 0, 6, 0, 100, 100, 0]), Some((100, false)));
        assert_eq!(parse_battery(&[2, 12, 6, 0, 8, 12, 50, 100, 0]), None);
        for field in 0..7 {
            let mut bad = [2, 12, 6, 0, 6, 12, 50, 100, 0];
            bad[field] = 255;
            assert_eq!(parse_battery(&bad), None);
        }
        assert_eq!(parse_battery(&[2, 12, 6, 0, 6, 12, 50]), None);
        assert_eq!(parse_battery(&[]), None);
    }

    #[test]
    fn read_only_request() {
        let mut expected = [0; 64];
        expected[..6].copy_from_slice(&[2, 12, 3, 0, 6, 12]);
        assert_eq!(request(), expected);
    }

    #[test]
    fn collections() {
        let mut c = HidColl {
            path: vec![],
            vid: 0x046d,
            pid: 0x0b0b,
            iface: 0,
            page: 0xff32,
            usage: 0x74,
            in_len: 64,
            out_len: 64,
            feat_len: 0,
        };
        assert!(matches(&c));
        c.pid = 0x0b1c;
        assert!(matches(&c));
        for field in 0..5 {
            let mut bad = c.clone();
            match field {
                0 => bad.vid = 0x1234,
                1 => bad.pid = 0x1234,
                2 => bad.page = 0xff00,
                3 => bad.usage = 1,
                _ => bad.out_len = 20,
            }
            assert!(!matches(&bad));
        }
    }
}

#[cfg(test)]
mod polling_tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

    struct Fake<'a> {
        replies: &'a RefCell<VecDeque<Vec<u8>>>,
        write_ok: bool,
    }
    impl Io for Fake<'_> {
        fn flush(&self) {}
        fn write(&self, data: &[u8]) -> bool {
            let mut expected = [0; 64];
            expected[..6].copy_from_slice(&[2, 12, 3, 0, 6, 12]);
            assert_eq!(data, expected);
            self.write_ok
        }
        fn read(&self, timeout_ms: u32) -> Option<Vec<u8>> {
            assert!(timeout_ms > 0 && timeout_ms <= 1000);
            self.replies.borrow_mut().pop_front()
        }
    }

    fn collection() -> HidColl {
        HidColl { vid: 0x046d, pid: 0x0b0b, page: 0xff32, usage: 0x74, in_len: 64, out_len: 64, ..Default::default() }
    }

    fn reply(level: u8, dock: u8) -> Vec<u8> {
        vec![2, 12, 6, 0, 6, 12, level, 100, dock]
    }

    #[test]
    fn cpp_polling_contract() {
        let opens = Cell::new(0);
        let open_ok = Cell::new(true);
        let write_ok = Cell::new(true);
        let run = |data: Vec<Vec<u8>>, c: HidColl| {
            let data = RefCell::new(VecDeque::from(data));
            let colls = [c];
            let mut ctx = PollCtx::new(&colls);
            let mut open = |_: &HidColl| {
                opens.set(opens.get() + 1);
                open_ok.get().then_some(Fake { replies: &data, write_ok: write_ok.get() })
            };
            poll_with(&mut ctx, &mut open);
            let claimed = ctx.is_claimed(0x046d, colls[0].pid);
            if claimed {
                let before = opens.get();
                poll_with(&mut ctx, &mut open);
                assert_eq!(opens.get(), before);
            }
            (ctx.out, claimed)
        };
        let (out, claimed) = run(vec![reply(99, 0)], collection());
        assert!(claimed);
        assert_eq!(out[0].name.as_deref(), Some("ASTRO A50 X"));
        assert_eq!(out[0].kind, Some(Kind::Headset));
        assert!(matches!(out[0].state, crate::device::State::Battery { percent: 99, charging: false }));
        let (out, _) = run(vec![reply(0, 1)], collection());
        assert!(matches!(out[0].state, crate::device::State::Battery { percent: 0, charging: true }));
        let mut push = reply(100, 0);
        push[5] = 0;
        assert!(!run(vec![push], collection()).0.is_empty());
        let mut unrelated = reply(50, 0);
        unrelated[4] = 8;
        let (out, _) = run(vec![unrelated, reply(60, 0)], collection());
        assert!(matches!(out[0].state, crate::device::State::Battery { percent: 60, .. }));
        for field in 0..7 {
            let mut bad = reply(50, 0);
            bad[field] = 255;
            let (out, claimed) = run(vec![bad], collection());
            assert!(out.is_empty() && !claimed);
        }
        assert!(run(vec![vec![2, 12, 6, 0, 6, 12, 50]], collection()).0.is_empty());
        assert!(run(vec![], collection()).0.is_empty());
        let mut c = collection();
        c.pid = 0x0b1c;
        assert_eq!(run(vec![reply(75, 0)], c).0[0].name.as_deref(), Some("ASTRO A50 Gen 5"));
        for field in 0..5 {
            let mut c = collection();
            match field {
                0 => c.vid = 0x1234,
                1 => c.pid = 0x1234,
                2 => c.page = 0xff00,
                3 => c.usage = 1,
                _ => c.out_len = 20,
            }
            let before = opens.get();
            assert!(run(vec![reply(50, 0)], c).0.is_empty());
            assert_eq!(opens.get(), before);
        }
        open_ok.set(false);
        assert!(run(vec![reply(50, 0)], collection()).0.is_empty());
        open_ok.set(true);
        write_ok.set(false);
        assert!(run(vec![reply(50, 0)], collection()).0.is_empty());
    }
}
