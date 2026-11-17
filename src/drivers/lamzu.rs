// LAMZU Aurora IsNewProtocol: report 0, request [3]=02 [4]=02 [6]=83.
// Reply A1 carries charging/percent; zero percent on a receiver means no link.
// Model/cable IDs and protocol: lamzu.net Aurora web driver.
use super::{simple, Driver, PollCtx};
use crate::{
    device::{Kind, Observation, State},
    hid::HidDev,
};
use std::{thread::sleep, time::Duration};

const MODELS: &[(u16, &[u16], &str)] = &[
    (0x373e, &[0x001c, 0x001d, 0x001e], "LAMZU Maya X"),
    (0x373e, &[0x0016], "LAMZU Maya X V2"),
    (0x37b0, &[0x001f], "LAMZU Maya X V2"),
    (0x37b0, &[0x0005, 0x000b, 0x000c], "LAMZU Tachi"),
    (0x37b0, &[0x001c, 0x001e], "LAMZU Tachi Lite"),
    (0x37b0, &[0x0009, 0x000f, 0x0010], "LAMZU Inca"),
    (0x37b0, &[0x0011, 0x0013, 0x0015], "LAMZU Maya"),
    (0x37b0, &[0x0007, 0x000d, 0x000e], "LAMZU Paro"),
    (0x37b0, &[0x0017, 0x0019, 0x001b], "LAMZU Thorn"),
    (0x37b0, &[0x0021, 0x0003, 0x0023], "LAMZU Thorn V2"),
    (0x37b0, &[0x0030, 0x0032, 0x0040], "LAMZU Thorn V2"),
    (0x37b0, &[0x0025, 0x0027], "LAMZU Atlantis OG Champion"),
    (0x37b0, &[0x0028, 0x002b, 0x002a], "LAMZU Atlantis Mini"),
    (0x37b0, &[0x003e, 0x003f], "LAMZU Atlantis Mini"),
    (0x37b0, &[0x002c, 0x0052], "LAMZU Maya M"),
    (0x37b0, &[0x003a, 0x003b, 0x0044], "LAMZU Maya X"),
    (0x37b0, &[0x003c, 0x0050], "LAMZU Maya"),
    (0x37b0, &[0x0042, 0x0056], "LAMZU Orcus"),
    (0x37b0, &[0x0046, 0x0054], "LAMZU Mini"),
];
fn is_cable(vid: u16, pid: u16) -> bool {
    if vid == 0x373e {
        return pid == 0x001c;
    }
    matches!(
        pid,
        0x0005
            | 0x0007
            | 0x0009
            | 0x0011
            | 0x0017
            | 0x001c
            | 0x001f
            | 0x0021
            | 0x0025
            | 0x0028
            | 0x002c
            | 0x0030
            | 0x0034
            | 0x0038
            | 0x003a
            | 0x003c
            | 0x003e
            | 0x0040
            | 0x0042
            | 0x0044
            | 0x0046
            | 0x0050
            | 0x0052
            | 0x0056
    )
}
// Aurora accepts the feature response both with and without its report ID.
fn parse_reply(r: &[u8]) -> Option<(u8, bool)> {
    if r.len() < 10 {
        return None;
    }
    let (charging, pct) = if r.get(1) == Some(&0xa1) && r.get(4) == Some(&2) && r.get(6) == Some(&0x83) {
        (*r.get(7)?, *r.get(8)?)
    } else if r.first() == Some(&0xa1) && r.get(3) == Some(&2) && r.get(5) == Some(&0x83) {
        (*r.get(6)?, *r.get(7)?)
    } else {
        return None;
    };
    (pct <= 100).then_some((pct, charging == 1))
}
pub fn driver() -> Box<dyn Driver> {
    simple("lamzu", poll)
}
fn poll(ctx: &mut PollCtx) {
    for vendor in [0x373e, 0x37b0] {
        for (vid, pid) in ctx.products(vendor) {
            let colls = ctx.colls.iter().filter(|c| c.vid == vid && c.pid == pid && c.feat_len == 65);
            for c in colls {
                let Some(d) = HidDev::open_feature_only(c) else { continue };
                let mut req = [0; 65];
                req[3] = 2;
                req[4] = 2;
                req[6] = 0x83;
                if !d.set_feature(&req) {
                    continue;
                }
                sleep(Duration::from_millis(100));
                let Some((pct, charging)) = d.get_feature(0).and_then(|r| parse_reply(&r)) else { continue };
                let name = MODELS
                    .iter()
                    .find(|(v, pids, _)| *v == vid && pids.contains(&pid))
                    .map_or("LAMZU wireless mouse", |(_, _, name)| *name);
                let cable = is_cable(vid, pid);
                let id = format!("lamzu:{vid:04x}:{pid:04x}");
                let obs = if pct > 0 {
                    Observation::battery(id, name, Kind::Mouse, pct, charging)
                } else {
                    Observation::new(id, if cable { State::Unreachable } else { State::Off })
                        .name(name)
                        .kind(Kind::Mouse)
                };
                ctx.add(obs.via(if cable { "USB cable" } else { "2.4 GHz receiver" }), vid, pid);
                break;
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aurora_reply_layouts() {
        assert_eq!(parse_reply(&[0, 0xa1, 0, 0, 2, 0, 0x83, 1, 50, 0]), Some((50, true)));
        assert_eq!(parse_reply(&[0xa1, 0, 0, 2, 0, 0x83, 0, 80, 0, 0]), Some((80, false)));
        assert_eq!(parse_reply(&[0; 9]), None);
        assert_eq!(parse_reply(&[0, 0xa1, 0, 0, 2, 0, 0x83, 1, 255, 0]), None);
    }
}
