//! Community mouse protocols. Only recognized status queries are sent.
use super::{Driver, PollCtx};
use crate::device::{Kind, Observation, State};
use crate::hid::{self, HidDev};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::time::{Duration, Instant};

pub fn driver() -> Box<dyn Driver> {
    Box::new(MiscMice::default())
}

#[derive(Default)]
struct MiscMice {
    xenta: BTreeMap<Vec<u16>, XentaListener>,
    wraith: [u8; 2],
    delux_seq: u8,
}

struct XentaListener {
    dev: HidDev,
    last: Option<[u8; 5]>,
    prev: Option<[u8; 5]>,
    at: Instant,
}

fn sleep(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}
fn deadline(ms: u64) -> Instant {
    Instant::now() + Duration::from_millis(ms)
}
fn left(end: Instant) -> u32 {
    end.saturating_duration_since(Instant::now()).as_millis().max(1) as u32
}

fn read_until<T>(dev: &HidDev, ms: u64, parse: impl Fn(&[u8]) -> Option<T>) -> Option<T> {
    let end = deadline(ms);
    while Instant::now() < end {
        let r = dev.read(left(end))?;
        if let Some(value) = parse(&r) {
            return Some(value);
        }
    }
    None
}

fn mouse_state(pct: Option<u8>, charging: bool, connected: bool) -> State {
    match (connected, pct) {
        (false, _) => State::Off,
        (true, Some(percent)) => State::Battery { percent, charging },
        (true, None) if charging => State::ChargingUnknown,
        (true, None) => State::Unreachable,
    }
}

fn observation(driver: &str, vid: u16, pid: u16, name: impl Into<String>, state: State, cable: bool) -> Observation {
    let via = if cable { "USB cable" } else { "2.4 GHz receiver" };
    Observation::new(format!("{driver}:{vid:04x}:{pid:04x}"), state).name(name).kind(Kind::Mouse).via(via)
}

impl Driver for MiscMice {
    fn name(&self) -> &'static str {
        "misc_mice"
    }
    fn poll(&mut self, ctx: &mut PollCtx) {
        poll_motospeed(ctx);
        self.poll_xenta(ctx);
        self.poll_delux(ctx);
        poll_ajazz(ctx);
        poll_aj_series(ctx);
        poll_aj139(ctx);
        self.poll_wraith(ctx);
        poll_incott(ctx);
    }
}
// Motospeed: output B5 06; input B4 byte 20 carries percent plus charging bit 7.

fn parse_motospeed(r: &[u8]) -> Option<(u8, bool)> {
    if r.first() != Some(&0xb4) {
        return None;
    }
    let raw = *r.get(20)?;
    Some(((raw & 0x7f).min(100), raw >= 128))
}

fn poll_motospeed(ctx: &mut PollCtx) {
    for pid in [0xffe0, 0xfff2, 0xfff1] {
        if ctx.is_claimed(0x0bda, pid) {
            continue;
        }
        for c in ctx.find(0x0bda, pid).filter(|c| c.iface == 4 && c.out_len >= 21) {
            let cable = pid == 0xfff1;
            let (pct, charging) = if cable {
                (None, true)
            } else {
                let Some(dev) = HidDev::open(c) else { continue };
                dev.flush();
                if !dev.write(&[0xb5, 6], 500) {
                    continue;
                }
                let Some((pct, charging)) = read_until(&dev, 1500, parse_motospeed) else { continue };
                (Some(pct), charging)
            };
            ctx.add(
                observation("motospeed", c.vid, pid, "Motospeed X6", mouse_state(pct, charging, true), cable),
                c.vid,
                pid,
            );
            break;
        }
    }
}
// Xenta pushes 03 model 40/41 state level; two matching frames reject link-up's false 100%.

fn xenta_frame(r: &[u8]) -> Option<[u8; 5]> {
    let frame: [u8; 5] = r.get(..5)?.try_into().ok()?;
    (frame[0] == 3 && [0x40, 0x41].contains(&frame[2])).then_some(frame)
}

