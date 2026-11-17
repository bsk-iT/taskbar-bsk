// Kysona/Compx report 08: 17 bytes, checksum = 55 - sum(bytes 0..15).
// Commands 03/04 read link/battery; battery [6]=percent [7]=charging [8..9]=mV BE.
// Sources: Linux hid-kysona, vxe-r1-pro-max-battery, BatteryTray, Redragon web tools.
use super::{
    glorious::{gwolves_is_cable, gwolves_model_name},
    simple, Driver, PollCtx,
};
use crate::{
    device::{Kind, Observation, State},
    hid::{self, HidDev},
};
use std::time::{Duration, Instant};

fn checksum(b: &[u8]) -> u8 {
    0x55u8.wrapping_sub(b.iter().take(16).fold(0u8, |sum, b| sum.wrapping_add(*b)))
}
fn valid_reply(r: &[u8], cmd: u8) -> bool {
    r.len() >= 17 && r.first() == Some(&8) && r.get(1) == Some(&cmd) && r.get(16) == Some(&checksum(r))
}
fn query(d: &HidDev, cmd: u8) -> Option<Vec<u8>> {
    let mut req = [0; 17];
    req[0] = 8;
    req[1] = cmd;
    req[16] = checksum(&req);
    d.flush();
    if !d.write(&req, 500) {
        return None;
    }
    let end = Instant::now() + Duration::from_millis(500);
    loop {
        let left = end.saturating_duration_since(Instant::now()).as_millis() as u32;
        if left == 0 {
            return None;
        }
        let r = d.read(left)?;
        if r.len() < 17 {
            return None;
        }
        if valid_reply(&r, cmd) {
            return Some(r);
        }
    }
}
fn is_r1(vid: u16, pid: u16) -> bool {
    vid == 0x3554 && matches!(pid, 0xf58a | 0xf58c | 0xf58e | 0xf58f | 0xf505 | 0xf506)
}
fn is_cable(vid: u16, pid: u16) -> bool {
    (vid == 0x33e4 && gwolves_is_cable(pid))
        || (vid == 0x35bb && matches!(pid, 0xd3fa | 0xd3fc | 0xd3fd))
        || (vid == 0x3554 && matches!(pid, 0xf55e | 0xf58c | 0xf58f | 0xf506))
}
fn clean_name(mut n: String, vid: u16, pid: u16) -> String {
    if vid == 0x33e4 {
        return gwolves_model_name(pid).into();
    }
    if vid == 0x3554 && matches!(pid, 0xf55d | 0xf5d5 | 0xf55e) {
        return "Redragon wireless mouse".into();
    }
    if is_r1(vid, pid) {
        return "VXE Dragonfly R1".into();
    }
    for suffix in [" Dongle", " Receiver", " 2.4G", " Wireless Receiver"] {
        if n.len() > suffix.len() && n.ends_with(suffix) {
            n.truncate(n.len() - suffix.len());
        }
    }
    if vid == 0x3770 {
        return "Hitscan Hyperlight".into();
    }
    if vid == 0x35bb {
        return "HECATE mouse".into();
    }
    if n.is_empty() {
        n = if vid == 0x373b { "ATK wireless mouse" } else { "Wireless mouse" }.into();
    }
    n
}
fn parse_battery(r: &[u8]) -> Option<(Option<u8>, bool, i32)> {
    let (&pct, &charging, &hi, &lo) = (r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?);
    Some(((pct <= 100).then_some(pct), charging != 0, (i32::from(hi) << 8) | i32::from(lo)))
}
pub fn driver() -> Box<dyn Driver> {
    simple("kysona", poll)
}
fn poll(ctx: &mut PollCtx) {
    let mut seen = Vec::<String>::new();
    for vendor in [0x3554, 0x373b, 0x3770, 0x33e4, 0x3710, 0x35bb] {
        for (vid, pid) in ctx.products(vendor) {
            if vid == 0x373b && matches!(pid, 0x11ca | 0x11cb | 0x1203 | 0x1204) {
                continue;
            }
            let mut colls: Vec<_> = ctx
                .colls
                .iter()
                .filter(|c| {
                    c.vid == vid
                        && c.pid == pid
                        && ((c.out_len == 17 && c.in_len == 17)
                            || (c.page >= 0xff00 && c.out_len >= 17 && c.in_len >= 17))
                })
                .collect();
            colls.sort_by_key(|c| !(c.page == 0xff02 && c.usage == 2));
            for c in colls {
                let Some(d) = HidDev::open(c) else { continue };
                let online = query(&d, 3).and_then(|r| r.get(6).map(|b| *b != 0));
                let mut answered = online.is_some();
                let mut connected = online.unwrap_or(true);
                let mut battery = None;
                if !answered || connected {
                    if let Some(r) = query(&d, 4) {
                        answered = true;
                        connected = true;
                        battery = parse_battery(&r);
                    }
                }
                if !answered {
                    continue;
                }
                if !connected {
                    ctx.claim(vid, pid);
                    break;
                }
                let name = clean_name(hid::product(c), vid, pid);
                let mut kind = hid::kind_from_collections(ctx.colls, vid, pid);
                if kind == Kind::Other {
                    kind = Kind::Mouse;
                }
                let id = format!("kysona:{vid:04x}:{pid:04x}");
                let charging = battery.is_some_and(|(_, ch, _)| ch);
                if seen.contains(&name) {
                    ctx.claim(vid, pid);
                    if charging {
                        for o in &mut ctx.out {
                            if o.name.as_deref() == Some(name.as_str()) {
                                if let State::Battery { charging, .. } = &mut o.state {
                                    *charging = true;
                                }
                            }
                        }
                    }
                } else {
                    let mut obs = match battery.and_then(|(pct, ch, _)| {
                        pct.map(|p| Observation::battery(id.clone(), name.clone(), kind, p, ch))
                    }) {
                        Some(obs) => obs,
                        None => Observation::new(id, State::Unreachable).name(name.clone()).kind(kind),
                    };
                    if let Some((_, _, mv)) = battery {
                        obs = obs.millivolts(mv as u16);
                    }
                    seen.push(name);
                    ctx.add(
                        obs.via(if is_cable(vid, pid) { "USB cable" } else { "2.4 GHz receiver" })
                            .grace(Duration::from_secs(1800)),
                        vid,
                        pid,
                    );
                }
                break;
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checksummed_frames() {
        let mut r = [0; 17];
        r[0] = 8;
        r[1] = 4;
        r[6] = 65;
        r[7] = 1;
        r[8] = 15;
        r[9] = 160;
        r[16] = checksum(&r);
        assert!(valid_reply(&r, 4));
        assert!(!valid_reply(&r, 3));
        assert!(!valid_reply(&r[..16], 4));
        assert_eq!(parse_battery(&r), Some((Some(65), true, 4000)));
        r[6] = 255;
        assert_eq!(parse_battery(&r), Some((None, true, 4000)));
        assert_eq!(parse_battery(&r[..9]), None);
    }
}
