//! G435 046D:0ADF uses CRC-16 vendor frames, not HID++. The older 0ACB transport differs.
//! Protocol: portyporty/g435-battery-percentage (MIT), deddroid/logitech-g435-battery.
use super::{simple, Driver, PollCtx};
use crate::device::{Kind, Observation, State};
use crate::hid::{self, HidDev};
use std::time::{Duration, Instant};

const VID: u16 = 0x046d;
const PID: u16 = 0x0adf;
// OTA_CMD_IOCTL_READ_MAX_LEN, sequence 1, from the capture.
const SESSION_INIT: &[u8] =
    &[0x50, 0x41, 0x06, 0x01, 0x0d, 0x00, 0x25, 0x34, 0x12, 0x5a, 0x5a, 0x01, 0, 0, 0, 0xf0, 0, 0, 0];

pub fn driver() -> Box<dyn Driver> {
    simple("Logitech G435", poll)
}

// CRC-16/XMODEM, polynomial 0x1021, initial value 0.
fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for byte in data {
        crc ^= u16::from(*byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

// CRC covers the complete zero-padded frame with its CRC bytes zeroed.
fn frame(cmd: u8, body: &[u8]) -> [u8; 64] {
    let mut out = [0; 64];
    out[0] = cmd;
    out[1] = body.len() as u8;
    for (dest, src) in out[4..].iter_mut().zip(body) {
        *dest = *src;
    }
    let crc = crc16(&out).to_le_bytes();
    out[2..4].copy_from_slice(&crc);
    out
}

fn pa(category: u8, seq: u8, len: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + payload.len());
    out.extend_from_slice(&[0x50, 0x41, category, seq, len]);
    out.extend_from_slice(payload);
    out
}

fn parse_millivolts(data: &[u8]) -> Option<u16> {
    data.windows(8).find_map(|w| {
        let &[0, 7, 0, 6, _, 0, lo, hi] = w else { return None };
        let mv = u16::from_le_bytes([lo, hi]);
        (2500..=4500).contains(&mv).then_some(mv)
    })
}

fn percent(mv: u16) -> u8 {
    hid::interpolate(
        i32::from(mv),
        &[
            (4200, 100),
            (4150, 95),
            (4100, 90),
            (4050, 85),
            (4000, 80),
            (3950, 75),
            (3900, 70),
            (3850, 62),
            (3800, 55),
            (3750, 45),
            (3700, 35),
            (3650, 25),
            (3600, 15),
            (3550, 10),
            (3500, 5),
            (3400, 0),
        ],
    )
}

fn transact(d: &HidDev, cmd: u8, body: &[u8], wait_ms: u64, battery: bool) -> Result<Option<u16>, ()> {
    let mut out = [0; 65];
    out[1..].copy_from_slice(&frame(cmd, body));
    d.flush();
    if !d.write(&out, 500) {
        return Err(());
    }
    let deadline = Instant::now() + Duration::from_millis(wait_ms);
    loop {
        let left = deadline.saturating_duration_since(Instant::now()).as_millis() as u32;
        if left == 0 {
            return Ok(None);
        }
        if let Some(reply) = d.read(left) {
            if battery {
                if let Some(mv) = parse_millivolts(&reply) {
                    return Ok(Some(mv));
                }
            }
        }
    }
}

struct Session<'a>(&'a HidDev);
impl Drop for Session<'_> {
    fn drop(&mut self) {
        let _ = transact(self.0, 0x85, &[2], 50, false);
    }
}

fn read_millivolts(d: &HidDev) -> Option<u16> {
    transact(d, 0x84, &[1], 50, false).ok()?;
    let _session = Session(d);
    let _ = transact(d, 0x85, &[1], 50, false);
    let _ = transact(d, 0x80, SESSION_INIT, 450, false);
    // SET_MODE selects command routing to the headset; it does not persist user settings.
    let _ = transact(d, 0x80, &pa(0x0e, 2, 1, &[0xe0]), 120, false);
    let _ = transact(d, 0x80, &pa(0x0e, 3, 2, &[0xe1, 1]), 120, false);
    let _ = transact(d, 0x80, &pa(0x0e, 4, 1, &[0xe0]), 120, false);
    transact(d, 0x80, &pa(6, 5, 1, &[0, 0x31]), 250, true).ok().flatten()
}

fn poll(ctx: &mut PollCtx) {
    if ctx.is_claimed(VID, PID) {
        return;
    }
    let Some(c) = ctx.find(VID, PID).find(|c| c.page == 0xff00 && c.usage == 1 && c.in_len == 65 && c.out_len == 65)
    else {
        return;
    };
    let Some(d) = HidDev::open(c) else { return };
    let id = "g435:046d:0adf";
    let observation = match read_millivolts(&d) {
        Some(mv) => Observation::battery(id, "Logitech G435", Kind::Headset, percent(mv), false).millivolts(mv),
        None => Observation::new(id, State::Off).name("Logitech G435").kind(Kind::Headset),
    };
    ctx.add(observation.via("2.4 GHz receiver"), VID, PID);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_and_frame() {
        assert_eq!(crc16(b"123456789"), 0x31c3);
        let f = frame(0x84, &[1]);
        assert_eq!(f.len(), 64);
        assert_eq!((f[0], f[1], f[4], f[5]), (0x84, 1, 1, 0));
        let mut zeroed = f;
        zeroed[2..4].fill(0);
        assert_eq!(u16::from_le_bytes([f[2], f[3]]), crc16(&zeroed));
        assert_eq!(pa(6, 5, 1, &[0, 0x31]), [0x50, 0x41, 6, 5, 1, 0, 0x31]);
    }

    #[test]
    fn battery_voltage() {
        let mut reply = [0; 64];
        reply[10..20].copy_from_slice(&[0x50, 0x49, 0, 7, 0, 6, 0x85, 0, 0xc4, 0x0e]);
        assert_eq!(parse_millivolts(&reply), Some(3780));
        reply[18..20].copy_from_slice(&[0xd2, 0]);
        assert_eq!(parse_millivolts(&reply), None);
        assert_eq!(parse_millivolts(&[0; 64]), None);
        assert_eq!(parse_millivolts(&[0, 7, 0, 6, 0x85, 0, 0xc4]), None);
    }

    #[test]
    fn voltage_curve() {
        for (mv, expected) in [(4250, 100), (4200, 100), (3800, 55), (3775, 50), (3400, 0), (3000, 0)] {
            assert_eq!(percent(mv), expected);
        }
    }
}
