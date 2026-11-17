//! Device drivers and the context they share during one scan.
//!
//! Every driver looks at the enumerated HID collections (or its own source: Bluetooth, a
//! serial port, a file...), talks to the devices it recognises and appends observations.
//! A driver *claims* each (vid, pid) it handled, so drivers further down the list, and the
//! generic fallbacks at the end, never report the same device twice.
//!
//! Rules every driver follows:
//! - Only read-only battery queries. Never write to a device it does not recognise by id.
//! - No panics on short or malformed replies: use `.get()`, never index unchecked bytes.
//! - Keep handles only as long as needed, except where the protocol pushes status reports
//!   (then keep the `HidDev` in the driver's state).

use crate::device::Observation;
use crate::hid::HidColl;
use std::time::Duration;

mod airpods;
mod areson;
mod astro;
mod atk_bitmouse;
mod atk_serial;
mod aula;
mod bluetooth;
mod coolermaster;
mod corsair;
mod dareu;
mod darmoshark;
mod eightbitdo;
mod endgame;
mod fantech;
mod finalmouse;
mod flydigi;
mod fractal;
mod galaxy_buds;
mod gamesir;
mod generic_hid;
mod glorious;
mod headsetcontrol;
mod hyperx;
mod jabra;
mod jbl;
mod keychron;
mod kysona;
mod lamzu;
pub mod logitech;
mod logitech_g435;
mod machenike;
mod mchose;
mod mchose_g3;
mod mchose_modern;
pub mod misc_mice;
mod moondrop;
mod msi;
mod ninjutso;
mod phonelink;
mod playstation;
mod pulsar;
mod rapoo;
mod razer;
mod razer_headset;
mod rog;
mod rongyuan;
mod royal_kludge;
mod sonix;
mod sony_inzone;
mod sprime;
mod steam_controller;
mod steelseries;
mod vaxee;
mod wlmouse;
mod xinput;
mod zowie;

/// Cheap, tested-safe sources (HID++, controllers that stream their state).
pub const FAST: Duration = Duration::from_secs(15);
/// Default: vendor queries, Bluetooth (reading wakes the radio), external processes.
pub const NORMAL: Duration = Duration::from_secs(60);

pub trait Driver {
    /// Short stable name for diagnostics, e.g. "razer".
    fn name(&self) -> &'static str;
    /// Minimum time between two polls. Between polls the scanner replays the driver's last
    /// observations and claims, so its devices don't flicker or leak to the fallbacks.
    fn interval(&self) -> Duration {
        NORMAL
    }
    fn poll(&mut self, ctx: &mut PollCtx);
}

/// A stateless driver: a plain function.
struct Simple {
    name: &'static str,
    interval: Duration,
    poll: fn(&mut PollCtx),
}

impl Driver for Simple {
    fn name(&self) -> &'static str {
        self.name
    }
    fn interval(&self) -> Duration {
        self.interval
    }
    fn poll(&mut self, ctx: &mut PollCtx) {
        (self.poll)(ctx)
    }
}

/// Wraps a stateless poll function polled every [`NORMAL`].
pub fn simple(name: &'static str, poll: fn(&mut PollCtx)) -> Box<dyn Driver> {
    simple_every(name, NORMAL, poll)
}

pub fn simple_every(name: &'static str, interval: Duration, poll: fn(&mut PollCtx)) -> Box<dyn Driver> {
    Box::new(Simple { name, interval, poll })
}

/// All drivers in poll order. Vendor drivers first; a driver that shares a vendor id with a
/// broader one must come before it. Fallbacks last.
pub fn all() -> Vec<Box<dyn Driver>> {
    vec![
        glorious::driver(),
        aula::driver(), // before kysona: shares vendor id 3554
        dareu::driver(),
        darmoshark::driver(),
        atk_serial::driver(),   // before kysona: shares the ATK vendor id
        pulsar::driver(),       // before kysona: shares vendor id 3710
        atk_bitmouse::driver(), // before kysona: NK dongle needs another protocol
        kysona::driver(),
        keychron::driver(),
        lamzu::driver(),
        endgame::driver(),
        vaxee::driver(),
        wlmouse::driver(),
        finalmouse::driver(),
        ninjutso::driver(),
        sprime::driver(),
        rongyuan::driver(),
        sonix::driver(),
        sony_inzone::driver(),
        fractal::driver(),
        jabra::driver(),
        jbl::driver(),
        rog::driver(),
        zowie::driver(),
        royal_kludge::driver(),
        mchose::driver(),
        rapoo::driver(),
        areson::driver(),
        mchose_g3::driver(),
        msi::driver(),
        machenike::driver(), // before misc_mice: shares vendor id 248A with Ajazz
        mchose_modern::driver(),
        misc_mice::driver(),
        fantech::driver(),
        razer_headset::driver(), // before razer: same vendor id
        hyperx::driver(),
        logitech_g435::driver(), // before logitech: same vendor id, not HID++
        astro::driver(),
        logitech::driver(),
        razer::driver(),
        steelseries::driver(),
        corsair::driver(),
        coolermaster::driver(),
        playstation::driver(),
        steam_controller::driver(),
        gamesir::driver(),
        flydigi::driver(),
        gamesir::cyclone_driver(),
        eightbitdo::driver(),
        airpods::driver(), // before bluetooth, which has no level for AirPods
        phonelink::driver(),
        galaxy_buds::driver(), // before bluetooth, which skips devices already listed by name
        moondrop::driver(),
        headsetcontrol::driver(),
        bluetooth::driver(), // before generic_hid: Windows' own level wins for the same device
        generic_hid::driver(),
        xinput::driver(),
    ]
}