fn xenta_model(code: u8) -> (&'static str, bool) {
    match code {
        0x3d => ("Delux M900 Pro", false),
        0x55 => ("Attack Shark X11", false),
        0x4e => ("Attack Shark X3 Max", false),
        0xe5 => ("Attack Shark V3 Pro", false),
        0x24 => ("Attack Shark V6", false),
        0x10 => ("Attack Shark X6 / R1", true),
        _ => ("Wireless mouse", false),
    }
}

fn xenta_status(frame: [u8; 5], agreed: bool, cable: bool) -> (Option<u8>, bool) {
    let (_, tens) = xenta_model(frame[1]);
    let (state, level) = (frame[3], frame[4]);
    if cable || state == 2 || state == 3 {
        return ((state == 2 && !cable).then_some(100), true);
    }
    let pct = if !agreed {
        None
    } else if tens && (1..=10).contains(&level) {
        Some(level * 10)
    } else if !tens && state == 1 && (1..=100).contains(&level) {
        Some(level)
    } else {
        None
    };
    (pct, false)
}

impl MiscMice {
    fn poll_xenta(&mut self, ctx: &mut PollCtx) {
        let mut present = BTreeSet::new();
        for pid in [0xfa60, 0xfa65, 0xfa55, 0x2155] {
            if ctx.is_claimed(0x1d57, pid) {
                continue;
            }
            for c in ctx.find(0x1d57, pid).filter(|c| c.page == 0x0a && c.in_len >= 5) {
                present.insert(c.path.clone());
                let fresh = !self.xenta.contains_key(&c.path);
                if fresh {
                    let Some(dev) = HidDev::open_read_only(c) else { continue };
                    self.xenta
                        .insert(c.path.clone(), XentaListener { dev, last: None, prev: None, at: Instant::now() });
                }
                let Some(l) = self.xenta.get_mut(&c.path) else { continue };
                let end = deadline(5000);
                let mut got = 0;
                loop {
                    if fresh && got < 2 && Instant::now() >= end {
                        break;
                    }
                    let wait = if fresh && got < 2 { left(end) } else { 30 };
                    let Some(r) = l.dev.read(wait) else { break };
                    if r.len() < 5 {
                        break;
                    }
                    let Some(frame) = xenta_frame(&r) else { continue };
                    l.prev = l.last;
                    l.last = Some(frame);
                    l.at = Instant::now();
                    got += 1;
                }
                let Some(frame) = l.last.filter(|_| l.at.elapsed() <= Duration::from_secs(15)) else { continue };
                let cable = [0xfa55, 0x2155].contains(&pid);
                let (pct, charging) = xenta_status(frame, l.prev == l.last, cable);
                ctx.add(
                    observation("xenta", c.vid, pid, xenta_model(frame[1]).0, mouse_state(pct, charging, true), cable),
                    c.vid,
                    pid,
                );
                break;
            }
        }
        self.xenta.retain(|path, _| present.contains(path));
    }

    fn poll_delux(&mut self, ctx: &mut PollCtx) {
        for pid in [0x5b2e, 0x5b2f] {
            if ctx.is_claimed(0x248a, pid) {
                continue;
            }
            for c in ctx.find(0x248a, pid).filter(|c| c.page == 0xff00 && c.usage == 1 && c.feat_len >= 33) {
                let Some(dev) = HidDev::open_feature_only(c) else { continue };
                let receiver = pid == 0x5b2f;
                self.delux_seq = self.delux_seq.wrapping_add(1);
                let mut req = [0x0c, 1, 0x20, 0, self.delux_seq];
                if !dev.set_feature(&req) {
                    continue;
                }
                sleep(20);
                let Some(mut r) = dev.get_feature(0x0c).filter(|r| valid_delux(r)) else { continue };
                if receiver && r.get(6) == Some(&0) {
                    self.delux_seq = self.delux_seq.wrapping_add(1);
                    req[4] = self.delux_seq;
                    if dev.set_feature(&req) {
                        sleep(20);
                        if let Some(next) = dev.get_feature(0x0c).filter(|r| valid_delux(r)) {
                            r = next;
                        }
                    }
                }
                let Some((pct, charging, connected)) = parse_delux(&r) else { continue };
                if !receiver && connected {
                    ctx.claim(0x248a, 0x5b2f);
                }
                ctx.add(
                    observation(
                        "delux",
                        c.vid,
                        pid,
                        "Delux M800 Pro",
                        mouse_state(pct, charging, connected),
                        !receiver,
                    ),
                    c.vid,
                    pid,
                );
                break;
            }
        }
    }
}

