// Glorious/Hitscan/G-Wolves XVI and WLMouse YING: 65-byte report 0,
// request [3]=02 [4]=02 [6]=83; reply [1]=status [7]=charging [8]=percent.
// Sources: glorious-indicator, glorious-ctl, Hitscan/G-Wolves/WLMouse web configurators.
use super::{simple, Driver, PollCtx};
use crate::{
    device::{Kind, Observation, State},
    hid::{self, HidDev},
};
use std::{thread::sleep, time::Duration};

pub(super) fn gwolves_model_name(pid: u16) -> &'static str {
    match pid {
        0x3808 | 0x3817 => "G-Wolves HTM Plus",
        0x6808 | 0x6817 => "G-Wolves HSK Pro 2.0",
        0x5608 | 0x5617 | 0x5618 | 0x5619 => "G-Wolves HTXU",
        0x3608 | 0x3617 | 0x3619 => "G-Wolves Fenrir Pro",
        0x3908 | 0x3917 => "G-Wolves VUK",
        0x7904 | 0x7913 => "G-Wolves HT-S2",
        0x3708 | 0x3717 => "G-Wolves Fenrir Max",
        0x5308 | 0x5317 => "G-Wolves HTS Ultra",
        0x7704 | 0x7713 => "G-Wolves HTR",
        0x3508 | 0x3517 => "G-Wolves Fenrir",
        0x7908 | 0x7917 => "G-Wolves HT-S2 Pro",
        0x5808 | 0x5817 => "G-Wolves HSK Pro",
        0x2708 | 0x2717 | 0x2719 => "G-Wolves HTX Mini",
        0x5408 | 0x5417 | 0x5418 | 0x5419 => "G-Wolves HTS Plus",
        0x5708 | 0x5717 => "G-Wolves HTX",
        0x5908 | 0x5917 => "G-Wolves HSK Plus",
        0x7204 | 0x7203 => "G-Wolves HSK Lite",
        0x7708 | 0x7717 => "G-Wolves HTR Pro",
        0x5804 | 0x5803 => "G-Wolves HSK Pro ACE",
        0x5404 | 0x5403 => "G-Wolves HTS Plus ACE",
        0x5704 | 0x5703 => "G-Wolves HTX ACE",
        0x5904 | 0x5903 => "G-Wolves HSK Plus ACE",
        0x4718 | 0x4719 => "G-Wolves Lycan",
        0x4219 => "G-Wolves WARG",
        0x5219 => "G-Wolves HTS Plus Pro",
        0x3519 => "G-Wolves Fenrir Asym",
        0x2619 => "G-Wolves HTX Mini Asym",
        0x1619 => "G-Wolves Fenrir Max Asym",
        0x2119 => "G-Wolves VUK Mini",
        _ => "G-Wolves wireless mouse",
    }
}

pub(super) fn gwolves_is_cable(pid: u16) -> bool {
    matches!(
        pid,
        0x1619
            | 0x2119
            | 0x2619
            | 0x2708
            | 0x2719
            | 0x3508
            | 0x3519
            | 0x3608
            | 0x3619
            | 0x3708
            | 0x3808
            | 0x3908
            | 0x4219
            | 0x4718
            | 0x4719
            | 0x5219
            | 0x5308
            | 0x5404
            | 0x5408
            | 0x5418
            | 0x5419
            | 0x5608
            | 0x5618
            | 0x5619
            | 0x5704
            | 0x5708
            | 0x5804
            | 0x5808
            | 0x5904
            | 0x5908
            | 0x6808
            | 0x7204
            | 0x7704
            | 0x7708
            | 0x7904
            | 0x7908
    )
}

fn glorious_name(pid: u16) -> Option<&'static str> {
    Some(match pid {
        0x2022 | 0x2011 => "Glorious Model O Wireless",
        0x2023 | 0x2012 => "Glorious Model D Wireless",
        0x2024 | 0x2013 => "Glorious Model O- Wireless",
        0x2025 | 0x2014 => "Glorious Model D- Wireless",
        0x2033 => "Glorious Model O 2 Wireless",
        0x2034 | 0x2036 | 0x201a => "Glorious Model D 2 PRO",
        _ => return None,
    })
}

