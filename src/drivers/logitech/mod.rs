//! Logitech HID++ 1.0/2.0 receivers, direct USB/Bluetooth devices and G headsets.
mod curve;
pub mod hidpp;

use super::{Driver, PollCtx, FAST};
use crate::device::{Kind, Observation, State};
use hidpp::{Endpoint, Error, Hidpp, Link, ERR_UNKNOWN_DEVICE};
use std::collections::HashMap;
use std::time::Duration;

const RECEIVER_PIDS: &[u16] = &[
    0xc517, 0xc518, 0xc51a, 0xc51b, 0xc521, 0xc525, 0xc526, 0xc52b, 0xc52e, 0xc52f, 0xc531, 0xc532, 0xc534, 0xc537,
    0xc539, 0xc53a, 0xc53d, 0xc53f, 0xc541, 0xc545, 0xc547, 0xc548, 0xc54d, 0xc54f,
];
const DEVICE_FW_VERSION: u16 = 0x0003;
const DEVICE_NAME: u16 = 0x0005;
const BATTERY_STATUS: u16 = 0x1000;
const BATTERY_VOLTAGE: u16 = 0x1001;
const UNIFIED_BATTERY: u16 = 0x1004;
const ADC_MEASUREMENT: u16 = 0x1f20;

enum Raw {
    Percent { percent: u8, charging: bool, feature: u16 },
    Millivolts { mv: u16, charging: bool, feature: u16 },
    Level { percent: u8, charging: bool, text: &'static str },
    Unknown,
    NoBattery,
}

#[derive(Clone)]
struct Identity {
    name: String,
    kind: Kind,
    pids: Vec<u16>,
    unit: Option<String>,
}

pub struct Logitech {
    curves: curve::Curves,
    identities: HashMap<String, Identity>,
}

pub fn driver() -> Box<dyn Driver> {
    Box::new(Logitech::new())
}

impl Driver for Logitech {
    fn name(&self) -> &'static str {
        "Logitech"
    }
    fn interval(&self) -> Duration {
        FAST
    }
    fn poll(&mut self, ctx: &mut PollCtx) {
        let mut claims = Vec::new();
        for ep in hidpp::endpoints(ctx.colls) {
            if ctx.is_claimed(hidpp::LOGITECH_VID, ep.pid) {
                continue;
            }
            let Ok(link) = Link::open(&ep) else { continue };
            let indices = indices(&link, &ep);
            let mut any = false;
            for &index in indices {
                let receiver = index != 0xff;
                let result = match ping(&link, index) {
                    Ok(r) if r[0] >= 2 => self.probe(&link, &ep, index, receiver),
                    Err(Error::Protocol(1)) => legacy_battery(&link, index).map(|raw| {
                        let name = fallback_name(&ep, receiver);
                        let kind = Kind::from_name(&name);
                        observation(fallback_id(&ep, index), name, kind, raw, receiver)
                    }),
                    _ => None,
                };
                // A direct (Bluetooth/USB) device that answered before but is asleep now: keep
                // its reading dimmed and claim it, so the Bluetooth fallback doesn't list it
                // again under its Bluetooth name.
                let result = result.or_else(|| if receiver { None } else { self.asleep(&ep, index) });
                if let Some(result) = result {
                    ctx.out.push(result);
                    any = true;
                }
            }
            if any && !claims.contains(&ep.pid) {
                claims.push(ep.pid);
            }
        }
        // Identical receivers share a PID but must all be polled before claiming it.
        for pid in claims {
            ctx.claim(hidpp::LOGITECH_VID, pid);
        }
    }
}

impl Logitech {
    pub fn new() -> Self {
        Self { curves: curve::Curves::load(), identities: HashMap::new() }
    }

    fn asleep(&self, ep: &Endpoint, index: u8) -> Option<Observation> {
        let key = fallback_id(ep, index);
        let identity = self.identities.get(&key)?;
        let id = identity.unit.as_ref().map(|unit| format!("logi:unit:{unit}")).unwrap_or(key);
        Some(Observation::new(id, State::Unreachable))
    }

