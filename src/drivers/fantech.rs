use super::{simple, Driver, PollCtx};
use crate::{
    device::{Kind, Observation},
    hid::HidDev,
};
use std::time::{Duration, Instant};

// Helios Go XD5 / jarjut/xd5-go-web: feature 08, command 04, sum of 17 bytes = 55.
fn packet() -> [u8; 17] {
    let mut r = [0; 17];
    r[0] = 8;
    r[1] = 4;
    r[16] = 0x49;
    r
}
#[derive(Clone, Copy, Debug, PartialEq)]
struct Battery {
    percent: u8,
    charging: bool,
    checksum: bool,
}
fn parse_battery(r: &[u8]) -> Option<Battery> {
    let checksum = r.len() >= 17 && r.get(..17)?.iter().fold(0u8, |s, b| s.wrapping_add(*b)) == 0x55;
    let plausible = |power, charge| {
        let (p, ch) = (*r.get(power)?, *r.get(charge)?);
        if !(1..=100).contains(&p) || ch > 1 {
            return None;
        }
        Some(Battery { percent: p, charging: ch != 0, checksum })
    };
    // WebHID layouts may omit the report ID, with either documented payload offset.
    if r.len() >= 7 && r.first() == Some(&4) {
        if let Some(b) = plausible(5, 6).or_else(|| plausible(4, 5)) {
            return Some(b);
        }
    }
    if r.len() >= 8 && r.get(1) == Some(&4) {
        return plausible(6, 7);
    }
    None
}
fn remember(found: &mut Option<Battery>, b: Battery) {
    if found.is_none() || (b.checksum && found.is_some_and(|old| !old.checksum)) {
        *found = Some(b);
    }
}
fn query(cmd: &HidDev, readers: &[HidDev], command_reads: bool) -> Option<Battery> {
    cmd.flush();
    if !cmd.set_feature(&packet()) {
        return None;
    }
    let end = Instant::now() + Duration::from_millis(500);
    let mut found = None;
    while Instant::now() < end {
        for d in command_reads.then_some(cmd).into_iter().chain(readers.iter()) {
            let left = end.saturating_duration_since(Instant::now()).as_millis() as u32;
            if left == 0 {
                continue;
            }
            if let Some(r) = d.read(left.min(40)) {
                if r.len() >= 6 {
                    if let Some(b) = parse_battery(&r) {
                        remember(&mut found, b);
                    }
                }
            }
        }
        if found.is_some_and(|b| b.checksum) {
            break;
        }
    }
    // Some firmware puts its answer in the feature buffer instead.
    if let Some(r) = cmd.get_feature(8) {
        if let Some(b) = parse_battery(&r) {
            remember(&mut found, b);
        }
    }
    found
}
fn poll(ctx: &mut PollCtx) {
    for (vid, pid) in ctx.products(0x25a7) {
        if !matches!(pid, 0xfa7b | 0xfa7c) {
            continue;
        }
        let Some(feat) = ctx.find(vid, pid).find(|c| c.feat_len >= 17) else {
            continue;
        };
        let Some(cmd) = HidDev::open(feat).or_else(|| HidDev::open_feature_only(feat)) else {
            continue;
        };
        // Battery input may arrive on any sibling collection, not the feature collection.
        let readers: Vec<_> =
            ctx.find(vid, pid).filter(|c| !std::ptr::eq(*c, feat) && c.in_len >= 6).filter_map(HidDev::open).collect();
        let Some(b) = query(&cmd, &readers, feat.in_len >= 6) else {
            continue;
        };
        ctx.add(
            Observation::battery(
                format!("fantech:{vid:04x}:{pid:04x}"),
                "Fantech Helios Go XD5",
                Kind::Mouse,
                b.percent,
                b.charging,
            )
            .via(if pid == 0xfa7b { "USB cable" } else { "2.4 GHz receiver" })
            .grace(Duration::from_secs(1800)),
            vid,
            pid,
        );
        break;
    }
}
pub fn driver() -> Box<dyn Driver> {
    simple("Fantech", poll)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn layouts_and_checksum_preference() {
        let mut r = [9, 4, 0, 0, 0, 2, 80, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        r[16] = 0x55u8.wrapping_sub(r[..16].iter().fold(0u8, |s, b| s.wrapping_add(*b)));
        let good = Battery { percent: 80, charging: true, checksum: true };
        assert_eq!(parse_battery(&r), Some(good));
        assert_eq!(
            parse_battery(&[4, 0, 0, 0, 0, 75, 0]),
            Some(Battery { percent: 75, charging: false, checksum: false })
        );
        assert_eq!(
            parse_battery(&[4, 0, 0, 0, 75, 0, 0]),
            Some(Battery { percent: 75, charging: false, checksum: false })
        );
        assert_eq!(parse_battery(&[]), None);
        assert_eq!(parse_battery(&[9, 4, 0, 0, 0, 0, 0, 0]), None);
        r[6] = 101;
        assert_eq!(parse_battery(&r), None);
        let mut found = Some(Battery { percent: 1, charging: false, checksum: false });
        remember(&mut found, good);
        assert_eq!(found, Some(good));
        remember(&mut found, Battery { percent: 2, charging: false, checksum: false });
        assert_eq!(found, Some(good));
        assert_eq!(packet(), [8, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x49]);
    }
}