// XVI feature report: status A1 = awake, A4 = asleep, A0 = waking.
fn parse_reply(r: &[u8]) -> Option<(u8, u8, bool)> {
    let (&status, &command, &charging, &percent) = (r.get(1)?, r.get(6)?, r.get(7)?, r.get(8)?);
    (command == 0x83 && status & 0xf0 == 0xa0).then_some((status, percent, charging == 1))
}

pub fn driver() -> Box<dyn Driver> {
    simple("glorious", poll)
}
fn poll(ctx: &mut PollCtx) {
    for (vendor, prefix, delay) in
        [(0x258a, "glorious", 50), (0x3770, "hitscan", 200), (0x33e4, "gwolves", 100), (0x36a7, "wlmouse", 100)]
    {
        for (vid, pid) in ctx.products(vendor) {
            let colls = ctx.colls.iter().filter(|c| {
                c.vid == vid
                    && c.pid == pid
                    && c.feat_len == 65
                    && (vid != 0x36a7 || (pid == 0xa874 && c.page == 0xffff && c.usage == 0))
            });
            for c in colls {
                let Some(d) = HidDev::open_feature_only(c) else { continue };
                let mut answer = None;
                for attempt in 0..2 {
                    let mut w = [0; 65];
                    w[3] = 2;
                    w[4] = 2;
                    w[6] = 0x83;
                    if !d.set_feature(&w) {
                        break;
                    }
                    sleep(Duration::from_millis(delay));
                    let Some(r) = d.get_feature(0) else { break };
                    let Some(parsed) = parse_reply(&r) else { break };
                    if parsed.0 == 0xa0 && attempt == 0 {
                        sleep(Duration::from_millis(400));
                        continue;
                    }
                    answer = Some(parsed);
                    break;
                }
                let Some((status, percent, charging)) = answer else { continue };
                let known = match vid {
                    0x258a => glorious_name(pid),
                    0x3770 => Some(match pid {
                        0x0136 => "Hitscan Hyperlight X",
                        0x0194 => "Hitscan Hyperlight N",
                        _ => "Hitscan Hyperlight",
                    }),
                    0x33e4 => Some(gwolves_model_name(pid)),
                    _ => Some("WLMouse YING"),
                };
                let name = known.map(str::to_owned).unwrap_or_else(|| {
                    let n = hid::product(c);
                    if n.is_empty() {
                        "Wireless device".into()
                    } else {
                        n
                    }
                });
                let kind = if known.is_some() {
                    Kind::Mouse
                } else {
                    let guessed = Kind::from_name(&name);
                    if guessed != Kind::Other {
                        guessed
                    } else if ctx
                        .colls
                        .iter()
                        .filter(|c| c.vid == vid && c.pid == pid && c.page == 1 && c.usage == 6)
                        .count()
                        >= 2
                    {
                        Kind::Keyboard
                    } else {
                        hid::kind_from_collections(ctx.colls, vid, pid)
                    }
                };
                let cable = match vid {
                    0x258a => matches!(pid, 0x2011 | 0x2012 | 0x2013 | 0x2014 | 0x201a),
                    0x3770 => matches!(pid, 0x0136 | 0x0194),
                    0x33e4 => gwolves_is_cable(pid),
                    _ => false,
                };
                let id = format!("{prefix}:{vid:04x}:{pid:04x}");
                let obs = if status == 0xa1 && percent <= 100 {
                    Observation::battery(id, name, kind, percent.max(1), cable || charging)
                } else {
                    Observation::new(id, State::Unreachable).name(name).kind(kind)
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
    fn statuses() {
        assert_eq!(parse_reply(&[0, 0xa1, 0, 0, 2, 0, 0x83, 1, 57]), Some((0xa1, 57, true)));
        assert_eq!(parse_reply(&[0, 0xa4, 0, 0, 2, 0, 0x83, 0, 0]), Some((0xa4, 0, false)));
        assert_eq!(parse_reply(&[0; 8]), None);
        assert_eq!(parse_reply(&[0; 9]), None);
    }
}
