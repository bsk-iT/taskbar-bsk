//! BlackShark vendor protocols and Barracuda X YS-Tech/Airoha bridge.
//! V3 battery queries must remain five minutes apart to avoid dropping the radio link.
use super::{Driver, PollCtx};
use crate::{
    device::{Kind, Observation, State},
    hid::HidDev,
};
use std::{
    collections::BTreeMap,
    thread::sleep,
    time::{Duration, Instant},
};
const VID: u16 = 0x1532;
const MIN_INTERVAL: Duration = Duration::from_secs(300);
struct Cached {
    at: Instant,
    status: Observation,
}
struct RazerHeadsets {
    cache: BTreeMap<u16, Cached>,
    hs_seq: u8,
    txid: u8,
}
impl Default for RazerHeadsets {
    fn default() -> Self {
        Self { cache: BTreeMap::new(), hs_seq: 0, txid: 0x40 }
    }
}
fn base(prefix: &str, pid: u16, name: &str, percent: u8, charging: bool) -> Observation {
    Observation::battery(format!("{prefix}:1532:{pid:04x}"), name, Kind::Headset, percent, charging)
        .via("2.4 GHz receiver")
}
fn packet(dir: u8, cls: u8, seq: u8) -> [u8; 64] {
    let mut b = [0; 64];
    b[0] = 2;
    b[2] = seq;
    b[6] = 4;
    b[9] = dir;
    b[10] = cls;
    b[62] = b[..62].iter().fold(0, |a, b| a ^ b);
    b
}
fn remaining(end: Instant) -> u32 {
    end.saturating_duration_since(Instant::now()).as_millis() as u32
}
fn parse_v3(r: &[u8], cls: u8) -> Option<(u8, bool)> {
    r.get(..15)?;
    if r.first() != Some(&2) || r.get(2) != Some(&0x60) || r.get(10) != Some(&cls) {
        return None;
    }
    Some((*r.get(13)?, *r.get(14)? != 0))
}
fn get_v3(d: &HidDev, cls: u8) -> Option<(u8, bool)> {
    if !d.write(&packet(0x80, cls, 0x60), 500) {
        return None;
    }
    let end = Instant::now() + Duration::from_millis(600);
    loop {
        let left = remaining(end);
        if left == 0 {
            return None;
        }
        let r = d.read(left)?;
        if r.len() < 15 {
            return None;
        }
        if let Some(v) = parse_v3(&r, cls) {
            return Some(v);
        }
    }
}
fn parse_hs(r: &[u8], cmd: u8, seq: u8) -> Option<u8> {
    r.get(..64)?;
    if r.first() != Some(&2) || r.get(2) != Some(&seq) || r.get(10) != Some(&cmd) || *r.get(12)? < 1 {
        return None;
    }
    if r.get(1..62)?.iter().fold(0, |a, b| a ^ b) != *r.get(62)? {
        return None;
    }
    r.get(13).copied()
}
fn parse_2020(r: &[u8], last: bool) -> Option<(State, u16)> {
    if r.len() != 64 {
        return None;
    }
    let fresh = r.get(..9) == Some(&[0xff, 0x0f, 5, 0xfe, 0x12, 4, 0x1f, 8, 5][..]);
    let offline = last && r.get(..9) == Some(&[0xff, 1, 0, 0xfe, 0x12, 4, 0x1f, 8, 5][..]);
    if !fresh && !offline {
        return None;
    }
    let mv = if fresh { u16::from_be_bytes([*r.get(12)?, *r.get(13)?]) } else { 0 };
    if mv == 0 {
        return Some((State::Off, mv));
    }
    let charging = r.get(11) == Some(&9);
    let state = if !charging && *r.get(14)? <= 100 {
        State::Battery { percent: *r.get(14)?, charging: false }
    } else {
        State::Unreachable
    };
    Some((state, mv))
}
// PA envelope: type 02/E1 toggles remote mode; type 03 reads customer data.
fn pa_frame(total: u8, inner: u8, ty: u8, cmd: u8, flag: u8) -> [u8; 64] {
    let mut b = [0; 64];
    b[0] = 2;
    b[1] = 0x80;
    b[2] = total;
    b[5] = b'P';
    b[6] = b'A';
    b[7] = inner;
    b[9] = ty;
    b[10] = cmd;
    b[11] = flag;
    b
}
fn pa_remote(d: &HidDev, on: bool) -> bool {
    let ok = d.write(&pa_frame(7, 0x0e, 2, 0xe1, u8::from(on)), 500);
    sleep(Duration::from_millis(35));
    ok
}
fn parse_pa(r: &[u8], cmd: u8) -> Option<u8> {
    r.get(..16)?;
    if r.first() == Some(&2) && r.get(12) == Some(&cmd) && r.get(13) == Some(&1) && *r.get(14)? >= 1 {
        r.get(15).copied()
    } else {
        None
    }
}
fn pa_query(d: &HidDev, cmd: u8) -> Option<u8> {
    d.flush();
    pa_remote(d, true);
    let mut value = None;
    for _ in 0..3 {
        pa_remote(d, true);
        if !d.write(&pa_frame(8, 8, 3, cmd, 0), 500) {
            break;
        }
        let end = Instant::now() + Duration::from_millis(150);
        loop {
            let left = remaining(end);
            if left == 0 {
                break;
            }
            let Some(r) = d.read(left) else { break };
            if r.len() < 16 {
                break;
            }
            value = parse_pa(&r, cmd);
            if value.is_some() {
                break;
            }
        }
        if value.is_some() {
            break;
        }
    }
    pa_remote(d, false);
    value
}
fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &b in data {
        crc ^= u16::from(b) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}
