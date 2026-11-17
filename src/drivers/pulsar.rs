// Pulsar Sonix64 report 0: packet 08 81 01 reads battery at packet byte 6.
// Status 01 answers, 02/05 remain pending; final 05 means offline.
// Sources: PulsarBattery and pulsar-mouse-linux; higher interfaces answer first.
use super::{simple, Driver, PollCtx};
use crate::{
    device::{Kind, Observation, State},
    hid::{self, HidDev},
};
use std::{thread::sleep, time::Duration};

#[derive(Debug, PartialEq)]
enum Reply {
    Battery(u8),
    Offline,
    Pending(bool),
    Invalid,
}
// Sonix64 packet starts after report ID; checksum sums packet bytes 0..61.
fn request() -> [u8; 65] {
    let mut r = [0; 65];
    r[2] = 8;
    r[3] = 0x81;
    r[4] = 1;
    let sum: u16 = r.iter().skip(1).take(62).map(|b| u16::from(*b)).sum();
    r[63] = sum as u8;
    r[64] = (sum >> 8) as u8;
    r
}
fn parse_reply(r: &[u8]) -> Reply {
    if r.len() < 65 {
        return Reply::Invalid;
    }
    if r.get(2..5) != Some(&[8, 0x81, 1][..]) {
        return Reply::Pending(false);
    }
    match (r.get(1), r.get(7)) {
        (Some(1), Some(p)) if *p <= 100 => Reply::Battery(*p),
        (Some(1), _) => Reply::Invalid,
        (Some(status), _) => Reply::Pending(*status == 5),
        _ => Reply::Invalid,
    }
}
fn query(d: &HidDev) -> Reply {
    let mut offline = false;
    for _ in 0..3 {
        if !d.set_feature(&request()) {
            return Reply::Invalid;
        }
        for _ in 0..6 {
            sleep(Duration::from_millis(50));
            let Some(r) = d.get_feature(0) else { return Reply::Invalid };
            // Unrelated replies do not clear the last matching offline status.
            if r.len() >= 65 && r.get(2..5) != Some(&[8, 0x81, 1][..]) {
                continue;
            }
            match parse_reply(&r) {
                Reply::Pending(value) => offline = value,
                answer => return answer,
            }
        }
    }
    if offline {
        Reply::Offline
    } else {
        Reply::Invalid
    }
}
pub fn driver() -> Box<dyn Driver> {
    simple("pulsar", poll)
}
fn poll(ctx: &mut PollCtx) {
    for (pid, cable, name) in
        [(0x3402, true, "Pulsar X2 V3"), (0x3406, true, "Pulsar X2 V3 eS"), (0x5402, false, ""), (0x5403, false, "")]
    {
        if ctx.is_claimed(0x3710, pid) {
            continue;
        }
        let mut colls: Vec<_> =
            ctx.colls.iter().filter(|c| c.vid == 0x3710 && c.pid == pid && c.feat_len >= 65).collect();
        colls.sort_by_key(|c| std::cmp::Reverse(c.iface));
        for c in colls {
            let Some(d) = HidDev::open_feature_only(c) else { continue };
            let rep = query(&d);
            if matches!(rep, Reply::Invalid | Reply::Pending(_)) {
                continue;
            }
            let name = if cable {
                name.to_owned()
            } else {
                let n = hid::product(c);
                if n.is_empty() || n.contains("Dongle") || n.contains("Receiver") {
                    "Pulsar wireless mouse".into()
                } else {
                    n
                }
            };
            let id = format!("pulsar:3710:{pid:04x}");
            let obs = match rep {
                Reply::Battery(p) if cable || p > 0 => Observation::battery(id, name, Kind::Mouse, p, cable),
                _ => Observation::new(id, if cable { State::Unreachable } else { State::Off })
                    .name(name)
                    .kind(Kind::Mouse),
            };
            ctx.add(obs.via(if cable { "USB cable" } else { "2.4 GHz receiver" }), 0x3710, pid);
            break;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sonix_packet() {
        let q = request();
        assert_eq!(&q[2..5], &[8, 0x81, 1]);
        assert_eq!(&q[63..], &[0x8a, 0]);
        let mut r = q;
        r[1] = 1;
        r[7] = 72;
        assert_eq!(parse_reply(&r), Reply::Battery(72));
        r[7] = 255;
        assert_eq!(parse_reply(&r), Reply::Invalid);
        r[1] = 5;
        assert_eq!(parse_reply(&r), Reply::Pending(true));
        assert_eq!(parse_reply(&r[..64]), Reply::Invalid);
    }
}
