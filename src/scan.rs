//! Runs the drivers: HID enumeration cache, per-driver intervals and panic isolation.

use crate::device::Observation;
use crate::drivers::{self, Added, Driver, PollCtx};
use crate::hid::{self, HidColl};
use crate::{diag, settings};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::{Duration, Instant};

/// Re-enumerate HID at least this often even without a device-change notification.
const HID_REFRESH: Duration = Duration::from_secs(300);
/// Drivers slower than this get a diagnostics line.
const SLOW_POLL: Duration = Duration::from_millis(300);

struct Slot {
    driver: Box<dyn Driver>,
    last: Option<Instant>,
    added: Added,
    /// Set after a panic: the driver's state can't be trusted any more.
    disabled: bool,
}

pub struct Scanner {
    slots: Vec<Slot>,
    colls: Vec<HidColl>,
    enumerated: Option<Instant>,
}

impl Default for Scanner {
    fn default() -> Self {
        Self::with_drivers(drivers::all())
    }
}

impl Scanner {
    fn with_drivers(drivers: Vec<Box<dyn Driver>>) -> Self {
        let slots = drivers
            .into_iter()
            .map(|driver| Slot { driver, last: None, added: Added::default(), disabled: false })
            .collect();
        Self { slots, colls: Vec::new(), enumerated: None }
    }

    /// One scan. `forced` (startup, device change, resume, "Refresh now") re-enumerates HID
    /// and polls every driver; otherwise only drivers whose interval elapsed are polled.
    pub fn scan(&mut self, forced: bool) -> Vec<Observation> {
        let now = Instant::now();
        if forced || self.enumerated.is_none_or(|t| now.duration_since(t) >= HID_REFRESH) {
            self.colls = hid::enumerate(&settings::ignored_devices());
            self.enumerated = Some(now);
        }
        run(&mut self.slots, &self.colls, forced, now)
    }

    /// The collections from the last enumeration (for `--dump`).
    pub fn collections(&self) -> &[HidColl] {
        &self.colls
    }
}

fn run(slots: &mut [Slot], colls: &[HidColl], forced: bool, now: Instant) -> Vec<Observation> {
    let mut ctx = PollCtx::new(colls);
    for slot in slots.iter_mut().filter(|s| !s.disabled) {
        let due = forced || slot.last.is_none_or(|t| now.duration_since(t) >= slot.driver.interval());
        if !due {
            ctx.replay(&slot.added);
            continue;
        }
        let mark = ctx.marks();
        let started = Instant::now();
        let result = catch_unwind(AssertUnwindSafe(|| slot.driver.poll(&mut ctx)));
        let name = slot.driver.name();
        match result {
            Ok(()) => {
                slot.added = ctx.added_since(mark);
                slot.last = Some(now);
                let took = started.elapsed();
                if took >= SLOW_POLL {
                    diag::event(&format!("scan:{name}"), format!("poll took {} ms", took.as_millis()));
                }
            }
            Err(_) => {
                ctx.truncate(mark);
                slot.disabled = true;
                diag::event(&format!("scan:{name}"), "driver panicked; disabled until restart");
            }
        }
    }
    ctx.out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::Kind;
    use std::cell::Cell;
    use std::rc::Rc;

    struct Fake {
        polls: Rc<Cell<u32>>,
        panic: bool,
    }

    impl Driver for Fake {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn poll(&mut self, ctx: &mut PollCtx) {
            self.polls.set(self.polls.get() + 1);
            ctx.add(Observation::battery("fake:1:1", "Fake", Kind::Mouse, 42, false), 1, 1);
            assert!(!self.panic, "boom");
        }
    }

    fn slot(polls: &Rc<Cell<u32>>, panic: bool) -> Slot {
        Slot {
            driver: Box::new(Fake { polls: polls.clone(), panic }),
            last: None,
            added: Added::default(),
            disabled: false,
        }
    }

    #[test]
    fn replays_between_intervals_and_polls_when_due() {
        let polls = Rc::new(Cell::new(0));
        let mut slots = vec![slot(&polls, false)];
        let t0 = Instant::now();
        assert_eq!(run(&mut slots, &[], false, t0).len(), 1);
        assert_eq!(run(&mut slots, &[], false, t0 + Duration::from_secs(5)).len(), 1);
        assert_eq!(polls.get(), 1, "not due yet: replayed");
        run(&mut slots, &[], false, t0 + drivers::NORMAL);
        assert_eq!(polls.get(), 2);
        run(&mut slots, &[], true, t0 + drivers::NORMAL);
        assert_eq!(polls.get(), 3, "forced scans poll everything");
    }

    #[test]
    fn panicking_driver_is_dropped_and_disabled() {
        let polls = Rc::new(Cell::new(0));
        let mut slots = vec![slot(&polls, true)];
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let out = run(&mut slots, &[], true, Instant::now());
        std::panic::set_hook(prev);
        assert!(out.is_empty(), "partial output of a panicking driver is discarded");
        assert!(slots[0].disabled);
        run(&mut slots, &[], true, Instant::now());
        assert_eq!(polls.get(), 1);
    }
}
