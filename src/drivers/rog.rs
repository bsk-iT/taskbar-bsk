use super::{Driver, PollCtx};
use crate::device::{Kind, Observation, State};
use crate::hid::HidDev;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

// peripheral-battery-tray / BatteryTool: [rid 12 sub], sub 01 keyboard / 07 mouse.
// Offsets include report ID. FF AA means asleep; older mouse gauges use steps of 25%.
struct Model {
    pid: u16,
    name: &'static str,
    kind: Kind,
    rid: u8,
    iface: i32,
    col: Option<&'static str>,
    offset: usize,
    scale: u16,
}
const KB: Kind = Kind::Keyboard;
const MS: Kind = Kind::Mouse;
macro_rules! model {
    ($pid:expr, $name:expr, $kind:expr, $rid:expr, $iface:expr, $col:expr, $offset:expr, $scale:expr) => {
        Model {
            pid: $pid,
            name: $name,
            kind: $kind,
            rid: $rid,
            iface: $iface,
            col: $col,
            offset: $offset,
            scale: $scale,
        }
    };
}
const MODELS: &[Model] = &[
    model!(0x1a85, "ROG Azoth", KB, 2, 2, Some("col02"), 6, 1),
    model!(0x1a85, "ROG Azoth", KB, 0, 1, None, 6, 1),
    model!(0x1a83, "ROG Azoth", KB, 0, 1, None, 6, 1),
    model!(0x1b42, "ROG Azoth Extreme", KB, 2, 2, Some("col02"), 6, 1),
    model!(0x1b3f, "ROG Azoth Extreme", KB, 0, 1, None, 6, 1),
    model!(0x1cf1, "ROG Azoth Extreme SE", KB, 2, 2, Some("col02"), 6, 1),
    model!(0x1cef, "ROG Azoth Extreme SE", KB, 0, 1, None, 6, 1),
    model!(0x1c25, "ROG Azoth X", KB, 0, 1, None, 6, 1),
    model!(0x1c24, "ROG Azoth X", KB, 0, 1, None, 6, 1),
    model!(0x193e, "ROG Falchion", KB, 0, 1, None, 11, 1),
    model!(0x193c, "ROG Falchion", KB, 0, 1, None, 11, 1),
    model!(0x1b04, "ROG Falchion RX", KB, 0, -1, None, 11, 1),
    model!(0x1b06, "ROG Falchion RX Low Profile", KB, 2, 2, Some("col02"), 11, 1),
    model!(0x1ab0, "ROG Strix Scope II 96", KB, 2, 2, Some("col02"), 6, 1),
    model!(0x1aae, "ROG Strix Scope II 96", KB, 0, 1, None, 6, 1),
    model!(0x1b7a, "ROG Strix Scope II 96 RX", KB, 2, 2, Some("col02"), 6, 1),
    model!(0x1b78, "ROG Strix Scope II 96 RX", KB, 0, 1, None, 6, 1),
    model!(0x1a07, "ROG Strix Scope RX TKL", KB, 0, -1, None, 6, 1),
    model!(0x18e5, "ROG Chakram", MS, 0, 0, None, 5, 25),
    model!(0x18e3, "ROG Chakram", MS, 0, 0, None, 5, 25),
    model!(0x1a1a, "ROG Chakram X", MS, 0, 0, None, 5, 1),
    model!(0x1a18, "ROG Chakram X", MS, 0, 0, None, 5, 1),
    model!(0x18a0, "ROG Gladius II Wireless", MS, 0, 2, None, 5, 25),
    model!(0x197f, "ROG Gladius III Wireless", MS, 0, 0, None, 5, 1),
    model!(0x197d, "ROG Gladius III Wireless", MS, 0, 0, None, 5, 1),
    model!(0x1a72, "ROG Gladius III Aimpoint", MS, 3, 2, Some("col03"), 5, 1),
    model!(0x1a72, "ROG Gladius III Aimpoint", MS, 0, 0, None, 5, 1),
    model!(0x1a70, "ROG Gladius III Aimpoint", MS, 0, 0, None, 5, 1),
    model!(0x1b0c, "ROG Gladius III Aimpoint EVA-02", MS, 0, 0, None, 5, 1),
    model!(0x1b0a, "ROG Gladius III Aimpoint EVA-02", MS, 0, 0, None, 5, 1),
    model!(0x1a94, "ROG Harpe Ace Aim Lab Edition", MS, 3, 2, Some("col03"), 5, 1),
    model!(0x1a94, "ROG Harpe Ace Aim Lab Edition", MS, 0, 0, None, 5, 1),
    model!(0x1a92, "ROG Harpe Ace Aim Lab Edition", MS, 0, 0, None, 5, 1),
    model!(0x1b68, "ROG Harpe Ace Extreme", MS, 3, 2, Some("col03"), 5, 1),
    model!(0x1b69, "ROG Harpe Ace Extreme", MS, 3, 2, Some("col03"), 5, 1),
    model!(0x1b67, "ROG Harpe Ace Extreme", MS, 0, 0, None, 5, 1),
    model!(0x1b65, "ROG Harpe Ace Mini", MS, 3, 2, Some("col03"), 5, 1),
    model!(0x1b63, "ROG Harpe Ace Mini", MS, 0, 0, None, 5, 1),
    model!(0x1ad0, "ROG Harpe II Ace", MS, 3, 2, Some("col03"), 5, 1),
    model!(0x1c69, "ROG Harpe II Ace", MS, 0, 0, None, 5, 1),
    model!(0x1960, "ROG Keris Wireless", MS, 0, 0, None, 5, 25),
    model!(0x195e, "ROG Keris Wireless", MS, 0, 0, None, 5, 25),
    model!(0x1a59, "ROG Keris Wireless EVA Edition", MS, 0, 0, None, 5, 25),
    model!(0x1a57, "ROG Keris Wireless EVA Edition", MS, 0, 0, None, 5, 25),
    model!(0x1a68, "ROG Keris Wireless Aimpoint", MS, 3, 2, Some("col03"), 5, 1),
    model!(0x1a6a, "ROG Keris Wireless Aimpoint", MS, 3, 2, Some("col03"), 5, 1),
    model!(0x1a68, "ROG Keris Wireless Aimpoint", MS, 0, 0, None, 5, 1),
    model!(0x1a66, "ROG Keris Wireless Aimpoint", MS, 0, 0, None, 5, 1),
    model!(0x1b1a, "ROG Keris II Ace", MS, 3, 2, Some("col03"), 5, 1),
    model!(0x1b18, "ROG Keris II Ace", MS, 3, 2, Some("col03"), 5, 1),
    model!(0x1b16, "ROG Keris II Ace", MS, 0, 0, None, 5, 1),
    model!(0x1c0e, "ROG Keris II Origin", MS, 3, 2, Some("col03"), 5, 1),
    model!(0x1c0c, "ROG Keris II Origin", MS, 0, 0, None, 5, 1),
    model!(0x1d4e, "ROG Keris II Origin KJP", MS, 3, 2, Some("col03"), 5, 1),
    model!(0x1d4c, "ROG Keris II Origin KJP", MS, 0, 0, None, 5, 1),
    model!(0x1908, "ROG Pugio II", MS, 0, 0, None, 5, 25),
    model!(0x1906, "ROG Pugio II", MS, 0, 0, None, 5, 25),
    model!(0x1979, "ROG Spatha X", MS, 0, 0, None, 5, 1),
    model!(0x1977, "ROG Spatha X", MS, 0, 0, None, 5, 1),
    model!(0x18b4, "ROG Strix Carry", MS, 0, 1, None, 7, 25),
    model!(0x1949, "ROG Strix Impact II Wireless", MS, 0, 0, None, 5, 25),
    model!(0x1947, "ROG Strix Impact II Wireless", MS, 0, 0, None, 5, 25),
    model!(0x1ad7, "ROG Strix Impact III Wireless", MS, 3, 2, Some("col03"), 5, 1),
];
#[derive(Debug, PartialEq)]
enum Reply {
    Ok(Vec<u8>),
    Asleep,
    None,
}
fn parse_reply(mut r: Vec<u8>, rid: u8, cmd: u8, sub: u8) -> Reply {
    if r.len() < 11 {
        return Reply::None;
    }
    if rid == 0 {
        r.insert(0, 0);
    }
    if r.len() < 12 {
        return Reply::None;
    }
    if r.get(1..3) == Some(&[0xff, 0xaa]) {
        return Reply::Asleep;
    }
    if r.get(1..3) == Some(&[cmd, sub]) {
        Reply::Ok(r)
    } else {
        Reply::None
    }
}
fn query(d: &HidDev, rid: u8, cmd: u8, sub: u8) -> Reply {
    d.flush();
    if !d.write(&[rid, cmd, sub], 500) {
        return Reply::None;
    }
    let end = Instant::now() + Duration::from_millis(500);
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        let Some(r) = d.read(left.as_millis() as u32) else {
            return Reply::None;
        };
        if r.len() < 11 {
            return Reply::None;
        }
        let res = parse_reply(r, rid, cmd, sub);
        if res != Reply::None {
            return res;
        }
    }
    Reply::None
}
fn model_state(r: &[u8], m: &Model) -> Option<State> {
    let v = *r.get(m.offset)? as u16 * m.scale;
    if m.kind == KB {
        Some(if v <= 100 {
            State::Battery { percent: v as u8, charging: r.get(9) == Some(&1) }
        } else {
            State::Unreachable
        })
    } else {
        Some(if v > 0 && v <= 100 {
            State::Battery { percent: v as u8, charging: r.get(10).is_some_and(|&c| c != 0) }
        } else {
            State::Unreachable
        })
    }
}
fn read_model(d: &HidDev, m: &Model) -> Option<(Observation, bool)> {
    let (state, asleep) = match query(d, m.rid, 0x12, if m.kind == KB { 1 } else { 7 }) {
        Reply::None => return None,
        Reply::Asleep => (State::Unreachable, true),
        Reply::Ok(r) => {
            let state = model_state(&r, m)?;
            let asleep = m.kind == MS && state == State::Unreachable;
            (state, asleep)
        }
    };
    Some((
        Observation::new(format!("rog:0b05:{:04x}:{}", m.pid, if m.kind == KB { "kb" } else { "ms" }), state)
            .name(m.name)
            .kind(m.kind)
            .via(if m.rid != 0 { "ROG Omni Receiver" } else { "2.4 GHz receiver / USB" }),
        asleep,
    ))
}
fn path_has(path: &[u16], tag: &str) -> bool {
    String::from_utf16_lossy(path).to_lowercase().contains(tag)
}
fn add_once(ctx: &mut PollCtx, seen: &mut Vec<&'static str>, m: &Model, s: Observation) {
    if !seen.contains(&m.name) {
        seen.push(m.name);
        ctx.out.push(s);
    }
}
fn poll_omni(ctx: &mut PollCtx, seen: &mut Vec<&'static str>) {
    if ctx.is_claimed(0x0b05, 0x1ace) {
        return;
    }
    let Some(c) = ctx.find(0x0b05, 0x1ace).find(|c| c.iface == 2 && c.out_len >= 12 && path_has(&c.path, "col01"))
    else {
        return;
    };
    let Some(d) = HidDev::open(c) else {
        return;
    };
    let Reply::Ok(r) = query(&d, 1, 0xa0, 0) else {
        return;
    };
    ctx.claim(0x0b05, 0x1ace);
    // Omni lists product IDs in four-byte slots starting at byte 5.
    for at in (5..r.len()).step_by(4) {
        let Some(pair) = r.get(at..at + 2) else {
            break;
        };
        let Some((&lo, tail)) = pair.split_first() else {
            break;
        };
        let Some(&hi) = tail.first() else {
            break;
        };
        let pid = u16::from_le_bytes([lo, hi]);
        if pid == 0 {
            break;
        }
        let Some(m) = MODELS.iter().find(|m| m.pid == pid && m.rid != 0) else {
            continue;
        };
        for c in ctx
            .find(0x0b05, 0x1ace)
            .filter(|c| c.iface == 2 && c.out_len >= 12 && m.col.is_some_and(|col| path_has(&c.path, col)))
        {
            let Some(dev) = HidDev::open(c) else {
                continue;
            };
            if let Some((s, _)) = read_model(&dev, m) {
                add_once(ctx, seen, m, s);
                break;
            }
        }
    }
}
// Pelta has no interrupt OUT pipe: SetOutputReport with a read already pending.
// CC 12 07: battery byte 6 (FE unknown); CC 12 08: charging byte 5. FF AA = asleep.
fn parse_pelta(r: &[u8], cmd: u8) -> Reply {
    if r.len() < 8 {
        return Reply::None;
    }
    if r.get(..3) == Some(&[0xcc, 0xff, 0xaa]) {
        return Reply::Asleep;
    }
    if r.get(..2) == Some(&[0xcc, 0x12]) {
        if r.get(5..7) == Some(&[0xff, 0xaa]) {
            return Reply::Asleep;
        }
        if r.get(2) == Some(&cmd) {
            return Reply::Ok(r.to_vec());
        }
    }
    Reply::None
}
fn pelta_query(d: &HidDev, cmd: u8) -> Reply {
    d.flush();
    // The first reply after reconnect can take 750 ms.
    let end = Instant::now() + Duration::from_millis(1200);
    let mut r = d.set_output_and_read(&[0xcc, 0x12, cmd], 1200);
    while let Some(bytes) = r {
        if bytes.len() < 8 {
            break;
        }
        let reply = parse_pelta(&bytes, cmd);
        if reply != Reply::None {
            return reply;
        }
        let Some(left) = end.checked_duration_since(Instant::now()) else {
            break;
        };
        r = d.read(left.as_millis() as u32);
    }
    Reply::None
}
fn poll_pelta(ctx: &mut PollCtx) {
    if ctx.is_claimed(0x0b05, 0x1b84) {
        return;
    }
    for c in ctx
        .find(0x0b05, 0x1b84)
        .filter(|c| c.iface == 3 && c.page == 0xff00 && c.usage == 1 && c.in_len >= 8 && c.out_len >= 8)
    {
        let Some(d) = HidDev::open(c) else {
            continue;
        };
        let state = match pelta_query(&d, 7) {
            Reply::None => continue,
            Reply::Asleep => State::Unreachable,
            Reply::Ok(r) => {
                let charging = match pelta_query(&d, 8) {
                    Reply::Ok(ch) => ch.get(5) == Some(&1),
                    _ => false,
                };
                match r.get(6) {
                    Some(&percent) if percent <= 100 => State::Battery { percent, charging },
                    _ => State::Unreachable,
                }
            }
        };
        ctx.add(
            Observation::new("rog:0b05:1b84:hs", state).name("ROG Pelta").kind(Kind::Headset).via("2.4 GHz receiver"),
            0x0b05,
            0x1b84,
        );
        return;
    }
}
#[derive(Default)]
struct Rog {
    awake: BTreeSet<u16>,
}
pub fn driver() -> Box<dyn Driver> {
    Box::new(Rog::default())
}
impl Driver for Rog {
    fn name(&self) -> &'static str {
        "rog"
    }
    fn poll(&mut self, ctx: &mut PollCtx) {
        let mut seen = Vec::new();
        poll_pelta(ctx);
        poll_omni(ctx, &mut seen);
        for m in MODELS {
            if ctx.is_claimed(0x0b05, m.pid) {
                continue;
            }
            for c in ctx.find(0x0b05, m.pid).filter(|c| {
                c.out_len >= 12
                    && c.page >= 0xff00
                    && (m.iface < 0 || c.iface == m.iface)
                    && m.col.is_none_or(|col| path_has(&c.path, col))
            }) {
                let Some(d) = HidDev::open(c) else {
                    continue;
                };
                let Some((s, asleep)) = read_model(&d, m) else {
                    continue;
                };
                // An unpaired Omni channel also answers asleep; only show it after a genuine reply.
                if m.rid != 0 && asleep && !self.awake.contains(&m.pid) {
                    continue;
                }
                if m.rid != 0 && !asleep {
                    self.awake.insert(m.pid);
                }
                add_once(ctx, &mut seen, m, s);
                ctx.claim(0x0b05, m.pid);
                break;
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reply_layouts() {
        let mut r = vec![0; 11];
        r[0] = 0x12;
        r[1] = 1;
        assert!(matches!(parse_reply(r.clone(), 0, 0x12, 1), Reply::Ok(_)));
        r[0] = 0xff;
        r[1] = 0xaa;
        assert_eq!(parse_reply(r, 0, 0x12, 1), Reply::Asleep);
        assert_eq!(parse_reply(vec![0; 10], 0, 0x12, 1), Reply::None);
        assert_eq!(parse_pelta(&[0xcc, 0x12, 7, 0, 0, 0xff, 0xaa, 0], 7), Reply::Asleep);
        assert_eq!(parse_pelta(&[0xcc, 0xff, 0xaa, 0, 0, 0, 0, 0], 7), Reply::Asleep);
        assert!(matches!(parse_pelta(&[0xcc, 0x12, 7, 0, 0, 0, 73, 0], 7), Reply::Ok(_)));
        assert_eq!(parse_pelta(&[0xcc, 0x12, 8, 0, 0, 1, 0, 0], 7), Reply::None);
    }
    #[test]
    fn model_battery_offsets_and_scaling() {
        let kb = model!(0x193e, "ROG Falchion", KB, 0, 1, None, 11, 1);
        let mouse = model!(0x18b4, "ROG Strix Carry", MS, 0, 1, None, 7, 25);
        let mut r = [0; 12];
        r[11] = 73;
        r[9] = 1;
        assert_eq!(model_state(&r, &kb), Some(State::Battery { percent: 73, charging: true }));
        r[7] = 3;
        r[10] = 1;
        assert_eq!(model_state(&r, &mouse), Some(State::Battery { percent: 75, charging: true }));
        r[7] = 0;
        assert_eq!(model_state(&r, &mouse), Some(State::Unreachable));
        r[7] = 5;
        assert_eq!(model_state(&r, &mouse), Some(State::Unreachable));
        assert_eq!(model_state(&[], &kb), None);
    }
}
