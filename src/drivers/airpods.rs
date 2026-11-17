//! Apple Continuity proximity advertisements, matched against paired Apple audio model IDs.
use super::{Driver, PollCtx};
use crate::device::{Kind, Observation};
use std::{
    collections::{BTreeMap, HashSet},
    mem::{size_of, zeroed},
    ptr::{null, null_mut},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use windows::{
    Devices::Bluetooth::Advertisement::{
        BluetoothLEAdvertisementReceivedEventArgs, BluetoothLEAdvertisementWatcher,
        BluetoothLEAdvertisementWatcherStatus,
    },
    Foundation::{EventRegistrationToken, TypedEventHandler},
    Storage::Streams::DataReader,
};
use windows_sys::{
    core::GUID,
    Win32::Devices::{
        DeviceAndDriverInstallation::*,
        Properties::{DEVPROPKEY, DEVPROP_TYPE_BOOLEAN},
    },
};
const CONNECTED: DEVPROPKEY = DEVPROPKEY { fmtid: GUID::from_u128(0x83da6326_97a6_4088_9453_a1923f573b29), pid: 15 };
const FRIENDLY: DEVPROPKEY = DEVPROPKEY { fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0), pid: 14 };
const CONTAINER: DEVPROPKEY = DEVPROPKEY { fmtid: GUID::from_u128(0x8c7ed206_3f8a_4827_b3ab_ae9e1faefc6c), pid: 2 };
#[derive(Clone, Debug)]
struct Advert {
    model: u16,
    single: bool,
    left: i32,
    right: i32,
    case: i32,
    left_charging: bool,
    right_charging: bool,
    case_charging: bool,
    rssi: i16,
    tick: Instant,
}
fn level(n: u8) -> i32 {
    if n <= 10 {
        i32::from(n) * 10
    } else {
        -1
    }
}
fn single(model: u16) -> bool {
    matches!(
        model,
        0x200a | 0x201f | 0x202d | 0x2003 | 0x2005 | 0x2006 | 0x2009 | 0x200c | 0x200d | 0x2010 | 0x2017 | 0x2025
    )
}
fn parse_tlv(d: &[u8]) -> Option<Advert> {
    if d.len() < 27 || d.get(..3)? != [7, 0x19, 1] {
        return None;
    }
    let model = u16::from_le_bytes([*d.get(3)?, *d.get(4)?]);
    if model & 0xff00 != 0x2000 {
        return None;
    }
    let left_broadcasts = d.get(5)? & 0x20 != 0;
    let current = d.get(6)? & 15;
    let other = d.get(6)? >> 4;
    let flags = d.get(7)? >> 4;
    let mut a = Advert {
        model,
        single: single(model),
        left: -1,
        right: -1,
        case: -1,
        left_charging: false,
        right_charging: false,
        case_charging: false,
        rssi: -127,
        tick: Instant::now(),
    };
    if a.single {
        let swapped = current == 0 && (1..=10).contains(&other);
        a.left = level(if swapped { other } else { current });
        a.left_charging = flags & if swapped { 2 } else { 1 } != 0;
        return (a.left >= 0).then_some(a);
    }
    a.left = level(if left_broadcasts { current } else { other });
    a.right = level(if left_broadcasts { other } else { current });
    a.left_charging = a.left >= 0 && flags & if left_broadcasts { 1 } else { 2 } != 0;
    a.right_charging = a.right >= 0 && flags & if left_broadcasts { 2 } else { 1 } != 0;
    a.case = level(d.get(7)? & 15);
    a.case_charging = a.case >= 0 && flags & 4 != 0;
    (a.left >= 0 || a.right >= 0 || a.case >= 0).then_some(a)
}
fn parse(mut bytes: &[u8]) -> Option<Advert> {
    while let Some(header) = bytes.get(..2) {
        let len = usize::from(*header.get(1)?) + 2;
        let tlv = bytes.get(..len)?;
        if header.first() == Some(&7) {
            if let Some(a) = parse_tlv(tlv) {
                return Some(a);
            }
        }
        bytes = bytes.get(len..)?;
    }
    None
}
#[derive(Default)]
struct AppleDevice {
    name: String,
    pid: u16,
    connected: bool,
    apple: bool,
}
fn paired() -> Vec<AppleDevice> {
    let mut by_container: BTreeMap<[u8; 16], AppleDevice> = BTreeMap::new();
    let enumerator: Vec<u16> = "BTHENUM\0".encode_utf16().collect();
    unsafe {
        let di = SetupDiGetClassDevsW(null(), enumerator.as_ptr(), null_mut(), DIGCF_ALLCLASSES | DIGCF_PRESENT);
        if di == -1 {
            return Vec::new();
        }
        let mut dd: SP_DEVINFO_DATA = zeroed();
        dd.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;
        let mut i = 0;
        while SetupDiEnumDeviceInfo(di, i, &mut dd) != 0 {
            i += 1;
            let mut inst = [0u16; 512];
            if SetupDiGetDeviceInstanceIdW(di, &dd, inst.as_mut_ptr(), 512, null_mut()) == 0 {
                continue;
            }
            let mut container = [0u8; 16];
            let mut ty = 0;
            if SetupDiGetDevicePropertyW(di, &dd, &CONTAINER, &mut ty, container.as_mut_ptr(), 16, null_mut(), 0) == 0 {
                continue;
            }
            let d = by_container.entry(container).or_default();
            let inst = String::from_utf16_lossy(&inst[..inst.iter().position(|v| *v == 0).unwrap_or(inst.len())]);
            let id = inst.to_uppercase();
            if let Some(v) = id.find("_VID&0001004C") {
                d.apple = true;
                if let Some(p) = id.get(v..).and_then(|s| s.find("_PID&").map(|p| p + v + 5)) {
                    if let Some(hex) = id.get(p..p + 4) {
                        d.pid = u16::from_str_radix(hex, 16).unwrap_or(0);
                    }
                }
            }
            if inst.starts_with("BTHENUM\\DEV_") {
                let mut name = [0u16; 256];
                if SetupDiGetDevicePropertyW(di, &dd, &FRIENDLY, &mut ty, name.as_mut_ptr().cast(), 510, null_mut(), 0)
                    != 0
                {
                    d.name = String::from_utf16_lossy(&name[..name.iter().position(|v| *v == 0).unwrap_or(name.len())]);
                }
                let mut connected = 0u8;
                if SetupDiGetDevicePropertyW(di, &dd, &CONNECTED, &mut ty, &mut connected, 1, null_mut(), 0) != 0
                    && ty == DEVPROP_TYPE_BOOLEAN
                {
                    d.connected = connected != 0;
                }
            }
        }
        SetupDiDestroyDeviceInfoList(di);
    }
    let mut devices: Vec<_> = by_container
        .into_values()
        .filter(|d| {
            let n = d.name.to_uppercase();
            (d.apple && d.pid & 0xff00 == 0x2000)
                || (d.connected && d.pid == 0 && (n.contains("AIRPODS") || n.contains("BEATS")))
        })
        .collect();
    devices.sort_by_key(|d| !d.connected);
    devices
}
fn pick(dev: &AppleDevice, adverts: &BTreeMap<u64, Advert>, now: Instant) -> Option<Advert> {
    let max_age = Duration::from_secs(if dev.connected { 300 } else { 90 });
    let min_rssi = if dev.connected { -90 } else { -80 };
    let fresh = |a: &Advert| now.saturating_duration_since(a.tick) < Duration::from_secs(90);
    let mut best: Option<Advert> = None;
    for a in adverts.values() {
        if (dev.pid != 0 && a.model != dev.pid) || now.saturating_duration_since(a.tick) > max_age || a.rssi < min_rssi
        {
            continue;
        }
        if best.as_ref().is_none_or(|b| {
            (fresh(a) && !fresh(b))
                || (fresh(a) && fresh(b) && a.rssi > b.rssi)
                || (!fresh(a) && !fresh(b) && a.tick > b.tick)
        }) {
            best = Some(a.clone());
        }
    }
    let mut best = best?;
    if !best.single {
        for a in adverts.values() {
            let delta =
                if best.tick > a.tick { best.tick.duration_since(a.tick) } else { a.tick.duration_since(best.tick) };
            if a.model != best.model || a.single || delta > Duration::from_secs(15) {
                continue;
            }
            if best.left < 0 && a.left >= 0 {
                best.left = a.left;
                best.left_charging = a.left_charging;
            }
            if best.right < 0 && a.right >= 0 {
                best.right = a.right;
                best.right_charging = a.right_charging;
            }
            if best.case < 0 && a.case >= 0 {
                best.case = a.case;
                best.case_charging = a.case_charging;
            }
        }
    }
    Some(best)
}
#[derive(Default)]
struct AirPods {
    watcher: Option<(BluetoothLEAdvertisementWatcher, EventRegistrationToken)>,
    adverts: Arc<Mutex<BTreeMap<u64, Advert>>>,
}
pub fn driver() -> Box<dyn Driver> {
    Box::new(AirPods::default())
}
impl AirPods {
    fn ensure_watcher(&mut self) -> bool {
        if self.watcher.is_none() {
            let Ok(watcher) = BluetoothLEAdvertisementWatcher::new() else { return false };
            let adverts = Arc::clone(&self.adverts);
            let handler =
                TypedEventHandler::<BluetoothLEAdvertisementWatcher, BluetoothLEAdvertisementReceivedEventArgs>::new(
                    move |_, args| {
                        let Some(args) = args else { return Ok(()) };
                        let view = args.Advertisement()?.GetManufacturerDataByCompanyId(0x004c)?;
                        for i in 0..view.Size()? {
                            let buffer = view.GetAt(i)?.Data()?;
                            let mut bytes = vec![0; buffer.Length()? as usize];
                            DataReader::FromBuffer(&buffer)?.ReadBytes(&mut bytes)?;
                            if let Some(mut a) = parse(&bytes) {
                                a.rssi = args.RawSignalStrengthInDBm()?;
                                a.tick = Instant::now();
                                let addr = args.BluetoothAddress()?;
                                if let Ok(mut map) = adverts.lock() {
                                    let now = a.tick;
                                    map.insert(addr, a);
                                    map.retain(|_, a| {
                                        now.saturating_duration_since(a.tick) <= Duration::from_secs(600)
                                    });
                                }
                            }
                        }
                        Ok(())
                    },
                );
            let Ok(token) = watcher.Received(&handler) else { return false };
            let started = watcher.Start().is_ok();
            self.watcher = Some((watcher, token));
            return started;
        }
        if let Some((watcher, _)) = &self.watcher {
            if watcher.Status().is_ok_and(|s| {
                s == BluetoothLEAdvertisementWatcherStatus::Stopped
                    || s == BluetoothLEAdvertisementWatcherStatus::Aborted
            }) {
                return watcher.Start().is_ok();
            }
        }
        false
    }
    fn pick(&self, dev: &AppleDevice) -> Option<Advert> {
        let adverts = self.adverts.lock().ok()?;
        pick(dev, &adverts, Instant::now())
    }
}
impl Drop for AirPods {
    fn drop(&mut self) {
        if let Some((watcher, token)) = &self.watcher {
            let _ = watcher.RemoveReceived(*token);
            let _ = watcher.Stop();
        }
    }
}
fn battery(a: &Advert) -> (u8, bool) {
    if a.single {
        return (a.left as u8, a.left_charging);
    }
    let mut discharging: Option<i32> = None;
    let mut charging: Option<i32> = None;
    for (p, ch) in [(a.left, a.left_charging), (a.right, a.right_charging)] {
        if p < 0 {
            continue;
        }
        let slot = if ch { &mut charging } else { &mut discharging };
        *slot = Some(slot.map_or(p, |old| old.min(p)));
    }
    if let Some(p) = discharging {
        (p as u8, false)
    } else if let Some(p) = charging {
        (p as u8, true)
    } else {
        (a.case as u8, a.case_charging)
    }
}
impl Driver for AirPods {
    fn name(&self) -> &'static str {
        "AirPods"
    }
    fn poll(&mut self, ctx: &mut PollCtx) {
        let devices = paired();
        if devices.is_empty() {
            return;
        }
        if self.ensure_watcher() {
            for _ in 0..15 {
                if devices.iter().any(|d| self.pick(d).is_some()) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
        let mut used = HashSet::new();
        for d in devices {
            if d.pid != 0 && !d.connected && used.contains(&d.pid) {
                continue;
            }
            let Some(a) = self.pick(&d) else { continue };
            if d.pid != 0 {
                used.insert(d.pid);
            }
            let name = if d.name.is_empty() { "AirPods".into() } else { d.name };
            let (p, ch) = battery(&a);
            let mut obs =
                Observation::battery(format!("airpods:004c:{:04x}:{name}", d.pid), name, Kind::Headset, p, ch);
            if a.single {
                obs = obs.via("Bluetooth");
            } else {
                let pct = |p: i32, ch| {
                    format!("{}{}", if p < 0 { "?".into() } else { format!("{p}%") }, if ch { "\u{26a1}" } else { "" })
                };
                obs = obs.via("Bluetooth").detail(format!(
                    "E {}  D {}  estojo {}",
                    pct(a.left, a.left_charging),
                    pct(a.right, a.right_charging),
                    pct(a.case, a.case_charging)
                ));
            }
            if d.connected {
                obs = obs.grace(Duration::from_secs(300));
            }
            ctx.out.push(obs);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn advert(model: u16, battery: u8, flags: u8) -> Vec<u8> {
        let mut d = vec![0; 27];
        d[..3].copy_from_slice(&[7, 0x19, 1]);
        d[3..5].copy_from_slice(&model.to_le_bytes());
        d[6] = battery;
        d[7] = flags;
        d
    }
    #[test]
    fn single_battery_variants() {
        for model in [0x200a, 0x201f, 0x202d, 0x2003, 0x2005, 0x2006, 0x2009, 0x200c, 0x200d, 0x2010, 0x2017, 0x2025] {
            let a = parse(&advert(model, 7, 0x90)).expect("single frame");
            assert_eq!(battery(&a), (70, true));
            assert_eq!(a.right, -1);
        }
        assert_eq!(battery(&parse(&advert(0x200a, 0x80, 0xa0)).expect("swapped")), (80, true));
        assert!(parse(&advert(0x200a, 0xff, 0)).is_none());
    }
    #[test]
    fn dual_buds_and_tlv() {
        let mut d = advert(0x2014, 0x87, 0x19);
        d[5] = 0x20;
        let a = parse(&d).expect("dual");
        assert_eq!((a.left, a.right, a.case), (70, 80, 90));
        assert_eq!(battery(&a), (80, false));
        let mut prefixed = vec![1, 1, 0xaa];
        prefixed.extend(&d);
        assert!(parse(&prefixed).is_some());
        d[2] = 0;
        assert!(parse(&d).is_none());
        assert!(parse(&[7, 0x19]).is_none());
    }
    #[test]
    fn strongest_recent_model_match() {
        let now = Instant::now();
        let mut a = parse(&advert(0x2014, 0x87, 9)).expect("frame");
        a.tick = now;
        a.rssi = -70;
        let mut b = a.clone();
        b.rssi = -60;
        b.left = 20;
        let mut map = BTreeMap::new();
        map.insert(1, a);
        map.insert(2, b);
        let dev = AppleDevice { pid: 0x2014, connected: true, ..Default::default() };
        assert_eq!(pick(&dev, &map, now).expect("best").left, 20);
        assert!(pick(&AppleDevice { pid: 0x200a, ..Default::default() }, &map, now).is_none());
        assert!(pick(&dev, &map, now + Duration::from_secs(301)).is_none());
    }
}
