//! Source-independent device model, and the tracker that merges scans over time.

use std::collections::HashSet;
use std::time::{Duration, Instant};

/// Also the default display order on the taskbar.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Kind {
    Headset,
    Earbuds,
    Mouse,
    Keyboard,
    Gamepad,
    Phone,
    Other,
}

impl Kind {
    /// Best guess from a product name, for sources that don't report a type.
    pub fn from_name(name: &str) -> Kind {
        let n = name.to_ascii_lowercase();
        let has = |words: &[&str]| words.iter().any(|w| n.contains(w));
        if has(&["headset", "headphone", "airpods max", "arctis", "cloud", "void", "virtuoso", "wh-"]) {
            Kind::Headset
        } else if has(&["earbud", "buds", "airpods", "earphone", "in-ear", "wf-", "pods"]) {
            Kind::Earbuds
        } else if has(&["mouse", "mice", "trackball"]) {
            Kind::Mouse
        } else if has(&["keyboard", "keys"]) {
            Kind::Keyboard
        } else if has(&["controller", "gamepad", "xbox", "dualsense", "dualshock", "joy-con"]) {
            Kind::Gamepad
        } else {
            Kind::Other
        }
    }
}

/// What a source saw for one device during one scan.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum State {
    Battery {
        percent: u8,
        charging: bool,
    },
    /// Known to exist but didn't answer (asleep, or off; often indistinguishable).
    Unreachable,
    /// Charging, but the level is unknown right now (e.g. on its cable): keep the last level
    /// dimmed and show it charging.
    ChargingUnknown,
    /// Definitely not connected (e.g. headset off while its dongle is plugged).
    Off,
}

/// One device as seen by one driver during one scan. Build with [`Observation::battery`] or
/// [`Observation::new`] and the chained setters.
#[derive(Clone, Debug, PartialEq)]
pub struct Observation {
    /// Stable per device across scans: `"<driver>:<vid>:<pid>[:<extra>]"` in lowercase hex,
    /// e.g. `"logi:c539:2:01"`, `"razer:1532:00b7"`, `"bt:f4ee25fbba46"`.
    pub id: String,
    /// None when the source can't identify the device right now (only updates known devices).
    pub name: Option<String>,
    pub kind: Option<Kind>,
    pub state: State,
    /// How it's connected: "2.4 GHz receiver", "USB cable", "Bluetooth"... Empty if unknown.
    pub via: &'static str,
    /// Extra breakdown for the tooltip, e.g. "L 80%, R 75%, case 40%".
    pub detail: Option<String>,
    /// Coarse level label for devices that only report steps ("Low", "Full"...).
    pub level_text: Option<&'static str>,
    pub millivolts: Option<u16>,
    /// Keep showing the last reading (dimmed) this long after the device stops appearing in
    /// scans; for receivers that drop sleeping devices from the link. Zero = no grace.
    pub grace: Duration,
}

impl Observation {
    pub fn new(id: impl Into<String>, state: State) -> Self {
        Self {
            id: id.into(),
            name: None,
            kind: None,
            state,
            via: "",
            detail: None,
            level_text: None,
            millivolts: None,
            grace: Duration::ZERO,
        }
    }

    pub fn battery(id: impl Into<String>, name: impl Into<String>, kind: Kind, percent: u8, charging: bool) -> Self {
        Self::new(id, State::Battery { percent: percent.min(100), charging }).name(name).kind(kind)
    }

    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
    pub fn kind(mut self, kind: Kind) -> Self {
        self.kind = Some(kind);
        self
    }
    pub fn via(mut self, via: &'static str) -> Self {
        self.via = via;
        self
    }
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
    pub fn level_text(mut self, text: &'static str) -> Self {
        self.level_text = Some(text);
        self
    }
    pub fn millivolts(mut self, mv: u16) -> Self {
        self.millivolts = Some(mv);
        self
    }
    pub fn grace(mut self, grace: Duration) -> Self {
        self.grace = grace;
        self
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Status {
    /// `stale`: last known reading of a device that's asleep right now.
    Battery {
        percent: u8,
        charging: bool,
        stale: bool,
    },
    NotConnected,
}

#[derive(Clone, Debug)]
pub struct Reading {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub status: Status,
    pub via: &'static str,
    pub detail: Option<String>,
    pub level_text: Option<&'static str>,
    pub millivolts: Option<u16>,
    grace: Duration,
    last_seen: Instant,
}

impl Reading {
    /// A standalone reading (demo mode, tests); scans build theirs through [`Tracker`].
    pub fn new(id: impl Into<String>, name: impl Into<String>, kind: Kind, status: Status) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            kind,
            status,
            via: "",
            detail: None,
            level_text: None,
            millivolts: None,
            grace: Duration::ZERO,
            last_seen: Instant::now(),
        }
    }
}

/// Remembers every device seen this session, so a device that disappears shows as
/// "not connected" and a sleeping one keeps its last battery reading.
#[derive(Default)]
pub struct Tracker {
    known: Vec<Reading>,
}

impl Tracker {
    pub fn update(&mut self, observations: Vec<Observation>) -> Vec<Reading> {
        self.update_at(observations, Instant::now())
    }