    fn probe(&mut self, link: &dyn Hidpp, ep: &Endpoint, index: u8, receiver: bool) -> Option<Observation> {
        let key = fallback_id(ep, index);
        let raw = match battery(link, index) {
            Ok(Raw::NoBattery) => return None,
            Ok(raw) => raw,
            Err(Error::Protocol(ERR_UNKNOWN_DEVICE)) if receiver => return None,
            Err(_) => return None,
        };
        if !self.identities.contains_key(&key) {
            self.identities.insert(key.clone(), identify(link, index, ep, receiver));
        }
        let identity = self.identities.get(&key)?;
        let id = identity.unit.as_ref().map(|unit| format!("logi:unit:{unit}")).unwrap_or(key);
        let mut out = match raw {
            Raw::Millivolts { mv, charging, feature } => {
                let percent = self.curves.percent(&identity.pids, mv, (feature == ADC_MEASUREMENT).then_some(ep.pid));
                Observation::battery(id, identity.name.clone(), identity.kind, percent, charging).millivolts(mv)
            }
            raw => observation(id, identity.name.clone(), identity.kind, raw, receiver),
        }
        .via(if receiver { "Logitech receiver" } else { "USB / Bluetooth" });
        if receiver {
            out = out.grace(Duration::from_secs(1800));
        } else if identity.unit.is_some() {
            out = out.grace(Duration::from_secs(60));
        }
        Some(out)
    }
}

fn fallback_id(ep: &Endpoint, index: u8) -> String {
    format!("logi:046d:{:04x}{}:{index}", ep.pid, ep.suffix)
}

fn fallback_name(ep: &Endpoint, receiver: bool) -> String {
    if receiver || ep.product.is_empty() {
        "Logitech wireless device".into()
    } else {
        ep.product.clone()
    }
}

fn ping(link: &dyn Hidpp, index: u8) -> Result<[u8; 16], Error> {
    link.raw_request(index, 0, 0x1d, &[0, 0, 0x5a], false, 400)
}

fn indices(link: &dyn Hidpp, ep: &Endpoint) -> &'static [u8] {
    // C54F answers at FF as a battery-less dongle; its paired mouse lives at index 1.
    if ep.pid == 0xc54f {
        return &[1];
    }
    let path = ep
        .long
        .as_ref()
        .or(ep.short.as_ref())
        .map(|c| String::from_utf16_lossy(&c.path).to_ascii_lowercase())
        .unwrap_or_default();
    let bluetooth = path.contains("{00001812-") || path.contains("{00001124-");
    let receiver = RECEIVER_PIDS.contains(&ep.pid) || ep.product.to_ascii_lowercase().contains("receiver");
    if bluetooth || (!receiver && ping(link, 0xff).is_ok_and(|r| r[0] >= 2)) {
        &[0xff]
    } else {
        &[1, 2, 3, 4, 5, 6]
    }
}

fn observation(id: String, name: String, kind: Kind, raw: Raw, receiver: bool) -> Observation {
    let (state, text) = match raw {
        Raw::Percent { percent, charging, feature } => {
            crate::diag::event("logitech", format!("battery feature 0x{feature:04x}"));
            (State::Battery { percent, charging }, None)
        }
        Raw::Level { percent, charging, text } => (State::Battery { percent, charging }, Some(text)),
        _ => (State::Unreachable, None),
    };
    let mut out = Observation::new(id, state).name(name).kind(kind).via(if receiver {
        "Logitech receiver"
    } else {
        "USB / Bluetooth"
    });
    if receiver {
        out = out.grace(Duration::from_secs(1800));
    }
    if let Some(text) = text {
        out = out.level_text(text);
    }
    out
}

fn parse_battery(feature: u16, r: &[u8]) -> Option<Raw> {
    let &[a, b, flags, ..] = r else { return None };
    Some(match feature {
        UNIFIED_BATTERY => {
            let charging = matches!(flags, 1..=3);
            if (1..=100).contains(&a) {
                Raw::Percent { percent: a, charging, feature }
            } else if b != 0 {
                let (percent, text) = if b & 8 != 0 {
                    (100, "Full")
                } else if b & 4 != 0 {
                    (60, "Good")
                } else if b & 2 != 0 {
                    (20, "Low")
                } else {
                    (5, "Critical")
                };
                Raw::Level { percent, charging, text }
            } else {
                Raw::Unknown
            }
        }
        BATTERY_STATUS => {
            if a > 100 {
                Raw::Unknown
            } else {
                Raw::Percent {
                    percent: if a == 0 && flags == 3 { 100 } else { a },
                    charging: matches!(flags, 1..=4),
                    feature,
                }
            }
        }
        BATTERY_VOLTAGE | ADC_MEASUREMENT => {
            let mv = u16::from_be_bytes([a, b]);
            let charging = if feature == ADC_MEASUREMENT { flags == 3 } else { flags & 0x80 != 0 };
            if mv == 0 {
                Raw::Unknown
            } else {
                Raw::Millivolts { mv, charging, feature }
            }
        }
        _ => return None,
    })
}