// Delux feature 0C/20: byte 6 link, 18 percent, 19 charging/full; increment sequence per query.
fn valid_delux(r: &[u8]) -> bool {
    r.len() >= 20 && r.first() == Some(&0x0c) && r.get(2) == Some(&0x20)
}
fn parse_delux(r: &[u8]) -> Option<(Option<u8>, bool, bool)> {
    if !valid_delux(r) {
        return None;
    }
    let pct = *r.get(18)?;
    if pct > 100 {
        return None;
    }
    let state = *r.get(19)?;
    let connected = r.get(6) != Some(&0) || pct > 0;
    Some((
        if state == 2 { Some(100) } else { (connected && pct > 0).then_some(pct) },
        [1, 2].contains(&state),
        connected,
    ))
}

// Ajazz command 10: checksum over bytes 4..30, model Mxxx, charge/link flags.
fn parse_ajazz(r: &[u8]) -> Option<(String, u8, bool, bool)> {
    if r.first() != Some(&0x10) || r.get(1) != Some(&0) {
        return None;
    }
    let sum = r.get(4..31)?.iter().fold(0u8, |sum, &b| sum.wrapping_add(b));
    if r.get(31) != Some(&sum) {
        return None;
    }
    let pct = *r.get(13)?;
    if pct > 100 {
        return None;
    }
    let model = r.get(4..8)?;
    let name = if model.first() == Some(&b'M') && model.iter().all(|b| (0x20..0x7f).contains(b)) {
        format!("Ajazz AJ{}", String::from_utf8_lossy(model.get(1..)?))
    } else {
        "Ajazz mouse".into()
    };
    Some((name, pct, *r.get(12)? != 0, *r.get(14)? != 0))
}

fn poll_ajazz(ctx: &mut PollCtx) {
    for vendor in [0x249a, 0x248a] {
        for (vid, pid) in ctx.products(vendor) {
            for c in ctx.find(vid, pid).filter(|c| c.iface == 2 && c.out_len == 33 && c.in_len == 33) {
                let Some(dev) = HidDev::open(c) else { continue };
                dev.flush();
                if !dev.write(&[0, 0x10], 500) {
                    continue;
                }
                let Some((name, pct, charging, linked)) = read_until(&dev, 1500, parse_ajazz) else { continue };
                let cable = vid == 0x248a;
                let connected = cable || linked;
                ctx.add(
                    observation(
                        "ajazz",
                        vid,
                        pid,
                        name,
                        mouse_state(connected.then_some(pct), charging, connected),
                        cable,
                    ),
                    vid,
                    pid,
                );
                break;
            }
        }
    }
}

// SONiX AJ-series: SET_FEATURE 00 F7 enables telemetry; GET_FEATURE 05 byte 3 is percent.
fn parse_aj_series(r: &[u8]) -> Option<u8> {
    match r {
        [5, 0, 0, p, ..] if *p <= 100 => Some(*p),
        _ => None,
    }
}
fn poll_aj_series(ctx: &mut PollCtx) {
    for (vid, pid) in ctx.products(0x3151) {
        for c in ctx.find(vid, pid).filter(|c| c.page == 0xffff && c.usage == 2 && c.feat_len >= 65) {
            let Some(dev) = HidDev::open_feature_only(c) else { continue };
            if !dev.set_feature(&[0, 0xf7]) {
                continue;
            }
            sleep(30);
            let Some(pct) = dev.get_feature(5).and_then(|r| parse_aj_series(&r)) else { continue };
            let name = hid::product(c);
            ctx.add(
                observation(
                    "ajazzaj",
                    vid,
                    pid,
                    if name.is_empty() { "AJAZZ mouse".into() } else { name },
                    mouse_state((pct > 0).then_some(pct), false, pct > 0),
                    false,
                ),
                vid,
                pid,
            );
            break;
        }
    }
}