/// Shared state of one scan.
pub struct PollCtx<'a> {
    /// Every present HID collection (ignored devices already removed).
    pub colls: &'a [HidColl],
    /// Observations so far, in driver order.
    pub out: Vec<Observation>,
    claimed: Vec<(u16, u16)>,
    /// Bluetooth container ids already reported, so generic HID skips the same device.
    bt_containers: Vec<String>,
}

impl<'a> PollCtx<'a> {
    pub fn new(colls: &'a [HidColl]) -> Self {
        Self { colls, out: Vec::new(), claimed: Vec::new(), bt_containers: Vec::new() }
    }

    pub fn is_claimed(&self, vid: u16, pid: u16) -> bool {
        self.claimed.contains(&(vid, pid))
    }

    pub fn claim(&mut self, vid: u16, pid: u16) {
        if !self.is_claimed(vid, pid) {
            self.claimed.push((vid, pid));
        }
    }

    /// Appends an observation and claims its device.
    pub fn add(&mut self, obs: Observation, vid: u16, pid: u16) {
        self.out.push(obs);
        self.claim(vid, pid);
    }

    /// Distinct unclaimed product ids present for a vendor, in ascending order.
    pub fn products(&self, vid: u16) -> Vec<(u16, u16)> {
        let mut v: Vec<(u16, u16)> = self
            .colls
            .iter()
            .filter(|c| c.vid == vid && !self.is_claimed(c.vid, c.pid))
            .map(|c| (c.vid, c.pid))
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// Collections of one product.
    pub fn find(&self, vid: u16, pid: u16) -> impl Iterator<Item = &'a HidColl> + 'a {
        self.colls.iter().filter(move |c| c.vid == vid && c.pid == pid)
    }

    pub fn has_bt_container(&self, id: &str) -> bool {
        self.bt_containers.iter().any(|c| c.eq_ignore_ascii_case(id))
    }

    pub fn add_bt_container(&mut self, id: impl Into<String>) {
        self.bt_containers.push(id.into());
    }

    /// Snapshot lengths, so the scanner can record what one driver added.
    pub(crate) fn marks(&self) -> (usize, usize, usize) {
        (self.out.len(), self.claimed.len(), self.bt_containers.len())
    }

    /// What was added since `marks`.
    pub(crate) fn added_since(&self, m: (usize, usize, usize)) -> Added {
        Added {
            out: self.out[m.0..].to_vec(),
            claimed: self.claimed[m.1..].to_vec(),
            bt_containers: self.bt_containers[m.2..].to_vec(),
        }
    }

    /// Re-applies a driver's previous output (driver not due this scan).
    pub(crate) fn replay(&mut self, a: &Added) {
        self.out.extend(a.out.iter().cloned());
        for &(v, p) in &a.claimed {
            self.claim(v, p);
        }
        self.bt_containers.extend(a.bt_containers.iter().cloned());
    }

    /// Drops what was added since `marks` (driver panicked mid-poll).
    pub(crate) fn truncate(&mut self, m: (usize, usize, usize)) {
        self.out.truncate(m.0);
        self.claimed.truncate(m.1);
        self.bt_containers.truncate(m.2);
    }
}

/// One driver's contribution to a scan.
#[derive(Default, Clone)]
pub(crate) struct Added {
    out: Vec<Observation>,
    claimed: Vec<(u16, u16)>,
    bt_containers: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{Kind, Observation};

    fn coll(vid: u16, pid: u16) -> HidColl {
        HidColl { vid, pid, ..Default::default() }
    }

    #[test]
    fn claims_hide_products_and_replay_restores_them() {
        let colls = [coll(0x1532, 0x00b7), coll(0x1532, 0x00b7), coll(0x1532, 0x0099), coll(0x046d, 0xc539)];
        let mut ctx = PollCtx::new(&colls);
        assert_eq!(ctx.products(0x1532), vec![(0x1532, 0x0099), (0x1532, 0x00b7)]);
        let m = ctx.marks();
        ctx.add(Observation::battery("razer:1532:00b7", "Viper", Kind::Mouse, 50, false), 0x1532, 0x00b7);
        assert_eq!(ctx.products(0x1532), vec![(0x1532, 0x0099)]);
        let added = ctx.added_since(m);

        let mut next = PollCtx::new(&colls);
        next.replay(&added);
        assert!(next.is_claimed(0x1532, 0x00b7));
        assert_eq!(next.out.len(), 1);
        next.truncate((0, 0, 0));
        assert!(next.out.is_empty() && !next.is_claimed(0x1532, 0x00b7));
    }
}