fn barracuda_packet(group: u8, seq: u8, body: &[u8]) -> Option<[u8; 64]> {
    if body.len() > 55 {
        return None;
    }
    let mut f = [0; 64];
    f[0] = 2;
    f[1] = 0x80;
    f[2] = (4 + body.len()) as u8;
    f[5..9].copy_from_slice(&[0x50, 0x41, group, seq]);
    f.get_mut(9..9 + body.len())?.copy_from_slice(body);
    let crc = crc16(&f);
    f[3] = crc as u8;
    f[4] = (crc >> 8) as u8;
    Some(f)
}
// Reassemble PI frames across numbered input reports, discarding unrelated stream bytes.
fn take_frame(stream: &mut Vec<u8>) -> Option<Vec<u8>> {
    if stream.len() < 2 {
        return None;
    }
    let start = stream.windows(2).position(|w| w == [0x50, 0x49]).unwrap_or(stream.len());
    stream.drain(..start);
    let header = stream.get(..10)?;
    let len = 10 + usize::from(u16::from_le_bytes([*header.get(8)?, *header.get(9)?]));
    if stream.len() < len {
        return None;
    }
    Some(stream.drain(..len).collect())
}
fn input_payload(r: &[u8]) -> Option<&[u8]> {
    if r.first() != Some(&2) {
        return None;
    }
    r.get(2..2 + usize::from(*r.get(1)?))
}
struct BarracudaLink {
    dev: HidDev,
    stream: Vec<u8>,
    seq: u8,
}
impl BarracudaLink {
    fn command(&mut self, group: u8, body: &[u8]) -> bool {
        self.seq = self.seq % 127 + 1;
        let Some(f) = barracuda_packet(group, self.seq, body) else {
            return false;
        };
        for attempt in 0..5 {
            if attempt > 0 {
                sleep(Duration::from_millis(50));
            }
            if !self.dev.write(&f, 500) {
                return false;
            }
            let Some(status) = self.dev.get_input_report(2) else {
                return false;
            };
            match status.get(1) {
                Some(b'O') => return true,
                Some(b'F') => {}
                _ => return false,
            }
        }
        false
    }
    fn await_frame(&mut self, timeout: u64, matches: impl Fn(&[u8]) -> bool) -> Option<Vec<u8>> {
        let end = Instant::now() + Duration::from_millis(timeout);
        loop {
            while let Some(f) = take_frame(&mut self.stream) {
                if matches(&f) {
                    return Some(f);
                }
            }
            let left = remaining(end);
            if left == 0 {
                return None;
            }
            let r = self.dev.read(left)?;
            if r.len() < 2 {
                return None;
            }
            if let Some(payload) = input_payload(&r) {
                self.stream.extend_from_slice(payload);
            }
        }
    }
    fn remote(&mut self, on: bool) -> bool {
        if !self.command(0x0e, &[2, 0xe1, u8::from(on)]) {
            return false;
        }
        let seq = self.seq;
        self.await_frame(800, |r| r.len() >= 13 && r.get(10) == Some(&0x0e) && r.get(11) == Some(&(0x80 | seq)))
            .is_some_and(|r| r.get(12) == Some(&0))
    }
    fn get(&mut self, cmd: u8) -> Option<u8> {
        if !self.command(8, &[3, cmd, 0, 0]) {
            return None;
        }
        let r = self.await_frame(1000, |r| {
            r.len() >= 13 && r.get(2) == Some(&8) && r.get(10) == Some(&cmd) && r.get(11) == Some(&1)
        })?;
        if *r.get(12)? < 1 {
            return None;
        }
        r.get(13).copied()
    }
}
fn v3x_packet(txid: u8, cmd: u8) -> [u8; 64] {
    let mut r = [0; 64];
    r[0] = 7;
    r[2] = txid;
    r[6] = 2;
    r[7] = 7;
    r[8] = cmd;
    r[62] = r[3..61].iter().fold(0, |a, b| a ^ b);
    r
}
fn parse_v3x(r: &[u8], txid: u8, cmd: u8) -> Option<Result<u8, ()>> {
    r.get(..63)?;
    if r.get(2) != Some(&txid) || r.get(7) != Some(&7) || r.get(8) != Some(&cmd) || *r.get(6)? < 2 {
        return None;
    }
    if !matches!(r.get(1), Some(1 | 2)) {
        return Some(Err(()));
    }
    Some(Ok(*r.get(10)?))
}
impl RazerHeadsets {
    fn hs_query(&mut self, d: &HidDev, cmd: u8) -> Option<u8> {
        let seq = 0x60 | (self.hs_seq & 0x1f);
        self.hs_seq = self.hs_seq.wrapping_add(1);
        d.flush();
        if !d.write(&packet(0x80, cmd, seq), 500) {
            return None;
        }
        let end = Instant::now() + Duration::from_millis(650);
        loop {
            let left = remaining(end);
            if left == 0 {
                return None;
            }
            let r = d.read(left)?;
            if let Some(v) = parse_hs(&r, cmd, seq) {
                return Some(v);
            }
        }
    }
    fn poll_hyperspeed(&mut self, ctx: &mut PollCtx) {
        let pid = 0x0565;
        if ctx.is_claimed(VID, pid) {
            return;
        }
        let colls = ctx.colls;
        for page in [0xff14, 0xff00] {
            for c in colls.iter().filter(|c| {
                c.vid == VID && c.pid == pid && c.page == page && c.usage == 1 && c.out_len >= 64 && c.in_len >= 64
            }) {
                let Some(d) = HidDev::open(c) else {
                    continue;
                };
                let Some(level) = self.hs_query(&d, 0x21).filter(|v| *v <= 100) else {
                    continue;
                };
                let charging = self.hs_query(&d, 0x2a).is_some_and(|v| v > 0);
                ctx.add(base("blackshark", pid, "Razer BlackShark V2 HyperSpeed", level, charging), VID, pid);
                return;
            }
        }
    }
    fn v3x_get(&mut self, d: &HidDev, cmd: u8) -> Option<u8> {
        for _ in 0..4 {
            self.txid = self.txid.wrapping_add(1);
            if !d.set_feature(&v3x_packet(self.txid, cmd)) {
                return None;
            }
            sleep(Duration::from_millis(60));
            if let Some(r) = d.get_feature(7) {
                if let Some(value) = parse_v3x(&r, self.txid, cmd) {
                    return value.ok();
                }
            }
        }
        None
    }
    fn cached(&self, pid: u16) -> Option<Observation> {
        self.cache.get(&pid).filter(|c| c.at.elapsed() < MIN_INTERVAL).map(|c| c.status.clone())
    }
    fn store(&mut self, pid: u16, s: Observation, ctx: &mut PollCtx) {
        self.cache.insert(pid, Cached { at: Instant::now(), status: s.clone() });
        ctx.add(s, VID, pid);
    }
    fn poll_v3x(&mut self, ctx: &mut PollCtx) {
        for pid in [0x057d, 0x057c] {
            if ctx.is_claimed(VID, pid) {
                continue;
            }
            let colls = ctx.colls;
            let Some(c) = colls.iter().find(|c| c.vid == VID && c.pid == pid && c.iface == 0 && c.feat_len >= 64)
            else {
                continue;
            };
            if let Some(s) = self.cached(pid) {
                ctx.add(s, VID, pid);
                continue;
            }
            let Some(d) = HidDev::open_feature_only(c) else {
                continue;
            };
            let Some(level) = self.v3x_get(&d, 0x80).filter(|v| *v <= 100) else {
                continue;
            };
            let charging = self.v3x_get(&d, 0x84) == Some(1);
            let s = base("blackshark", pid, "Razer BlackShark V3 X", level, charging).via(if pid == 0x057d {
                "2.4 GHz receiver"
            } else {
                "USB cable"
            });
            self.store(pid, s, ctx);
        }
    }
    fn poll_v3(&mut self, ctx: &mut PollCtx) {
        for pid in [0x0576, 0x0577, 0x0579, 0x057a] {
            if ctx.is_claimed(VID, pid) {
                continue;
            }
            let colls = ctx.colls;
            let Some(c) = colls
                .iter()
                .find(|c| c.vid == VID && c.pid == pid && c.iface == 5 && c.out_len >= 64 && c.in_len >= 15)
            else {
                continue;
            };
            if let Some(s) = self.cached(pid) {
                ctx.add(s, VID, pid);
                continue;
            }
            let Some(d) = HidDev::open(c) else {
                continue;
            };
            // Required init and charging-state handshake on both links.
            d.write(&packet(0, 2, 0x60), 500);
            d.write(&packet(0, 0x2a, 0x60), 500);
            d.write(&packet(0x80, 0x2a, 0x60), 500);
            sleep(Duration::from_millis(250));
            d.flush();
            let Some((level, charging)) = get_v3(&d, 0x21).filter(|(v, _)| *v <= 100) else {
                continue;
            };
            let name = if [0x0576, 0x0577].contains(&pid) { "Razer BlackShark V3 Pro" } else { "Razer BlackShark V3" };
            self.store(pid, base("blackshark", pid, name, level, charging), ctx);
        }
    }
}
fn poll_2020(ctx: &mut PollCtx) {
    let pid = 0x0528;
    if ctx.is_claimed(VID, pid) {
        return;
    }
    let colls = ctx.colls;
    for c in colls.iter().filter(|c| {
        c.vid == VID && c.pid == pid && c.iface == 3 && c.page == 0xff00 && c.usage == 1 && c.feat_len == 64
    }) {
        let Some(d) = HidDev::open_feature_only(c) else {
            continue;
        };
        if !d.set_feature(&[0xff, 0x0a, 0, 0xfd, 4, 0x12, 0xf1, 2, 5]) {
            continue;
        }
        for read in 0..5 {
            sleep(Duration::from_millis(100));
            let Some((state, mv)) = d.get_feature(0xff).and_then(|r| parse_2020(&r, read == 4)) else {
                continue;
            };
            let mut s = base("blackshark", pid, "Razer BlackShark V2 Pro (2020)", 0, false).millivolts(mv);
            s.state = state;
            ctx.add(s, VID, pid);
            return;
        }
    }
}
fn poll_2023(ctx: &mut PollCtx) {
    let pid = 0x0555;
    if ctx.is_claimed(VID, pid) {
        return;
    }
    let colls = ctx.colls;
    for c in
        colls.iter().filter(|c| c.vid == VID && c.pid == pid && c.page == 0xff00 && c.out_len >= 64 && c.in_len >= 16)
    {
        let Some(d) = HidDev::open(c) else {
            continue;
        };
        let Some(level) = pa_query(&d, 0x21).filter(|v| *v <= 100) else {
            continue;
        };
        let charge = pa_query(&d, 0x2a).is_some_and(|v| v > 0);
        ctx.add(base("blackshark", pid, "Razer BlackShark V2 Pro (2023)", level, charge), VID, pid);
        return;
    }
}
fn poll_barracuda(ctx: &mut PollCtx) {
    let pid = 0x0550;
    if ctx.is_claimed(VID, pid) {
        return;
    }
    let colls = ctx.colls;
    for c in colls.iter().filter(|c| {
        c.vid == VID && c.pid == pid && c.page == 0xff00 && c.usage == 1 && c.out_len >= 64 && c.in_len >= 64
    }) {
        let Some(dev) = HidDev::open(c) else {
            continue;
        };
        let mut link = BarracudaLink { dev, stream: Vec::new(), seq: 0 };
        if !link.remote(true) {
            continue;
        }
        let level = link.get(0x21);
        let cable = if level.is_some() { link.get(0x2a) } else { None };
        link.remote(false);
        let Some(level) = level.filter(|v| *v <= 100) else {
            continue;
        };
        ctx.add(base("barracuda", pid, "Razer Barracuda X (2022)", level, cable == Some(1)), VID, pid);
        return;
    }
}
impl Driver for RazerHeadsets {
    fn name(&self) -> &'static str {
        "razer_headset"
    }
    fn poll(&mut self, ctx: &mut PollCtx) {
        self.poll_hyperspeed(ctx);
        poll_barracuda(ctx);
        poll_2020(ctx);
        poll_2023(ctx);
        self.poll_v3x(ctx);
        self.poll_v3(ctx);
    }
}
pub fn driver() -> Box<dyn Driver> {
    Box::new(RazerHeadsets::default())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mediatek_frames() {
        let p = packet(0x80, 0x21, 0x60);
        assert_eq!(p[62], p[..62].iter().fold(0, |a, b| a ^ b));
        let mut r = p;
        r[12] = 1;
        r[13] = 70;
        r[14] = 1;
        r[62] = r[1..62].iter().fold(0, |a, b| a ^ b);
        assert_eq!(parse_hs(&r, 0x21, 0x60), Some(70));
        assert_eq!(parse_hs(&r, 0x21, 0x61), None);
        assert_eq!(parse_hs(&r[..63], 0x21, 0x60), None);
        assert_eq!(parse_v3(&r, 0x21), Some((70, true)));
        r[62] ^= 1;
        assert_eq!(parse_hs(&r, 0x21, 0x60), None);
        assert_eq!(parse_v3(&[], 0x21), None);
    }
    #[test]
    fn feature_2020_states() {
        let mut r = [0; 64];
        r[..9].copy_from_slice(&[0xff, 0x0f, 5, 0xfe, 0x12, 4, 0x1f, 8, 5]);
        r[12] = 0x0f;
        r[13] = 0xa4;
        r[14] = 70;
        assert_eq!(parse_2020(&r, false), Some((State::Battery { percent: 70, charging: false }, 0x0fa4)));
        r[11] = 9;
        assert_eq!(parse_2020(&r, false), Some((State::Unreachable, 0x0fa4)));
        r[1] = 1;
        r[2] = 0;
        assert_eq!(parse_2020(&r, false), None);
        assert_eq!(parse_2020(&r, true), Some((State::Off, 0)));
        assert_eq!(parse_2020(&r[..63], true), None);
    }
    #[test]
    fn pa_and_v3x_replies() {
        let mut r = [0; 64];
        r[0] = 2;
        r[12] = 0x21;
        r[13] = 1;
        r[14] = 1;
        r[15] = 70;
        assert_eq!(parse_pa(&r, 0x21), Some(70));
        assert_eq!(parse_pa(&r, 0x2a), None);
        assert_eq!(parse_pa(&r[..15], 0x21), None);
        let mut r = v3x_packet(0x41, 0x80);
        r[1] = 2;
        r[10] = 80;
        assert_eq!(parse_v3x(&r, 0x41, 0x80), Some(Ok(80)));
        r[1] = 1;
        assert_eq!(parse_v3x(&r, 0x41, 0x80), Some(Ok(80)));
        r[1] = 4;
        assert_eq!(parse_v3x(&r, 0x41, 0x80), Some(Err(())));
        assert_eq!(parse_v3x(&r, 0x42, 0x80), None);
        assert_eq!(parse_v3x(&r[..62], 0x41, 0x80), None);
    }
    #[test]
    fn barracuda_crc_and_fragmentation() {
        assert_eq!(crc16(b"123456789"), 0x31c3);
        let f = barracuda_packet(8, 1, &[3, 0x21, 0, 0]).unwrap();
        let crc = u16::from_le_bytes([f[3], f[4]]);
        let mut zero = f;
        zero[3] = 0;
        zero[4] = 0;
        assert_eq!(crc16(&zero), crc);
        let mut stream = vec![0, 0x50, 0x49, 8, 0, 0, 0, 0, 0, 4, 0, 0x21];
        assert!(take_frame(&mut stream).is_none());
        stream.extend_from_slice(&[1, 1, 70]);
        let frame = take_frame(&mut stream).unwrap();
        assert_eq!(frame.len(), 14);
        assert_eq!(frame.get(13), Some(&70));
        assert!(stream.is_empty());
        assert_eq!(input_payload(&[2, 2, 0x50, 0x49]), Some(&[0x50, 0x49][..]));
        assert_eq!(input_payload(&[2, 4, 0]), None);
    }
    #[test]
    fn five_minute_cache() {
        let mut h = RazerHeadsets::default();
        let status = base("blackshark", 0x0576, "Razer BlackShark V3 Pro", 70, false);
        h.cache.insert(0x0576, Cached { at: Instant::now(), status: status.clone() });
        assert_eq!(h.cached(0x0576), Some(status));
        h.cache.get_mut(&0x0576).unwrap().at = Instant::now() - MIN_INTERVAL;
        assert!(h.cached(0x0576).is_none());
    }
}