fn battery(link: &dyn Hidpp, index: u8) -> Result<Raw, Error> {
    let mut error = None;
    for (feature, function) in [(UNIFIED_BATTERY, 1), (BATTERY_STATUS, 0), (BATTERY_VOLTAGE, 0), (ADC_MEASUREMENT, 0)] {
        match link.feature_index(index, feature) {
            Ok(Some(f)) => match link.request(index, f, function, &[]) {
                Ok(r) => {
                    if let Some(raw) = parse_battery(feature, &r) {
                        return Ok(raw);
                    }
                }
                Err(e) => error = Some(e),
            },
            Ok(None) => {}
            Err(e) => error = Some(e),
        }
    }
    match error {
        Some(e) => Err(e),
        None => Ok(Raw::NoBattery),
    }
}

fn parse_legacy(register: u8, r: &[u8]) -> Option<Raw> {
    match register {
        0x0d => {
            let &[percent, _, status, ..] = r else { return None };
            (1..=100).contains(&percent).then_some(Raw::Percent {
                percent,
                charging: (0x21..=0x26).contains(&status),
                feature: 0x0d,
            })
        }
        7 => {
            let &[level, status, ..] = r else { return None };
            if level == 0 {
                return None;
            }
            let percent = [0, 5, 10, 20, 35, 50, 75, 100].get(usize::from(level.min(7))).copied()?;
            let text = if level >= 7 {
                "Full"
            } else if level >= 5 {
                "Good"
            } else if level >= 3 {
                "Low"
            } else {
                "Critical"
            };
            Some(Raw::Level { percent, charging: (0x21..=0x26).contains(&status), text })
        }
        _ => None,
    }
}

fn legacy_battery(link: &dyn Hidpp, index: u8) -> Option<Raw> {
    for register in [0x0d, 7] {
        if let Ok(r) = link.raw_request(index, 0x81, register, &[], true, 500) {
            if let Some(raw) = parse_legacy(register, &r) {
                return Some(raw);
            }
        }
    }
    None
}

