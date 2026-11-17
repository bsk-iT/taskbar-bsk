//! OpenRazer 90-byte feature protocol, prefixed with report ID 0.
//! Class 07: command 80 battery (0..255), 84 charging; XOR covers bytes 2..87.
use super::{simple, Driver, PollCtx};
use crate::{
    device::{Kind, Observation, State},
    hid::{self, HidDev},
};
use std::{thread::sleep, time::Duration};

const KEYBOARDS: &[u16] = &[
    0x010d, 0x010e, 0x010f, 0x0111, 0x0113, 0x0118, 0x011a, 0x011b, 0x011c, 0x0201, 0x0202, 0x0203, 0x0204, 0x0205,
    0x0207, 0x0208, 0x0209, 0x020f, 0x0210, 0x0211, 0x0214, 0x0216, 0x0217, 0x021a, 0x021e, 0x021f, 0x0220, 0x0221,
    0x0224, 0x0225, 0x0226, 0x0227, 0x0228, 0x022a, 0x022b, 0x022c, 0x022d, 0x022f, 0x0232, 0x0233, 0x0234, 0x0235,
    0x0237, 0x0239, 0x023a, 0x023b, 0x023f, 0x0240, 0x0241, 0x0243, 0x0244, 0x0245, 0x0246, 0x024a, 0x024b, 0x024c,
    0x024d, 0x024e, 0x0252, 0x0253, 0x0255, 0x0256, 0x0257, 0x0258, 0x0259, 0x025a, 0x025c, 0x025d, 0x025e, 0x0266,
    0x0268, 0x0269, 0x026a, 0x026b, 0x026c, 0x026d, 0x026e, 0x026f, 0x0270, 0x0271, 0x0276, 0x0279, 0x027a, 0x0282,
    0x0287, 0x028a, 0x028b, 0x028c, 0x028d, 0x028f, 0x0290, 0x0292, 0x0293, 0x0294, 0x0295, 0x0296, 0x0298, 0x029d,
    0x029e, 0x029f, 0x02a0, 0x02a1, 0x02a2, 0x02a3, 0x02a5, 0x02a6, 0x02a7, 0x02b0, 0x02b6, 0x02b8, 0x02b9, 0x02ba,
    0x02c5, 0x02c6, 0x02c7, 0x02cd, 0x02cf, 0x02d2, 0x02d4, 0x02d5, 0x02d7, 0x02e0, 0x02e1, 0x02ea, 0x0a24,
];
const MICE: &[u16] = &[
    0x0013, 0x0015, 0x0016, 0x001f, 0x0020, 0x0024, 0x0025, 0x0029, 0x002e, 0x002f, 0x0032, 0x0034, 0x0036, 0x0037,
    0x0038, 0x0039, 0x003e, 0x003f, 0x0040, 0x0041, 0x0042, 0x0043, 0x0044, 0x0045, 0x0046, 0x0048, 0x004c, 0x004f,
    0x0050, 0x0053, 0x0054, 0x0059, 0x005a, 0x005b, 0x005c, 0x005e, 0x005f, 0x0060, 0x0062, 0x0064, 0x0065, 0x0067,
    0x006a, 0x006b, 0x006c, 0x006e, 0x006f, 0x0070, 0x0071, 0x0072, 0x0073, 0x0077, 0x0078, 0x007a, 0x007b, 0x007c,
    0x007d, 0x0080, 0x0083, 0x0084, 0x0085, 0x0086, 0x0088, 0x008a, 0x008c, 0x008d, 0x008f, 0x0090, 0x0091, 0x0094,
    0x0095, 0x0096, 0x0098, 0x0099, 0x009a, 0x009c, 0x009e, 0x009f, 0x00a1, 0x00a3, 0x00a5, 0x00a6, 0x00a7, 0x00a8,
    0x00aa, 0x00ab, 0x00af, 0x00b0, 0x00b2, 0x00b3, 0x00b4, 0x00b6, 0x00b7, 0x00b8, 0x00b9, 0x00be, 0x00bf, 0x00c0,
    0x00c1, 0x00c2, 0x00c3, 0x00c4, 0x00c5, 0x00c7, 0x00c8, 0x00cb, 0x00cc, 0x00cd, 0x00d0, 0x00d1, 0x00d3, 0x00d4,
    0x00d6, 0x00d7, 0x00da, 0x00db, 0x00de, 0x00df, 0x00e0, 0x00e5, 0x00e6,
];
fn keyboard_tid(pid: u16) -> u8 {
    match pid {
        0x025c | 0x0271 | 0x0290 | 0x0296 | 0x02ba | 0x02d2 | 0x02d5 => 0x9f,
        0x0258 | 0x0292 | 0x0298 | 0x02b9 | 0x02cd | 0x02d4 | 0x02d7 => 0x1f,
        0x025a => 0x3f,
        _ => 0,
    }
}
#[derive(Clone, Copy, PartialEq, Debug)]
enum Reply {
    Ok(u8),
    Busy,
    Offline,
    Unsupported,
    Silent,
}
fn packet(tid: u8, cmd: u8) -> [u8; 91] {
    let mut r = [0; 91];
    r[2] = tid;
    r[6] = 2;
    r[7] = 7;
    r[8] = cmd;
    r[89] = r[3..89].iter().fold(0, |a, b| a ^ b);
    r
}
fn parse_reply(r: &[u8], tid: u8, cmd: u8) -> Reply {
    let Some(a) = r.get(1..91) else { return Reply::Silent };
    if a.get(1..8) != Some(&[tid, 0, 0, 0, 2, 7, cmd][..])
        || a.get(88).copied() != Some(a.get(2..88).unwrap_or_default().iter().fold(0, |x, y| x ^ y))
    {
        return Reply::Silent;
    }
    match a.first() {
        Some(1) => Reply::Busy,
        Some(2) => a.get(9).copied().map(Reply::Ok).unwrap_or(Reply::Silent),
        Some(4) => Reply::Offline,
        Some(3 | 5) => Reply::Unsupported,
        _ => Reply::Silent,
    }
}
fn command(d: &HidDev, pid: u16, tid: u8, cmd: u8) -> Reply {
    let rep = packet(tid, cmd);
    let mut send = true;
    let mut result = Reply::Silent;
    for attempt in 0..8 {
        if send && !d.set_feature(&rep) {
            return Reply::Silent;
        }
        sleep(Duration::from_millis(if keyboard_tid(pid) != 0 {
            5
        } else if attempt == 0 {
            15
        } else {
            40
        }));
        send = true;
        result = d.get_feature(0).map(|r| parse_reply(&r, tid, cmd)).unwrap_or(Reply::Silent);
        match result {
            Reply::Busy => {
                send = false;
                result = Reply::Silent;
            }
            Reply::Ok(_) | Reply::Unsupported => return result,
            _ => {}
        }
    }
    result
}
fn clean_name(mut n: String) -> String {
    for suffix in [" Dongle", " (Wired)", " (Wireless)", " Wireless Receiver", " Receiver", " HyperSpeed Dongle"] {
        if n.len() > suffix.len() && n.ends_with(suffix) {
            n.truncate(n.len() - suffix.len());
        }
    }
    if n.is_empty() {
        "Razer device".into()
    } else {
        n
    }
}
pub fn driver() -> Box<dyn Driver> {
    simple("razer", poll)
}
fn poll(ctx: &mut PollCtx) {
    let mut seen = Vec::new();
    for (vid, pid) in ctx.products(0x1532) {
        let colls = ctx.colls;
        for c in colls.iter().filter(|c| c.vid == vid && c.pid == pid && c.feat_len == 91) {
            let Some(d) = HidDev::open_feature_only(c) else { continue };
            let kt = keyboard_tid(pid);
            let known_tid = [kt];
            let tids = if kt != 0 { &known_tid[..] } else { &[0x1f, 0x3f, 0xff, 0x08, 0x9f][..] };
            let mut result = Reply::Silent;
            let mut tid = 0;
            let mut offline = false;
            for &t in tids {
                let r = command(&d, pid, t, 0x80);
                if matches!(r, Reply::Ok(_)) {
                    result = r;
                    tid = t;
                    break;
                }
                offline |= r == Reply::Offline;
            }
            if !matches!(result, Reply::Ok(_)) && offline {
                result = Reply::Offline;
            }
            let state = match result {
                Reply::Ok(level) => State::Battery {
                    percent: ((u16::from(level) * 100 + 127) / 255) as u8,
                    charging: command(&d, pid, tid, 0x84) == Reply::Ok(1),
                },
                Reply::Offline => State::Off,
                _ => continue,
            };
            let name = clean_name(hid::product(c));
            let mut kind = if KEYBOARDS.contains(&pid) {
                Kind::Keyboard
            } else if MICE.contains(&pid) {
                Kind::Mouse
            } else {
                hid::kind_from_collections(colls, vid, pid)
            };
            if kind == Kind::Other {
                kind = Kind::from_name(&name);
            }
            if seen.contains(&name) {
                ctx.claim(vid, pid);
                if matches!(state, State::Battery { charging: true, .. }) {
                    for old in &mut ctx.out {
                        if old.name.as_deref() == Some(&name) {
                            if let State::Battery { charging, .. } = &mut old.state {
                                *charging = true;
                            }
                        }
                    }
                }
            } else {
                seen.push(name.clone());
                ctx.add(
                    Observation::new(format!("razer:{vid:04x}:{pid:04x}"), state)
                        .name(name)
                        .kind(kind)
                        .via("Razer receiver")
                        .grace(Duration::from_secs(120)),
                    vid,
                    pid,
                );
            }
            break;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mailbox_checks() {
        let mut r = packet(0x9f, 0x80);
        r[1] = 2;
        r[10] = 255;
        r[89] = r[3..89].iter().fold(0, |a, b| a ^ b);
        assert_eq!(parse_reply(&r, 0x9f, 0x80), Reply::Ok(255));
        assert_eq!(parse_reply(&r[..90], 0x9f, 0x80), Reply::Silent);
        assert_eq!(parse_reply(&r, 0x1f, 0x80), Reply::Silent);
        for (status, expected) in
            [(1, Reply::Busy), (3, Reply::Unsupported), (4, Reply::Offline), (5, Reply::Unsupported)]
        {
            r[1] = status;
            assert_eq!(parse_reply(&r, 0x9f, 0x80), expected);
        }
        r[89] ^= 1;
        assert_eq!(parse_reply(&r, 0x9f, 0x80), Reply::Silent);
    }
}
