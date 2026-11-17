use super::{simple, Driver, PollCtx};
use crate::{
    device::{Kind, Observation},
    hid::{self, HidDev},
};
use std::time::{Duration, Instant};

// BY receivers: AULA F75/F87/F99 and MCHOSE keyboards. Commands 05 (model) and 4A (battery).
// Protocol: PyFlat/Device-Battery-Info, deepan-alve/womier-l65-linux, ushineko/sanshoku #36,
// MCHOSE M HUB getPassWord/getBattery. MCHOSE may omit the checksum on a single-packet reply.
const RECEIVERS: &[(u16, u16)] = &[(0x3554, 0xfa09), (0x3554, 0xfb40), (0x41e4, 0x2001)];
const MODELS: &[([u8; 6], &str)] = &[
    ([3, 0, 0, 0, 2, 0x1c], "MCHOSE G87"),
    ([3, 0, 0, 0, 0, 0x77], "MCHOSE K99"),
    ([3, 0, 0, 0, 1, 0xdf], "MCHOSE K99 V2"),
    ([3, 0, 0, 0, 1, 0x32], "MCHOSE G98"),
    ([3, 0, 0, 0, 0, 0xd6], "MCHOSE X75"),
    ([10, 0, 0, 0, 0, 3], "MCHOSE G98 Pro"),
    ([3, 0, 0, 0, 1, 0xc6], "MCHOSE X75 V2"),
    ([3, 0, 0, 0, 1, 0x15], "MCHOSE G75 Pro"),
    ([10, 0, 0, 0, 0, 0x18], "MCHOSE G98 V2"),
    ([10, 0, 0, 0, 0, 0x2b], "MCHOSE UT98"),
    ([10, 0, 0, 0, 0, 1], "MCHOSE Z75"),
    ([10, 0, 0, 0, 0, 9], "MCHOSE Z75S"),
    ([3, 0, 0, 0, 1, 0xc7], "MCHOSE KX75"),
    ([3, 0, 0, 0, 1, 0x74], "MCHOSE GX87"),
    ([3, 0, 0, 0, 1, 0xed], "MCHOSE GX87 V2"),
    ([3, 0, 0, 0, 1, 0x35], "MCHOSE K87"),
    ([3, 0, 0, 0, 3, 0x35], "MCHOSE G75 V2"),
    ([10, 0, 0, 0, 0, 0x34], "MCHOSE G87 V2"),
];
fn sum(r: &[u8]) -> u8 {
    r.iter().fold(0u8, |s, b| s.wrapping_add(*b))
}
fn request(cmd: u8, packets: u8) -> [u8; 20] {
    let mut r = [0; 20];
    r[0] = 0x13;
    r[1] = cmd;
    r[2] = packets;
    r[19] = sum(&r[..19]);
    r
}
fn parse_by(r: &[u8], cmd: u8) -> Option<&[u8]> {
    let frame = r.get(..20)?;
    if frame.first() != Some(&0x13) || *frame.get(1)? & 0x7f != cmd || frame.get(4..19)?.iter().all(|b| *b == 0) {
        return None;
    }
    let checksum = frame.get(19) == Some(&sum(frame.get(..19)?));
    let mchose = frame.get(19) == Some(&0) && frame.get(2) == Some(&1) && frame.get(3) == Some(&0);
    (checksum || mchose).then_some(frame)
}
fn model_name(r: &[u8]) -> Option<&'static str> {
    for off in [5, 4] {
        for (code, name) in MODELS {
            if r.get(off..off + 6) == Some(code.as_slice()) {
                return Some(name);
            }
        }
    }
    None
}
fn parse_by_battery(r: &[u8]) -> Option<(u8, bool)> {
    let p = *r.get(5)?;
    (1..=100).contains(&p).then_some((p, *r.get(6)? >> 4 == 1))
}
fn exchange(d: &HidDev, cmd: u8, total_ms: u64) -> Option<Vec<u8>> {
    let start = Instant::now();
    let end = start + Duration::from_millis(total_ms);
    let mut sent = 0u64;
    d.flush();
    loop {
        let now = Instant::now();
        if now >= end {
            return None;
        }
        if now >= start + Duration::from_millis(sent * 1300) {
            if !d.write(&request(cmd, (sent & 1) as u8), 500) {
                return None;
            }
            sent += 1;
        }
        let next = (start + Duration::from_millis(sent * 1300)).min(end);
        let wait = next.saturating_duration_since(now).as_millis().max(1) as u32;
        let before = Instant::now();
        let r = d.read(wait);
        if r.is_none() && before.elapsed().as_millis() + 20 < u128::from(wait) {
            return None;
        }
        if let Some(r) = r {
            if parse_by(&r, cmd).is_some() {
                return Some(r);
            }
        }
    }
}