// AJ139 unnumbered AA 30: bytes 8/9 carry battery and USB power; AA FA/ED are unrelated.
fn parse_aj139(r: &[u8]) -> Option<(u8, bool)> {
    match r {
        [0xaa, 0x30, ..] => Some((*r.get(8)?, *r.get(9)? != 0)),
        _ => None,
    }
}
fn poll_aj139(ctx: &mut PollCtx) {
    for vid in [0xa8a5, 0xa8a4] {
        let pid = 0x2255;
        if ctx.is_claimed(vid, pid) {
            continue;
        }
        for c in ctx.find(vid, pid).filter(|c| c.page == 0xff01 && c.usage == 0x10 && c.out_len >= 65 && c.in_len >= 65)
        {
            let Some(dev) = HidDev::open(c) else { continue };
            dev.flush();
            let mut result = None;
            if dev.write(&[0, 0x55, 0x30, 0xa5, 0x2e, 0x2e], 500) {
                result = read_until(&dev, 700, parse_aj139);
            }
            if result.is_none() && dev.write(&[0, 0x55, 0x30, 0xa5, 0x0b, 0x2e, 1, 1], 500) {
                result = read_until(&dev, 800, parse_aj139);
            }
            let Some((pct, charging)) = result.filter(|(p, _)| *p <= 100) else { continue };
            let cable = vid == 0xa8a4;
            let connected = cable || pct > 0;
            ctx.add(
                observation(
                    "ajazz139",
                    vid,
                    pid,
                    "AJAZZ wireless mouse",
                    mouse_state(connected.then_some(pct), charging, connected),
                    cable,
                ),
                vid,
                pid,
            );
            break;
        }
    }
}

// Incott 09 89: battery byte 2, bit 7 charging; all-zero status is stale.
fn parse_incott(r: &[u8]) -> Option<(u8, bool)> {
    match r {
        [9, 0x89, raw, rate, ..] if (*raw | *rate) != 0 && (*raw & 0x7f) <= 100 => {
            Some((*raw & 0x7f, *raw & 0x80 != 0))
        }
        _ => None,
    }
}
fn poll_incott(ctx: &mut PollCtx) {
    for pid in [0x522c, 0x622c] {
        if ctx.is_claimed(0x093a, pid) {
            continue;
        }
        let colls: Vec<_> = ctx.find(0x093a, pid).filter(|c| c.page == 0xff05 && c.feat_len >= 9).collect();
        for c in colls.into_iter().rev() {
            let Some(dev) = HidDev::open_feature_only(c) else { continue };
            if !dev.set_feature(&[9, 0x89]) {
                continue;
            }
            let mut result = None;
            for _ in 0..10 {
                sleep(50);
                let Some(r) = dev.get_feature(9) else { break };
                if r.len() >= 4
                    && r.first() == Some(&9)
                    && r.get(1) == Some(&0x89)
                    && r.get(2).zip(r.get(3)).is_some_and(|(a, b)| a | b != 0)
                {
                    result = parse_incott(&r);
                    break;
                }
            }
            let Some((pct, charging)) = result else { continue };
            let cable = pid == 0x622c;
            let connected = cable || pct > 0;
            let mut obs = observation(
                "incott",
                c.vid,
                pid,
                "Incott mouse",
                mouse_state(connected.then_some(pct), charging, connected),
                cable,
            );
            if !cable {
                obs = obs.grace(Duration::from_secs(1800));
            }
            ctx.add(obs, c.vid, pid);
            break;
        }
    }
}

