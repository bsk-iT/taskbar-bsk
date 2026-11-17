use super::{Driver, PollCtx};
use crate::device::{Kind, Observation, State};
use crate::hid::{self, HidDev};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

// LED thresholds from zowie-battery-tray. Only battery commands are safe: other CW commands unpair.
const LEVELS: &[(i32, i32)] = &[(4200, 100), (4000, 75), (3890, 50), (3760, 25), (3680, 10), (3500, 0)];
#[derive(Default)]
struct ChargeTrend {
    has: bool,
    charging: bool,
    peak: i32,
    trough: i32,
}
impl ChargeTrend {
    fn update(&mut self, mv: i32) -> bool {
        if !self.has {
            self.has = true;
            self.charging = mv >= 4250;
            self.peak = mv;
            self.trough = mv;
        } else if self.charging {
            self.peak = self.peak.max(mv);
            if mv <= self.peak - 20 {
                self.charging = false;
                self.trough = mv;
            }
        } else {
            self.trough = self.trough.min(mv);
            if mv >= self.trough + 50 {
                self.charging = true;
                self.peak = mv;
            }
        }
        self.charging
    }
}
fn parse_cw(r: &[u8]) -> Option<(u16, u8)> {
    let [lo, hi, percent, ..] = r else {
        return None;
    };
    let mv = u16::from_le_bytes([*lo, *hi]);
    if mv != 0 && !(3000..=4500).contains(&mv) {
        return None;
    }
    Some((mv, if (1..=100).contains(percent) { *percent } else { hid::interpolate(mv as i32, LEVELS) }))
}
fn parse_dw(r: &[u8]) -> Option<i32> {
    let [0x09, hi, lo, _, 0xc8, ..] = r else {
        return None;
    };
    if *hi == 0xe6 {
        return None;
    }
    Some((*hi as i32 * 256 + *lo as i32) * 10)
}
#[derive(Default)]
struct Zowie {
    trends: BTreeMap<u16, ChargeTrend>,
}
pub fn driver() -> Box<dyn Driver> {
    Box::new(Zowie::default())
}
impl Zowie {
    fn poll_cw(&mut self, ctx: &mut PollCtx, pid: u16) {
        for c in ctx.find(0x04a5, pid).filter(|c| (c.page >= 0xff00 || c.page == 0x00ff) && c.out_len == 65) {
            let Some(d) = HidDev::open(c) else {
                continue;
            };
            d.flush();
            if !d.write(&[0, 0xa5, 3], 500) {
                continue;
            }
            let Some((mv, percent)) = d.read(1000).and_then(|r| parse_cw(&r)) else {
                continue;
            };
            let state = if mv == 0 { State::Off } else { State::Battery { percent, charging: pid == 0x8005 } };
            let mut s = Observation::new(format!("zowie:04a5:{pid:04x}"), state)
                .name("ZOWIE EC-CW")
                .kind(Kind::Mouse)
                .via(if pid == 0x8005 { "USB cable" } else { "2.4 GHz receiver" });
            if mv != 0 {
                s = s.millivolts(mv);
            }
            ctx.add(s, 0x04a5, pid);
            return;
        }
    }
    fn poll_dw(&mut self, ctx: &mut PollCtx, pid: u16) {
        let Some(cmd) = ctx.find(0x04a5, pid).find(|c| c.page == 0xff03 && c.out_len == 16) else {
            return;
        };
        let Some(rep) = ctx.find(0x04a5, pid).find(|c| c.page == 0xff04 && c.in_len == 16) else {
            return;
        };
        // The input channel must be open before sending the command on the output channel.
        let Some(input) = HidDev::open(rep) else {
            return;
        };
        let Some(output) = HidDev::open(cmd) else {
            return;
        };
        input.flush();
        if !output.write(&[8, 0xb4, 0x15], 500) {
            return;
        }
        let end = Instant::now() + Duration::from_millis(1500);
        let mut voltage = None;
        while let Some(left) = end.checked_duration_since(Instant::now()) {
            let Some(r) = input.read(left.as_millis() as u32) else {
                break;
            };
            if r.len() < 5 {
                break;
            }
            if r.first() == Some(&9) {
                voltage = parse_dw(&r);
                break;
            }
        }
        let Some(mv) = voltage else {
            return;
        };
        let cable = pid != 0x800a && pid != 0x8008;
        let mut name = hid::product(cmd);
        if pid == 0x8008 {
            name = "ZOWIE U2".into();
        } else if name.is_empty() || name.contains("Receiver") {
            name = "ZOWIE DW mouse".into();
        }
        let state = if mv == 0 {
            State::Off
        } else {
            State::Battery {
                percent: hid::interpolate(mv, LEVELS),
                charging: cable || self.trends.entry(pid).or_default().update(mv),
            }
        };
        let mut s = Observation::new(format!("zowie:04a5:{pid:04x}"), state)
            .name(name)
            .kind(Kind::Mouse)
            .via(if cable { "USB cable" } else { "2.4 GHz receiver" });
        if mv != 0 {
            s = s.millivolts(mv.min(u16::MAX as i32) as u16);
        }
        ctx.add(s, 0x04a5, pid);
    }
}
impl Driver for Zowie {
    fn name(&self) -> &'static str {
        "zowie"
    }
    fn poll(&mut self, ctx: &mut PollCtx) {
        for (_, pid) in ctx.products(0x04a5) {
            if matches!(pid, 0x8005 | 0x8006) {
                self.poll_cw(ctx, pid);
            } else {
                self.poll_dw(ctx, pid);
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn voltage_frames() {
        assert_eq!(parse_cw(&[0xa0, 0x0f, 80]), Some((4000, 80)));
        assert_eq!(parse_cw(&[0xa0, 0x0f, 0]), Some((4000, 75)));
        assert_eq!(parse_cw(&[0, 0, 0]), Some((0, 0)));
        assert_eq!(parse_cw(&[1, 0, 50]), None);
        assert_eq!(parse_cw(&[1]), None);
        assert_eq!(parse_dw(&[9, 1, 0x90, 0, 0xc8]), Some(4000));
        assert_eq!(parse_dw(&[9, 0xe6, 0, 0, 0xc8]), None);
        assert_eq!(parse_dw(&[9, 1, 0x90, 0, 0]), None);
    }
    #[test]
    fn dock_hysteresis() {
        let mut trend = ChargeTrend::default();
        for (mv, charging) in
            [(4000, false), (3980, false), (4029, false), (4030, true), (4050, true), (4031, true), (4030, false)]
        {
            assert_eq!(trend.update(mv), charging);
        }
        assert!(ChargeTrend::default().update(4250));
    }
}