// HERO 68 Air, captured AULA Hub protocol (#201): four acknowledged 14-byte data frames,
// then a trailer. Counter bits span bytes 2..4. Only a read-only battery message is sent.
type Frame = [u8; 20];
fn hero_header(counter: u8, index: u8, kind: u8) -> Frame {
    let mut f = [0; 20];
    f[0] = 9;
    f[1] = 0x66;
    f[2] = 5 | if counter & 4 != 0 { 0x80 } else { 0 };
    f[3] = index | if counter & 2 != 0 { 0x80 } else { 0 };
    f[4] = kind | if counter & 1 != 0 { 0x80 } else { 0 };
    f
}
fn hero_data(counter: u8, index: u8, msg: &[u8; 56]) -> Frame {
    let mut f = hero_header(counter, index, 0x0e);
    if let Some(data) = msg.get(usize::from(index.saturating_sub(1)) * 14..usize::from(index) * 14) {
        f[5..19].copy_from_slice(data);
    }
    f[19] = sum(&f[1..19]);
    f
}
fn hero_trailer(counter: u8, msg: &[u8; 56]) -> Frame {
    let mut f = hero_header(counter, 5, 8);
    f[12] = 0xffu8.wrapping_sub(sum(msg));
    f[13] = sum(&f[1..13]);
    f
}
fn hero_frame(r: &[u8]) -> Option<Frame> {
    let f: Frame = r.get(..20)?.try_into().ok()?;
    (f[0] == 9 && f[1] == 0x66).then_some(f)
}
fn hero_ack(r: &Frame) -> Frame {
    let mut f = [0; 20];
    f[0] = 9;
    f[1] = 0x66;
    f[2] = r[2];
    f[3] = r[3];
    f[4] = r[4] & 0x80;
    f[5] = sum(&f[1..5]);
    f
}
fn is_ack(r: &Frame, f: &Frame) -> bool {
    r[4] & 0x7f == 0 && r[2] == f[2] && r[3] == f[3] && r[4] & 0x80 == f[4] & 0x80 && r[5] == sum(&r[1..5])
}
fn hero_battery(r: &Frame, have_level: bool) -> Option<(u8, u8, bool)> {
    if r[5] == 0x87 && (1..=100).contains(&r[11]) {
        Some((r[11], r[12], true))
    } else if r[5] == 0xfe && r[6] == 5 && (1..=100).contains(&r[7]) && !have_level {
        Some((r[7], r[8], false))
    } else {
        None
    }
}
struct HeroLink<'a> {
    d: &'a HidDev,
    end: Instant,
    level: Option<u8>,
    flags: u8,
    in_reply: bool,
    reply_done: bool,
    io_error: bool,
}
impl<'a> HeroLink<'a> {
    fn remaining(&self) -> u32 {
        self.end.saturating_duration_since(Instant::now()).as_millis() as u32
    }
    fn inspect(&mut self, r: &Frame) {
        self.in_reply = false;
        if let Some((level, flags, in_reply)) = hero_battery(r, self.level.is_some()) {
            self.level = Some(level);
            self.flags = flags;
            self.in_reply = in_reply;
        }
    }
    fn read_frame(&mut self, mut wait: u32) -> Option<Frame> {
        loop {
            if wait == 0 {
                return None;
            }
            let before = Instant::now();
            let r = self.d.read(wait);
            let took = before.elapsed().as_millis() as u32;
            if r.is_none() && took.saturating_add(20) < wait {
                self.io_error = true;
                return None;
            }
            let r = r?;
            wait = wait.saturating_sub(took);
            let Some(f) = hero_frame(&r) else {
                continue;
            };
            match f[4] & 0x7f {
                0x0e => {
                    if f[19] != sum(&f[1..19]) {
                        continue;
                    }
                    self.d.write(&hero_ack(&f), 500);
                    if f[3] & 0x7f == 1 {
                        self.inspect(&f);
                    }
                }
                7 => {
                    self.d.write(&hero_ack(&f), 500);
                    if self.in_reply {
                        self.reply_done = true;
                    }
                    self.in_reply = false;
                }
                _ => {}
            }
            return Some(f);
        }
    }
    fn send_frame(&mut self, f: &Frame) -> bool {
        while self.remaining() > 0 {
            if !self.d.write(f, 500) {
                return false;
            }
            let resend = Instant::now() + Duration::from_millis(150);
            loop {
                let wait = (resend.saturating_duration_since(Instant::now()).as_millis() as u32).min(self.remaining());
                if wait == 0 {
                    break;
                }
                match self.read_frame(wait) {
                    Some(r) => {
                        if is_ack(&r, f) {
                            return true;
                        }
                    }
                    None => {
                        if self.io_error {
                            return false;
                        }
                        break;
                    }
                }
            }
        }
        false
    }
    fn send_message(&mut self, msg: &[u8; 56]) -> bool {
        let mut counter = 1;
        for i in 1..=4 {
            if !self.send_frame(&hero_data(counter, i, msg)) {
                return false;
            }
            counter = (counter + 1) & 7;
        }
        self.send_frame(&hero_trailer(counter, msg))
    }
    fn wait_for_battery(&mut self) -> Option<(u8, bool)> {
        while !(self.level.is_some() && self.reply_done) {
            if self.read_frame(self.remaining()).is_none() {
                break;
            }
        }
        self.level.map(|p| (p, self.flags & 0x10 != 0))
    }
}
fn query_hero(d: &HidDev) -> Option<(u8, bool)> {
    let mut msg = [0; 56];
    msg[0] = 9;
    msg[1] = 0x87;
    msg[4] = 1;
    d.flush();
    let mut link = HeroLink {
        d,
        end: Instant::now() + Duration::from_millis(1000),
        level: None,
        flags: 0,
        in_reply: false,
        reply_done: false,
        io_error: false,
    };
    if !link.send_message(&msg) {
        return None;
    }
    link.wait_for_battery()
}
fn poll(ctx: &mut PollCtx) {
    for &(vid, pid) in RECEIVERS {
        if ctx.is_claimed(vid, pid) {
            continue;
        }
        for c in ctx
            .find(vid, pid)
            .filter(|c| matches!(c.page, 0xff02 | 0xff04) && c.usage == 2 && c.out_len >= 20 && c.in_len >= 20)
        {
            let Some(d) = HidDev::open(c) else {
                continue;
            };
            let product = hid::product(c);
            let generic = product.contains("2.4G Wireless Receiver") || product.contains("MCHOSE 2.4G Wireless");
            let model = if generic { exchange(&d, 5, 1300).and_then(|r| model_name(&r)) } else { None };
            let mchose = vid == 0x41e4 || model.is_some();
            let Some(r) = exchange(&d, 0x4a, 4000) else {
                continue;
            };
            let Some((p, ch)) = parse_by_battery(&r) else {
                continue;
            };
            let name = model.unwrap_or(if mchose { "MCHOSE wireless keyboard" } else { "AULA wireless keyboard" });
            ctx.add(
                Observation::battery(format!("aula:{vid:04x}:{pid:04x}"), name, Kind::Keyboard, p, ch)
                    .via("2.4 GHz receiver"),
                vid,
                pid,
            );
            break;
        }
    }
    for (vid, pid) in ctx.products(0x372e) {
        if pid != 0x106c {
            continue;
        }
        for c in ctx.find(vid, pid).filter(|c| c.page == 0xff60 && c.usage == 0x61 && c.out_len >= 20 && c.in_len >= 20)
        {
            let Some(d) = HidDev::open(c) else {
                continue;
            };
            let Some((p, ch)) = query_hero(&d) else {
                continue;
            };
            ctx.add(
                Observation::battery(
                    format!("aula:{vid:04x}:{pid:04x}"),
                    "AULA wireless keyboard",
                    Kind::Keyboard,
                    p,
                    ch,
                )
                .via("2.4 GHz receiver"),
                vid,
                pid,
            );
            break;
        }
    }
}
pub fn driver() -> Box<dyn Driver> {
    simple("Aula", poll)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn by_checksum_echo_and_model() {
        let mut r = request(0x4a, 0);
        assert_eq!(parse_by(&r, 0x4a), None);
        r[5] = 75;
        r[6] = 0x10;
        r[19] = sum(&r[..19]);
        assert!(parse_by(&r, 0x4a).is_some());
        assert_eq!(parse_by_battery(&r), Some((75, true)));
        for n in 0..20 {
            assert_eq!(parse_by(&r[..n], 0x4a), None);
        }
        assert_eq!(parse_by(&r, 5), None);
        r[19] ^= 1;
        assert_eq!(parse_by(&r, 0x4a), None);
        r[19] = 0;
        r[2] = 1;
        assert!(parse_by(&r, 0x4a).is_some());
        r[3] = 1;
        assert_eq!(parse_by(&r, 0x4a), None);
        r[5] = 101;
        assert_eq!(parse_by_battery(&r), None);
        for off in [5, 4] {
            for (code, name) in MODELS {
                let mut r = [0; 20];
                r[off..off + 6].copy_from_slice(code);
                assert_eq!(model_name(&r), Some(*name));
            }
        }
        assert_eq!(model_name(&[]), None);
    }
    #[test]
    fn hero_frames_ack_and_payload() {
        let mut msg = [0; 56];
        msg[0] = 9;
        msg[1] = 0x87;
        msg[4] = 1;
        let data = hero_data(1, 1, &msg);
        assert_eq!(&data[..11], &[9, 0x66, 5, 1, 0x8e, 9, 0x87, 0, 0, 1, 0]);
        assert_eq!(data[19], sum(&data[1..19]));
        let trailer = hero_trailer(5, &msg);
        assert_eq!(trailer[12], 0xffu8.wrapping_sub(sum(&msg)));
        assert_eq!(trailer[13], sum(&trailer[1..13]));
        let mut ack = hero_ack(&data);
        assert!(is_ack(&ack, &data));
        ack[5] ^= 1;
        assert!(!is_ack(&ack, &data));
        for n in 0..20 {
            assert_eq!(hero_frame(&data[..n]), None);
        }
        let mut reply = hero_header(1, 1, 0x0e);
        reply[5] = 0x87;
        reply[11] = 80;
        reply[12] = 0x10;
        assert_eq!(hero_battery(&reply, false), Some((80, 0x10, true)));
        reply[11] = 0;
        assert_eq!(hero_battery(&reply, false), None);
        reply[5] = 0xfe;
        reply[6] = 5;
        reply[7] = 42;
        reply[8] = 0;
        assert_eq!(hero_battery(&reply, false), Some((42, 0, false)));
        assert_eq!(hero_battery(&reply, true), None);
    }
}