// Wraith shares Incott ids: identify 8F model 18/20 before the timed 86 button sweep.
fn parse_wraith_model(r: &[u8]) -> Option<u8> {
    match r {
        [9, 0x8f, _, model, ..] => Some(if [18, 20].contains(model) { *model } else { 0 }),
        _ => None,
    }
}
fn parse_wraith_status(r: &[u8]) -> Option<(u8, bool)> {
    match r {
        [9, raw, ..] if (*raw & 0x7f) <= 100 => Some((*raw & 0x7f, *raw & 0x80 != 0)),
        _ => None,
    }
}
fn read_button(dev: &HidDev, button: u8) -> bool {
    if !dev.set_feature(&[9, 0x86, button]) {
        return false;
    }
    for ms in [150, 20] {
        sleep(ms);
        let Some(r) = dev.get_feature(9) else { return true };
        if r.len() >= 4 && r.get(1) == Some(&0x86) {
            break;
        }
    }
    true
}
fn listen_wraith(ins: &[HidDev], ms: u64) -> Option<(u8, bool)> {
    let end = deadline(ms);
    while Instant::now() < end {
        for input in ins {
            if let Some(status) = input.read(50).and_then(|r| parse_wraith_status(&r)) {
                return Some(status);
            }
        }
    }
    None
}
impl MiscMice {
    fn poll_wraith(&mut self, ctx: &mut PollCtx) {
        for (index, pid) in [0x522c, 0x622c].into_iter().enumerate() {
            if ctx.is_claimed(0x093a, pid) {
                continue;
            }
            let colls: Vec<_> = ctx.find(0x093a, pid).filter(|c| c.page == 0xff05 && c.feat_len >= 9).collect();
            if colls.is_empty() {
                continue;
            }
            let mut dev = None;
            let mut replied = None;
            for c in colls.into_iter().rev() {
                let Some(d) = HidDev::open_feature_only(c) else { continue };
                if !d.set_feature(&[9, 0x8f]) {
                    continue;
                }
                for _ in 0..10 {
                    sleep(50);
                    let Some(r) = d.get_feature(9) else { break };
                    if let Some(model) = parse_wraith_model(&r) {
                        replied = Some(model);
                        break;
                    }
                }
                if replied.is_some() {
                    dev = Some(d);
                    break;
                }
            }
            if let Some(model) = replied {
                self.wraith[index] = model;
            }
            let model = self.wraith[index];
            if model == 0 {
                continue;
            }
            let cable = pid == 0x622c;
            let mut status = None;
            if let Some(dev) = dev {
                let ins: Vec<_> = ctx
                    .find(0x093a, pid)
                    .filter(|c| c.page == 0xff05 && c.in_len >= 2)
                    .filter_map(HidDev::open_read_only)
                    .collect();
                for input in &ins {
                    input.flush();
                }
                if !ins.is_empty() {
                    sleep(200);
                    let mut sent = false;
                    for b in [0, 1, 2, 4, 3] {
                        sent |= read_button(&dev, b);
                    }
                    if sent {
                        status = listen_wraith(&ins, 400);
                        for _ in 0..4 {
                            if status.is_some() || !read_button(&dev, 0) {
                                break;
                            }
                            status = listen_wraith(&ins, 550);
                        }
                    }
                }
            }
            let (pct, charging) = status.map(|(p, c)| (Some(p), c)).unwrap_or((None, false));
            let connected = status.is_some_and(|(p, c)| cable || p > 0 || c);
            let mut obs = observation(
                "wraithw1",
                0x093a,
                pid,
                if model == 20 { "Wraith W1 Freestyle" } else { "Wraith W1" },
                mouse_state(pct, charging, connected),
                cable,
            );
            if !cable {
                obs = obs.grace(Duration::from_secs(1800));
            }
            ctx.add(obs, 0x093a, pid);
        }
    }
}

