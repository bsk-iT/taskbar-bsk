use super::{simple, Driver, PollCtx};
use crate::device::{Kind, Observation};
use crate::hid::{self, HidColl, HidDev};
use std::time::{Duration, Instant};

// Launcher: unnumbered 64-byte [cmd 00 81 sub ... checksum], checksum A1-sum(0..62).
// 4K FF0A:0001 uses battery/state 6/5; classic and 8k_nordic use 11/10.
// Link-KM Nape uses VIA raw HID FF60:0061: B2 slots, then A7 31 battery/charging.
fn request(cmd: u8, sub: u8) -> [u8; 65] {
    let mut r = [0; 65];
    r[1] = cmd;
    r[3] = 0x81;
    r[4] = sub;
    r[64] = 0xa1u8.wrapping_sub(cmd).wrapping_sub(0x81).wrapping_sub(sub);
    r
}
fn query(d: &HidDev, cmd: u8, sub: u8) -> Option<Vec<u8>> {
    d.flush();
    if !d.write(&request(cmd, sub), 500) {
        return None;
    }
    let end = Instant::now() + Duration::from_millis(600);
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        let r = d.read(left.as_millis() as u32)?;
        if r.len() < 13 {
            return None;
        }
        if r.first() == Some(&cmd) && r.get(3) == Some(&sub) {
            return Some(r);
        }
    }
    None
}
fn raw_query(d: &HidDev, cmd: &[u8]) -> Option<Vec<u8>> {
    let mut req = [0; 33];
    req.get_mut(1..cmd.len() + 1)?.copy_from_slice(cmd);
    d.flush();
    if !d.write(&req, 500) {
        return None;
    }
    let end = Instant::now() + Duration::from_millis(600);
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        let r = d.read(left.as_millis() as u32)?;
        if r.len() >= cmd.len() + 2 && r.starts_with(cmd) {
            return Some(r);
        }
    }
    None
}
fn nape_connected(r: &[u8]) -> bool {
    if r.len() < 17 {
        return false;
    }
    let mut connected = false;
    for off in [2, 7, 12] {
        if r.get(off + 2..off + 4) == Some(&[0x40, 4]) {
            connected = r.get(off + 4) == Some(&1);
        }
    }
    connected
}
fn parse_nape(r: &[u8]) -> Option<(u8, bool)> {
    let [0xa7, 0x31, p, charging, ..] = r else {
        return None;
    };
    (*p <= 100).then_some((*p, *charging != 0))
}
fn poll_nape(ctx: &mut PollCtx) {
    for pid in [0xd026, 0x0440] {
        for c in ctx.find(0x3434, pid).filter(|c| c.page == 0xff60 && c.usage == 0x61 && c.out_len == 33) {
            let Some(d) = HidDev::open(c) else {
                continue;
            };
            if pid == 0xd026 && !raw_query(&d, &[0xb2]).is_some_and(|r| nape_connected(&r)) {
                continue;
            }
            let Some((p, charging)) = raw_query(&d, &[0xa7, 0x31]).and_then(|r| parse_nape(&r)) else {
                continue;
            };
            ctx.add(
                Observation::battery("keychron:3434:0440:nape", "Keychron Nape Pro", Kind::Mouse, p, charging)
                    .via(if pid == 0xd026 { "2.4 GHz receiver" } else { "USB cable" }),
                0x3434,
                pid,
            );
            return;
        }
    }
}
fn name_4k(pid: u16) -> Option<&'static str> {
    match pid {
        0x0620 | 0x0622 => Some("Keychron M3 mini 4K"),
        0x0621 => Some("Keychron M4 4K"),
        0x0624 => Some("Keychron M6 4K"),
        0x07a0 => Some("Keychron M3 4K"),
        _ => None,
    }
}
fn legacy_name(pid: u16) -> &'static str {
    match pid {
        0xd032 | 0xd033 => "Keychron M3",
        0xd035 => "Keychron M1",
        0xd036 | 0xd03e => "Keychron M3 mini",
        0xd03b => "Keychron M2",
        0xd03d => "Keychron M2 mini",
        0xd03f => "Keychron M6",
        0xd044 => "Keychron M7",
        _ => "Keychron mouse",
    }
}
fn parse_legacy(r: &[u8]) -> Option<(u16, u8, bool)> {
    if r.len() < 13 || r.get(..3) != Some(&[0x51, 6, 1]) || r.get(4..6) != Some(&[0x34, 0x34]) {
        return None;
    }
    let percent = *r.get(11)?;
    if percent > 100 {
        return None;
    }
    Some((u16::from_le_bytes([*r.get(6)?, *r.get(7)?]), percent, r.get(12) == Some(&1)))
}
fn legacy_collection(c: &HidColl) -> bool {
    c.vid == 0x3434 && matches!(c.pid, 0xd030 | 0xd031) && c.page == 0x8c && c.feat_len >= 16
}
fn launcher_collection(c: &HidColl) -> bool {
    c.page >= 0xff00 && c.out_len == 65 && c.in_len == 65
}
fn poll_legacy(ctx: &mut PollCtx) {
    // Legacy 1K receiver: 1001-byte feature report 51, status command 06, page 008C.
    for pid in [0xd030, 0xd031] {
        for c in ctx.find(0x3434, pid).filter(|c| legacy_collection(c)) {
            let Some(d) = HidDev::open(c).or_else(|| HidDev::open_feature_only(c)) else {
                continue;
            };
            if !d.set_feature(&[0x51, 6]) {
                continue;
            }
            let mut reply = None;
            for _ in 0..5 {
                std::thread::sleep(Duration::from_millis(40));
                if let Some(r) = d.get_feature(0x51) {
                    if r.len() >= 13 && r.get(..2) == Some(&[0x51, 6]) {
                        reply = Some(r);
                        break;
                    }
                }
            }
            let Some((mouse_pid, p, charging)) = reply.and_then(|r| parse_legacy(&r)) else {
                continue;
            };
            ctx.add(
                Observation::battery(
                    format!("keychron:3434:{mouse_pid:04x}"),
                    legacy_name(mouse_pid),
                    Kind::Mouse,
                    p,
                    charging,
                )
                .via("2.4 GHz receiver"),
                0x3434,
                pid,
            );
            break;
        }
    }
}
fn clean_name(name: String) -> String {
    if name.is_empty() || name.contains("Link") || name.contains("Receiver") {
        "Keychron mouse".into()
    } else {
        name
    }
}
fn le16(r: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*r.get(at)?, *r.get(at + 1)?]))
}
fn parse_classic(r: &[u8]) -> Option<(u8, bool)> {
    if le16(r, 6)? != 0x3434 {
        return None;
    }
    let p = *r.get(11)?;
    (p <= 100).then_some((p, *r.get(10)? != 0))
}
fn parse_8k(r: &[u8], cable: bool) -> Option<(u8, bool)> {
    let p = *r.get(11)?;
    let state = *r.get(10)?;
    if state > 3 || p > 100 || (cable && (p == 0 || !(1..=8).contains(r.get(5)?))) {
        return None;
    }
    Some((p, state == 1))
}
fn parse_4k(r: &[u8]) -> Option<(u8, bool)> {
    let p = *r.get(6)?;
    (1..=100).contains(&p).then_some((p, *r.get(5)? != 0))
}
fn mouse_status(
    pid: u16,
    page: u16,
    usage: u16,
    name: String,
    mut ask: impl FnMut(u8, u8) -> Option<Vec<u8>>,
) -> Option<Observation> {
    let r1 = ask(1, 1);
    let tag = r1.as_deref().and_then(|r| le16(r, 6)).unwrap_or(0);
    let mut name = clean_name(name);
    let (percent, charging, wireless) =
        if r1.is_some() && page == 0xff0a && usage == 1 && tag != 0x3434 && tag != 0x362d {
            let (p, ch) = parse_4k(&ask(0x41, 1)?)?;
            if let Some(model) = ask(0x40, 0).as_deref().and_then(|r| le16(r, 4)).and_then(name_4k) {
                name = model.into();
            }
            (p, ch, true)
        } else if let Some((p, ch)) = r1.as_deref().and_then(parse_classic) {
            (p, ch, true)
        } else if let Some((p, ch)) = ask(0x41, 1).as_deref().and_then(|r| parse_8k(r, false)) {
            (p, ch, true)
        } else {
            let (p, ch) = parse_8k(r1.as_deref()?, true)?;
            (p, ch, false)
        };
    Some(
        Observation::battery(format!("keychron:3434:{pid:04x}"), name, Kind::Mouse, percent, charging)
            .via(if wireless { "2.4 GHz receiver" } else { "USB cable" }),
    )
}
pub fn driver() -> Box<dyn Driver> {
    simple("keychron", poll)
}
fn poll(ctx: &mut PollCtx) {
    poll_nape(ctx);
    poll_legacy(ctx);
    for (vid, pid) in ctx.products(0x3434) {
        for c in ctx.find(vid, pid).filter(|c| launcher_collection(c)) {
            let Some(d) = HidDev::open(c) else {
                continue;
            };
            if let Some(s) = mouse_status(pid, c.page, c.usage, hid::product(c), |cmd, sub| query(&d, cmd, sub)) {
                ctx.add(s, vid, pid);
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::State;
    fn reply(cmd: u8, sub: u8, bytes: &[(usize, u8)]) -> Vec<u8> {
        let mut r = vec![0; 64];
        r[0] = cmd;
        r[2] = 0x81;
        r[3] = sub;
        for &(i, v) in bytes {
            r[i] = v;
        }
        r
    }
    fn scripted(pid: u16, script: &[(u8, u8, Vec<u8>)]) -> (Option<Observation>, Vec<(u8, u8)>) {
        let mut writes = Vec::new();
        let result = mouse_status(pid, 0xff0a, 1, "Keychron 4K Link".into(), |cmd, sub| {
            writes.push((cmd, sub));
            script.iter().find(|&&(c, s, _)| c == cmd && s == sub).map(|(_, _, r)| r.clone())
        });
        (result, writes)
    }
    #[test]
    fn exact_requests() {
        for (cmd, sub, checksum) in [(1, 1, 0x1e), (0x41, 1, 0xde), (0x40, 0, 0xe0)] {
            let mut expected = [0; 65];
            expected[1] = cmd;
            expected[3] = 0x81;
            expected[4] = sub;
            expected[64] = checksum;
            assert_eq!(request(cmd, sub), expected);
        }
    }
    #[test]
    fn four_k_scripted_cases() {
        for (state, p, model, name) in [
            (1, 73, Some(0x0621), "Keychron M4 4K"),
            (0, 100, Some(0x07a0), "Keychron M3 4K"),
            (2, 40, None, "Keychron mouse"),
            (0, 55, Some(0x0999), "Keychron mouse"),
        ] {
            let mut script = vec![
                (1, 1, reply(1, 1, &[(6, 0x12), (7, 5)])),
                (0x41, 1, reply(0x41, 1, &[(5, state), (6, p), (7, 2)])),
            ];
            if let Some(model) = model {
                script.push((0x40, 0, reply(0x40, 0, &[(4, model as u8), (5, (model >> 8) as u8)])));
            }
            let (s, writes) = scripted(0xd038, &script);
            let s = s.expect("scripted response");
            assert_eq!(writes, [(1, 1), (0x41, 1), (0x40, 0)]);
            assert_eq!(s.id, "keychron:3434:d038");
            assert_eq!(s.name.as_deref(), Some(name));
            assert_eq!(s.kind, Some(Kind::Mouse));
            assert_eq!(s.via, "2.4 GHz receiver");
            assert_eq!(s.state, State::Battery { percent: p, charging: state != 0 });
        }
        for p in [0, 101, 0xff] {
            let (s, writes) = scripted(
                0xd038,
                &[(1, 1, reply(1, 1, &[])), (0x41, 1, reply(0x41, 1, &[(5, 1), (6, p), (10, 1), (11, 50)]))],
            );
            assert!(s.is_none());
            assert_eq!(writes, [(1, 1), (0x41, 1)]);
        }
        for script in [vec![], vec![(1, 1, reply(1, 1, &[]))]] {
            let (s, writes) = scripted(0xd038, &script);
            assert!(s.is_none());
            assert_eq!(writes, [(1, 1), (0x41, 1)]);
        }
    }
    #[test]
    fn eight_k_scripted_cases() {
        let (s, writes) = scripted(
            0xd028,
            &[(1, 1, reply(1, 1, &[(6, 0x34), (7, 0x34), (10, 1), (11, 64)])), (0x41, 1, reply(0x41, 1, &[(6, 99)]))],
        );
        assert_eq!(writes, [(1, 1)]);
        assert_eq!(s.expect("classic").state, State::Battery { percent: 64, charging: true });
        let (s, writes) = scripted(
            0xd028,
            &[
                (1, 1, reply(1, 1, &[(6, 0x2d), (7, 0x36)])),
                (0x41, 1, reply(0x41, 1, &[(5, 1), (6, 99), (10, 3), (11, 81)])),
            ],
        );
        assert_eq!(writes, [(1, 1), (0x41, 1)]);
        let s = s.expect("nordic");
        assert_eq!(s.state, State::Battery { percent: 81, charging: false });
        assert_eq!(s.name.as_deref(), Some("Keychron mouse"));
        assert_eq!(parse_8k(&reply(1, 1, &[(5, 8), (10, 1), (11, 40)]), true), Some((40, true)));
        assert_eq!(parse_8k(&reply(1, 1, &[(5, 9), (11, 40)]), true), None);
        assert_eq!(parse_classic(&[]), None);
        assert_eq!(parse_4k(&[]), None);
    }
    fn status(ok: u8, vid: u16, pid: u16, battery: u8, charging: u8) -> Vec<u8> {
        vec![0x51, 6, ok, 0, vid as u8, (vid >> 8) as u8, pid as u8, (pid >> 8) as u8, 0, 0, 0, battery, charging]
    }
    #[test]
    fn legacy_cases() {
        assert_eq!(parse_legacy(&status(1, 0x3434, 0xd033, 87, 0)), Some((0xd033, 87, false)));
        assert_eq!(legacy_name(0xd033), "Keychron M3");
        assert_eq!(parse_legacy(&status(1, 0x3434, 0xd0ff, 100, 1)), Some((0xd0ff, 100, true)));
        assert_eq!(legacy_name(0xd0ff), "Keychron mouse");
        for bad in [
            status(0, 0x3434, 0xd033, 50, 0),
            status(1, 0x1234, 0xd033, 50, 0),
            status(1, 0x3434, 0xd033, 101, 0),
            vec![],
        ] {
            assert_eq!(parse_legacy(&bad), None);
        }
    }
    #[test]
    fn collection_filters_and_receiver_claim() {
        let mut launcher =
            HidColl { vid: 0x3434, pid: 0xd038, page: 0xff0a, usage: 1, in_len: 65, out_len: 65, ..HidColl::default() };
        assert!(launcher_collection(&launcher));
        launcher.in_len = 33;
        assert!(!launcher_collection(&launcher));
        launcher.in_len = 65;
        launcher.page = 1;
        assert!(!launcher_collection(&launcher));
        let mut legacy =
            HidColl { vid: 0x3434, pid: 0xd030, page: 0x8c, usage: 1, feat_len: 1001, ..HidColl::default() };
        assert!(legacy_collection(&legacy));
        legacy.pid = 0xd031;
        assert!(legacy_collection(&legacy));
        legacy.feat_len = 0;
        assert!(!legacy_collection(&legacy));
        legacy.feat_len = 1001;
        legacy.pid = 0xd038;
        assert!(!legacy_collection(&legacy));
        let (s, _) = scripted(0xd038, &[(1, 1, reply(1, 1, &[])), (0x41, 1, reply(0x41, 1, &[(6, 73)]))]);
        let mut ctx = PollCtx::new(&[]);
        if let Some(s) = s {
            ctx.add(s, 0x3434, 0xd038);
        }
        assert_eq!(ctx.out.len(), 1);
        assert!(ctx.is_claimed(0x3434, 0xd038));
    }
    #[test]
    fn nape_frames() {
        let mut slots = [0; 17];
        slots[4..7].copy_from_slice(&[0x40, 4, 1]);
        assert!(nape_connected(&slots));
        slots[6] = 0;
        assert!(!nape_connected(&slots));
        assert!(!nape_connected(&slots[..16]));
        assert_eq!(parse_nape(&[0xa7, 0x31, 73, 1]), Some((73, true)));
        assert_eq!(parse_nape(&[0xa7, 0x31, 101, 0]), None);
        assert_eq!(parse_nape(&[]), None);
    }
}