    fn update_at(&mut self, observations: Vec<Observation>, now: Instant) -> Vec<Reading> {
        let mut seen = HashSet::new();
        for o in observations {
            seen.insert(o.id.clone());
            let pos = self.known.iter().position(|r| r.id == o.id);
            let status = match (o.state, pos.map(|p| self.known[p].status)) {
                (State::Battery { percent, charging }, _) => Status::Battery { percent, charging, stale: false },
                // Asleep: battery hasn't changed, keep the last reading but mark it.
                (State::Unreachable, Some(Status::Battery { percent, charging, .. })) => {
                    Status::Battery { percent, charging, stale: true }
                }
                (State::ChargingUnknown, Some(Status::Battery { percent, .. })) => {
                    Status::Battery { percent, charging: true, stale: true }
                }
                (State::Unreachable | State::ChargingUnknown | State::Off, _) => Status::NotConnected,
            };
            match pos {
                Some(p) => {
                    let r = &mut self.known[p];
                    r.status = status;
                    r.last_seen = now;
                    r.grace = o.grace;
                    if let Some(name) = o.name {
                        r.name = name;
                    }
                    if let Some(kind) = o.kind {
                        r.kind = kind;
                    }
                    if !o.via.is_empty() {
                        r.via = o.via;
                    }
                    if matches!(o.state, State::Battery { .. }) {
                        r.detail = o.detail;
                        r.level_text = o.level_text;
                        r.millivolts = o.millivolts;
                    }
                }
                None => {
                    let Some(name) = o.name else { continue };
                    let kind = o.kind.unwrap_or_else(|| Kind::from_name(&name));
                    self.known.push(Reading {
                        id: o.id,
                        name,
                        kind,
                        status,
                        via: o.via,
                        detail: o.detail,
                        level_text: o.level_text,
                        millivolts: o.millivolts,
                        grace: o.grace,
                        last_seen: now,
                    });
                }
            }
        }
        for r in self.known.iter_mut().filter(|r| !seen.contains(&r.id)) {
            r.status = match r.status {
                Status::Battery { percent, charging, .. } if now.duration_since(r.last_seen) < r.grace => {
                    Status::Battery { percent, charging, stale: true }
                }
                _ => Status::NotConnected,
            };
        }
        self.known.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.name.cmp(&b.name)));
        self.known.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(state: State) -> Observation {
        Observation::new("x", state).name("Mouse").kind(Kind::Mouse)
    }

    #[test]
    fn kind_from_name() {
        assert_eq!(Kind::from_name("Renan's AirPods"), Kind::Earbuds);
        assert_eq!(Kind::from_name("AirPods Max"), Kind::Headset);
        assert_eq!(Kind::from_name("Galaxy Buds2 Pro"), Kind::Earbuds);
        assert_eq!(Kind::from_name("PRO X Wireless Gaming Headset"), Kind::Headset);
        assert_eq!(Kind::from_name("MX Master 3S Mouse"), Kind::Mouse);
        assert_eq!(Kind::from_name("HyperX Cloud III Wireless"), Kind::Headset);
        assert_eq!(Kind::from_name("Xbox Wireless Controller"), Kind::Gamepad);
        assert_eq!(Kind::from_name("Keychron Nape Pro trackball"), Kind::Mouse);
    }

    #[test]
    fn charging_with_unknown_level_keeps_last_level() {
        let mut t = Tracker::default();
        t.update(vec![obs(State::Battery { percent: 30, charging: false })]);
        let r = t.update(vec![obs(State::ChargingUnknown)]);
        assert_eq!(r[0].status, Status::Battery { percent: 30, charging: true, stale: true });
    }

    #[test]
    fn sleeping_device_keeps_last_reading_dimmed() {
        let mut t = Tracker::default();
        t.update(vec![obs(State::Battery { percent: 80, charging: false })]);
        let r = t.update(vec![obs(State::Unreachable)]);
        assert_eq!(r[0].status, Status::Battery { percent: 80, charging: false, stale: true });
    }

    #[test]
    fn vanished_device_is_not_connected() {
        let mut t = Tracker::default();
        t.update(vec![obs(State::Battery { percent: 80, charging: false })]);
        assert_eq!(t.update(vec![])[0].status, Status::NotConnected);
    }

    #[test]
    fn vanished_device_keeps_reading_during_grace() {
        let mut t = Tracker::default();
        let start = Instant::now();
        let o = obs(State::Battery { percent: 60, charging: false }).grace(Duration::from_secs(1800));
        t.update_at(vec![o], start);
        let r = t.update_at(vec![], start + Duration::from_secs(600));
        assert_eq!(r[0].status, Status::Battery { percent: 60, charging: false, stale: true });
        let r = t.update_at(vec![], start + Duration::from_secs(1801));
        assert_eq!(r[0].status, Status::NotConnected);
    }

    #[test]
    fn unknown_unreachable_device_is_ignored() {
        let mut t = Tracker::default();
        let mut o = obs(State::Unreachable);
        o.name = None;
        assert!(t.update(vec![o]).is_empty());
    }

    #[test]
    fn battery_constructor_clamps_percent() {
        let o = Observation::battery("a", "A", Kind::Mouse, 140, true);
        assert_eq!(o.state, State::Battery { percent: 100, charging: true });
    }
}