/// Diagnostic-only enumeration: normal polling always uses PollCtx collections.
pub fn dump_xenta_frames() -> String {
    let mut out = String::new();
    for c in hid::enumerate(&[]) {
        if c.vid != 0x1d57 || c.page != 0x0a || c.in_len < 5 {
            continue;
        }
        let _ = writeln!(out, "\nXenta {:04X}:{:04X} status frames (6 s):", c.vid, c.pid);
        let Some(dev) = HidDev::open_read_only(&c) else {
            let _ =
                writeln!(out, "  cannot open (error {})", unsafe { windows_sys::Win32::Foundation::GetLastError() });
            continue;
        };
        let end = deadline(6000);
        let mut frames = 0;
        while Instant::now() < end && frames < 16 {
            let Some(r) = dev.read(left(end)) else { break };
            out.push(' ');
            for b in r {
                let _ = write!(out, " {b:02X}");
            }
            out.push('\n');
            frames += 1;
        }
        if frames == 0 {
            out.push_str("  none (mouse off, asleep or on Bluetooth?)\n");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse() {
        assert_eq!(
            xenta_frame(&[3, 0x2c, 0x40, 1, 0x36]).map(|r| xenta_status(r, true, false)),
            Some((Some(54), false))
        );
        assert_eq!(xenta_frame(&[3, 0x2c, 0x40, 1, 0xff]).map(|r| xenta_status(r, true, false)), Some((None, false)));
        assert_eq!(xenta_frame(&[1, 0]), None);
        assert_eq!(xenta_status([3, 0x24, 0x41, 1, 100], true, false), (Some(100), false));
        assert_eq!(xenta_status([3, 0x10, 0x40, 1, 8], true, false), (Some(80), false));
        assert_eq!(xenta_status([3, 0x3d, 0x40, 3, 80], true, false), (None, true));
        assert_eq!(xenta_status([3, 0x3d, 0x40, 2, 80], true, false), (Some(100), true));
        assert_eq!(xenta_status([3, 0x3d, 0x40, 1, 100], false, false), (None, false));
    }
    #[test]
    fn mouse_replies() {
        let mut moto = [0; 21];
        moto[0] = 0xb4;
        moto[20] = 128 + 64;
        assert_eq!(parse_motospeed(&moto), Some((64, true)));
        assert_eq!(parse_motospeed(&moto[..20]), None);
        assert_eq!(parse_aj_series(&[5, 0, 0, 72]), Some(72));
        assert_eq!(parse_aj_series(&[5, 1, 0, 72]), None);
        assert_eq!(parse_aj_series(&[5, 0, 0, 101]), None);
        let mut aj = [0; 32];
        aj[0] = 0x10;
        aj[4..8].copy_from_slice(b"M179");
        aj[12] = 1;
        aj[13] = 64;
        aj[14] = 1;
        aj[31] = aj[4..31].iter().fold(0u8, |s, &b| s.wrapping_add(b));
        assert_eq!(parse_ajazz(&aj), Some(("Ajazz AJ179".into(), 64, true, true)));
        aj[31] ^= 1;
        assert_eq!(parse_ajazz(&aj), None);
        let mut aj139 = [0; 10];
        aj139[0] = 0xaa;
        aj139[1] = 0x30;
        aj139[8] = 91;
        aj139[9] = 1;
        assert_eq!(parse_aj139(&aj139), Some((91, true)));
        aj139[1] = 0xfa;
        assert_eq!(parse_aj139(&aj139), None);
        assert_eq!(parse_incott(&[9, 0x89, 0x80 | 64, 1]), Some((64, true)));
        assert_eq!(parse_incott(&[9, 0x89, 0, 0]), None);
        assert_eq!(parse_wraith_model(&[9, 0x8f, 0, 20]), Some(20));
        assert_eq!(parse_wraith_model(&[9, 0x8f, 0, 6]), Some(0));
        assert_eq!(parse_wraith_status(&[9, 0x80 | 80]), Some((80, true)));
        assert_eq!(parse_wraith_status(&[9, 101]), None);
        let mut delux = [0; 20];
        delux[0] = 12;
        delux[2] = 0x20;
        delux[6] = 1;
        delux[18] = 70;
        delux[19] = 1;
        assert_eq!(parse_delux(&delux), Some((Some(70), true, true)));
        delux[19] = 2;
        assert_eq!(parse_delux(&delux), Some((Some(100), true, true)));
        delux[18] = 101;
        assert_eq!(parse_delux(&delux), None);
        for r in [&[][..], &[9][..]] {
            assert_eq!(parse_incott(r), None);
            assert_eq!(parse_wraith_model(r), None);
            assert_eq!(parse_wraith_status(r), None);
            assert_eq!(parse_ajazz(r), None);
            assert_eq!(parse_aj139(r), None);
            assert_eq!(parse_delux(r), None);
        }
    }
}