fn identify(link: &dyn Hidpp, index: u8, ep: &Endpoint, receiver: bool) -> Identity {
    let mut name = None;
    let mut kind = None;
    if let Ok(Some(f)) = link.feature_index(index, DEVICE_NAME) {
        if let Ok(r) = link.request(index, f, 0, &[]) {
            let len = usize::from(r[0]);
            let mut bytes = Vec::with_capacity(len);
            while bytes.len() < len {
                let Ok(chunk) = link.request(index, f, 1, &[bytes.len() as u8]) else { break };
                let before = bytes.len();
                let take = (len - before).min(link.payload_capacity());
                bytes.extend(chunk.iter().take(take).take_while(|b| **b != 0));
                if bytes.len() == before || bytes.len() - before < take {
                    break;
                }
            }
            let s = String::from_utf8_lossy(&bytes).trim().to_string();
            if !s.is_empty() {
                name = Some(if s.starts_with("Logitech") || s.starts_with("G ") || s.starts_with("MX") {
                    s
                } else {
                    format!("Logitech {s}")
                });
            }
        }
        if let Ok(r) = link.request(index, f, 2, &[]) {
            kind = Some(match r[0] {
                0 | 2 => Kind::Keyboard,
                3..=5 => Kind::Mouse,
                8 => Kind::Headset,
                11 | 12 => Kind::Gamepad,
                _ => Kind::Other,
            });
        }
    }
    let mut pids = if receiver { Vec::new() } else { vec![ep.pid] };
    let mut unit = None;
    if let Ok(Some(f)) = link.feature_index(index, DEVICE_FW_VERSION) {
        if let Ok(r) = link.request(index, f, 0, &[]) {
            if let Some(ids) = r.get(..link.payload_capacity()).and_then(|r| r.get(7..13)) {
                for &[hi, lo] in ids.as_chunks::<2>().0 {
                    let pid = u16::from_be_bytes([hi, lo]);
                    if pid != 0 {
                        pids.push(pid);
                    }
                }
            }
            if let Some(bytes) =
                r.get(..link.payload_capacity()).and_then(|r| r.get(1..5)).filter(|bytes| bytes.iter().any(|b| *b != 0))
            {
                unit = Some(bytes.iter().map(|b| format!("{b:02x}")).collect());
            }
        }
    }
    let name = name.unwrap_or_else(|| fallback_name(ep, receiver));
    let mut kind = kind.unwrap_or_else(|| Kind::from_name(&name));
    if kind == Kind::Other && ep.long.as_ref().is_some_and(|c| c.page == 0xff43) {
        kind = Kind::Headset;
    }
    Identity { name, kind, pids, unit }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Clone)]
    enum Reply {
        Ok(Vec<u8>),
        Timeout,
        Protocol(u8),
    }

    /// Scripted device: feature table (id -> index) plus canned replies per (index, function).
    struct Fake {
        features: HashMap<u16, u8>,
        replies: HashMap<(u8, u8), Reply>,
    }

    impl Fake {
        fn new(features: &[(u16, u8)]) -> Self {
            Self { features: features.iter().copied().collect(), replies: HashMap::new() }
        }
        fn reply(mut self, feature_idx: u8, function: u8, r: Reply) -> Self {
            self.replies.insert((feature_idx, function), r);
            self
        }
    }

    impl Hidpp for Fake {
        fn request(&self, _index: u8, feature_idx: u8, function: u8, params: &[u8]) -> Result<[u8; 16], Error> {
            let pad = |v: &[u8]| {
                let mut out = [0u8; 16];
                out[..v.len()].copy_from_slice(v);
                out
            };
            if feature_idx == 0 {
                let id = u16::from_be_bytes([params[0], params[1]]);
                return Ok(pad(&[self.features.get(&id).copied().unwrap_or(0)]));
            }
            match self.replies.get(&(feature_idx, function)).cloned().unwrap_or(Reply::Timeout) {
                Reply::Ok(v) => Ok(pad(&v)),
                Reply::Timeout => Err(Error::Timeout),
                Reply::Protocol(c) => Err(Error::Protocol(c)),
            }
        }

        fn raw_request(
            &self,
            _index: u8,
            b2: u8,
            b3: u8,
            _params: &[u8],
            _short: bool,
            _timeout_ms: u32,
        ) -> Result<[u8; 16], Error> {
            match self.replies.get(&(b2, b3)) {
                Some(Reply::Ok(v)) => {
                    let mut out = [0; 16];
                    for (dest, src) in out.iter_mut().zip(v) {
                        *dest = *src;
                    }
                    Ok(out)
                }
                Some(Reply::Protocol(c)) => Err(Error::Protocol(*c)),
                _ => Err(Error::Timeout),
            }
        }
    }

    fn raw(r: Result<Raw, Error>) -> Raw {
        r.unwrap_or_else(|e| panic!("unexpected error: {e}"))
    }

    // G733-style headset: only 0x1F20 (ADC measurement), at feature index 8 like HeadsetControl uses.
    fn g733(adc: Reply) -> Fake {
        Fake::new(&[(ADC_MEASUREMENT, 8)]).reply(8, 0, adc)
    }

    fn logi() -> Logitech {
        Logitech { curves: curve::Curves::load(), identities: HashMap::new() }
    }

    fn endpoint(pid: u16, product: &str) -> Endpoint {
        Endpoint { pid, iface: 3, product: product.into(), suffix: String::new(), short: None, long: None }
    }

    #[test]
    fn adc_headset_reports_millivolts() {
        // 4000 mV = 0x0FA0, flags: linked.
        let Raw::Millivolts { mv, charging, .. } = raw(battery(&g733(Reply::Ok(vec![0x0F, 0xA0, 0x01])), 0xFF)) else {
            panic!("not millivolts")
        };
        assert_eq!((mv, charging), (4000, false));
    }

    #[test]
    fn adc_headset_charging_flag() {
        let Raw::Millivolts { charging, .. } = raw(battery(&g733(Reply::Ok(vec![0x10, 0x68, 0x03])), 0xFF)) else {
            panic!("not millivolts")
        };
        assert!(charging);
    }

    #[test]
    fn adc_zero_flags_still_reports_voltage_like_cpp() {
        assert!(matches!(
            raw(battery(&g733(Reply::Ok(vec![0x0F, 0xA0, 0x00])), 0xFF)),
            Raw::Millivolts { mv: 4000, charging: false, .. }
        ));
    }

    #[test]
    fn unified_battery_wins_over_other_features() {
        let dev = Fake::new(&[(UNIFIED_BATTERY, 5), (BATTERY_VOLTAGE, 6)]).reply(5, 1, Reply::Ok(vec![72, 0, 1]));
        assert!(matches!(
            raw(battery(&dev, 1)),
            Raw::Percent { percent: 72, charging: true, feature: UNIFIED_BATTERY }
        ));
    }

    #[test]
    fn battery_status_percent() {
        let dev = Fake::new(&[(BATTERY_STATUS, 4)]).reply(4, 0, Reply::Ok(vec![55, 0, 0]));
        assert!(matches!(
            raw(battery(&dev, 1)),
            Raw::Percent { percent: 55, charging: false, feature: BATTERY_STATUS }
        ));
    }

    #[test]
    fn battery_voltage_charging_bit() {
        let dev = Fake::new(&[(BATTERY_VOLTAGE, 3)]).reply(3, 0, Reply::Ok(vec![0x0E, 0x10, 0x80]));
        assert!(matches!(raw(battery(&dev, 1)), Raw::Millivolts { mv: 3600, charging: true, .. }));
    }

    #[test]
    fn device_without_battery_feature() {
        assert!(matches!(raw(battery(&Fake::new(&[]), 1)), Raw::NoBattery));
    }

    #[test]
    fn silent_device_times_out() {
        assert!(matches!(battery(&g733(Reply::Timeout), 0xFF), Err(Error::Timeout)));
    }

    #[test]
    fn direct_headset_is_observed_with_name_and_percent() {
        let dev = g733(Reply::Ok(vec![0x0F, 0xA0, 0x01]));
        let o = logi().probe(&dev, &endpoint(0x0AB5, "G733 Gaming Headset"), 0xFF, false).expect("observed");
        assert_eq!(o.name.as_deref(), Some("G733 Gaming Headset"));
        assert!(matches!(o.state, State::Battery { charging: false, .. }));
    }

    #[test]
    fn silent_headset_has_no_observation_like_cpp() {
        assert!(logi().probe(&g733(Reply::Timeout), &endpoint(0x0AB5, "G733 Gaming Headset"), 0xFF, false).is_none());
    }

    #[test]
    fn silent_direct_device_without_wireless_name_is_ignored() {
        assert!(logi().probe(&g733(Reply::Timeout), &endpoint(0x0B1F, "USB Dongle"), 0xFF, false).is_none());
    }

    #[test]
    fn receiver_adc_preserves_cpp_voltage_and_grace() {
        let dev = Fake::new(&[(ADC_MEASUREMENT, 8)]).reply(8, 0, Reply::Ok(vec![0x0F, 0xA0, 0x00]));
        let out = logi().probe(&dev, &endpoint(0xC539, "USB Receiver"), 1, true).expect("voltage observation");
        assert!(matches!(out.state, State::Battery { .. }));
        assert_eq!(out.grace, Duration::from_secs(1800));
        assert_eq!(out.via, "Logitech receiver");
    }

    #[test]
    fn empty_receiver_slot_is_skipped() {
        let dev = Fake::new(&[(UNIFIED_BATTERY, 5)]).reply(5, 1, Reply::Protocol(ERR_UNKNOWN_DEVICE));
        assert!(logi().probe(&dev, &endpoint(0xC52B, "USB Receiver"), 2, true).is_none());
    }

    #[test]
    fn legacy_registers_and_levels() {
        assert!(matches!(parse_legacy(0x0d, &[55, 0, 0x22]), Some(Raw::Percent { percent: 55, charging: true, .. })));
        assert!(parse_legacy(0x0d, &[0, 0, 0x22]).is_none());
        assert!(parse_legacy(0x0d, &[101, 0, 0x22]).is_none());
        assert!(parse_legacy(0x0d, &[55]).is_none());
        for (level, percent, text) in [
            (1, 5, "Critical"),
            (2, 10, "Critical"),
            (3, 20, "Low"),
            (4, 35, "Low"),
            (5, 50, "Good"),
            (6, 75, "Good"),
            (7, 100, "Full"),
            (255, 100, "Full"),
        ] {
            assert!(matches!(parse_legacy(7, &[level, 0x26]),
                Some(Raw::Level { percent: p, charging: true, text: t }) if p == percent && t == text));
        }
        assert!(parse_legacy(7, &[0, 0]).is_none());
        let dev = Fake::new(&[]).reply(0x81, 0x0d, Reply::Protocol(2)).reply(0x81, 7, Reply::Ok(vec![5, 0x21]));
        assert!(matches!(legacy_battery(&dev, 1), Some(Raw::Level { percent: 50, charging: true, .. })));
    }

    #[test]
    fn feature_fallbacks_and_coarse_levels() {
        let dev = Fake::new(&[(UNIFIED_BATTERY, 5), (BATTERY_VOLTAGE, 6)]).reply(5, 1, Reply::Protocol(2)).reply(
            6,
            0,
            Reply::Ok(vec![0x0f, 0xa0, 0x80]),
        );
        assert!(matches!(raw(battery(&dev, 1)), Raw::Millivolts { mv: 4000, charging: true, .. }));
        for (level, percent, text) in [(8, 100, "Full"), (4, 60, "Good"), (2, 20, "Low"), (1, 5, "Critical")] {
            assert!(matches!(parse_battery(UNIFIED_BATTERY, &[0, level, 3]),
                Some(Raw::Level { percent: p, charging: true, text: t }) if p == percent && t == text));
        }
        assert!(matches!(parse_battery(UNIFIED_BATTERY, &[0, 0, 0]), Some(Raw::Unknown)));
        assert!(matches!(
            parse_battery(BATTERY_STATUS, &[0, 0, 3]),
            Some(Raw::Percent { percent: 100, charging: true, .. })
        ));
        assert!(matches!(parse_battery(BATTERY_STATUS, &[255, 0, 0]), Some(Raw::Unknown)));
        assert!(matches!(parse_battery(BATTERY_VOLTAGE, &[0, 0, 0]), Some(Raw::Unknown)));
        assert!(parse_battery(BATTERY_STATUS, &[50, 0]).is_none());
    }

    #[test]
    fn passthrough_and_bluetooth_are_not_six_slots() {
        let silent = Fake::new(&[]);
        assert_eq!(indices(&silent, &endpoint(0xc54f, "USB Receiver")), &[1]);
        assert_eq!(indices(&silent, &endpoint(0xc52b, "USB Receiver")), &[1, 2, 3, 4, 5, 6]);
        let mut bt = endpoint(0xb034, "MX Master");
        bt.long = Some(crate::hid::HidColl {
            path: r"hid#{00001812-service}".encode_utf16().collect(),
            ..Default::default()
        });
        assert_eq!(indices(&silent, &bt), &[0xff]);
        let direct = Fake::new(&[]).reply(0, 0x1d, Reply::Ok(vec![2, 0, 0x5a]));
        assert_eq!(indices(&direct, &endpoint(0x0ab5, "G733")), &[0xff]);
    }

    #[test]
    fn device_information_preserves_unit_and_model_pids() {
        let dev = Fake::new(&[(DEVICE_FW_VERSION, 3), (ADC_MEASUREMENT, 8)])
            .reply(3, 0, Reply::Ok(vec![1, 0x12, 0x34, 0x56, 0x78, 0, 0, 0x0a, 0xb5, 0x0a, 0xfe, 0, 0]))
            .reply(8, 0, Reply::Ok(vec![0x0f, 0xa0, 3]));
        let ep = endpoint(0x0ab5, "G733 Gaming Headset");
        let identity = identify(&dev, 0xff, &ep, false);
        assert_eq!(identity.unit.as_deref(), Some("12345678"));
        assert!(identity.pids.contains(&0x0afe));
        let out = logi().probe(&dev, &ep, 0xff, false).expect("battery");
        assert_eq!(out.id, "logi:unit:12345678");
        assert_eq!(out.millivolts, Some(4000));
        assert_eq!(out.grace, Duration::from_secs(60));
    }
}
