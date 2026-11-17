// ATK HUB N9 Pro / first-generation Ultra: virtual COM, 9600 8N1, 64-byte frames.
// Command 00/04 reads link, 00/03 reads percent (bits 0..6) and charging (bit 7).
// Source: ATK HUB 3.2.31 n9-pro model, myfinder-device-battery protocol notes.
use super::{simple, Driver, PollCtx};
use crate::device::{Kind, Observation, State};
use std::{
    mem::{size_of, size_of_val, zeroed},
    ptr::{null, null_mut},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Devices::{Communication::*, DeviceAndDriverInstallation::*},
    Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE},
    Storage::FileSystem::{CreateFileW, ReadFile, WriteFile, OPEN_EXISTING},
    System::Registry::{RegCloseKey, RegQueryValueExW, KEY_READ, REG_SZ},
};
const MODELS: &[(u16, &str, bool)] = &[
    (0x11cb, "ATK N9 Pro", false),
    (0x11ca, "ATK N9 Pro", true),
    (0x1204, "ATK N9 Ultra", false),
    (0x1203, "ATK N9 Ultra", true),
];
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn model_from_instance(id: &str) -> Option<usize> {
    let id = id.to_uppercase();
    MODELS
        .iter()
        .enumerate()
        .filter(|(_, (pid, _, _))| id.contains(&format!("VID_373B&PID_{pid:04X}")))
        .map(|(i, _)| i)
        .next_back()
}
fn find_ports() -> Vec<(String, usize)> {
    let mut out = Vec::new();
    unsafe {
        let di = SetupDiGetClassDevsW(&GUID_DEVCLASS_PORTS, null(), null_mut(), DIGCF_PRESENT);
        if di == -1 {
            return out;
        }
        let mut dd: SP_DEVINFO_DATA = zeroed();
        dd.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;
        let port_name = wide("PortName");
        let mut i = 0;
        while SetupDiEnumDeviceInfo(di, i, &mut dd) != 0 {
            i += 1;
            let mut inst = [0u16; 512];
            if SetupDiGetDeviceInstanceIdW(di, &dd, inst.as_mut_ptr(), 512, null_mut()) == 0 {
                continue;
            }
            let len = inst.iter().position(|c| *c == 0).unwrap_or(inst.len());
            let Some(model) = model_from_instance(&String::from_utf16_lossy(&inst[..len])) else { continue };
            let key = SetupDiOpenDevRegKey(di, &dd, DICS_FLAG_GLOBAL, 0, DIREG_DEV, KEY_READ);
            if key == INVALID_HANDLE_VALUE {
                continue;
            }
            let mut name = [0u16; 32];
            let mut size = (size_of_val(&name) - size_of::<u16>()) as u32;
            let mut kind = 0;
            if RegQueryValueExW(key, port_name.as_ptr(), null(), &mut kind, name.as_mut_ptr().cast(), &mut size) == 0
                && kind == REG_SZ
            {
                let len = name.iter().position(|c| *c == 0).unwrap_or(name.len());
                out.push((String::from_utf16_lossy(&name[..len]), model));
            }
            RegCloseKey(key);
        }
        SetupDiDestroyDeviceInfoList(di);
    }
    out
}
struct Serial(HANDLE);
impl Drop for Serial {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
impl Serial {
    fn open(port: &str) -> Option<Self> {
        unsafe {
            let path = wide(&format!("\\\\.\\{port}"));
            let h = CreateFileW(path.as_ptr(), GENERIC_READ | GENERIC_WRITE, 0, null(), OPEN_EXISTING, 0, null_mut());
            if h == INVALID_HANDLE_VALUE {
                return None;
            }
            let port = Self(h);
            let mut dcb: DCB = zeroed();
            dcb.DCBlength = size_of::<DCB>() as u32;
            GetCommState(h, &mut dcb);
            dcb.BaudRate = 9600;
            dcb.ByteSize = 8;
            dcb.Parity = NOPARITY;
            dcb.StopBits = ONESTOPBIT;
            // DCB packs binary, CTS/DSR, DTR, software flow control and RTS into one DWORD.
            dcb._bitfield = (dcb._bitfield & !((1 << 2) | (1 << 3) | (3 << 4) | (1 << 8) | (1 << 9) | (3 << 12)))
                | 1
                | (1 << 4)
                | (1 << 12);
            let to = COMMTIMEOUTS {
                ReadIntervalTimeout: 50,
                ReadTotalTimeoutMultiplier: 0,
                ReadTotalTimeoutConstant: 300,
                WriteTotalTimeoutMultiplier: 0,
                WriteTotalTimeoutConstant: 300,
            };
            if SetCommState(h, &dcb) == 0 || SetCommTimeouts(h, &to) == 0 {
                return None;
            }
            PurgeComm(h, PURGE_RXCLEAR | PURGE_TXCLEAR);
            Some(port)
        }
    }
    fn command(&self, cmd: u8, sub: u8) -> Option<[u8; 61]> {
        let frame = request(cmd, sub);
        let mut n = 0;
        if unsafe { WriteFile(self.0, frame.as_ptr(), 64, &mut n, null_mut()) } == 0 || n != 64 {
            return None;
        }
        let end = Instant::now() + Duration::from_millis(800);
        let mut buf = Vec::new();
        while Instant::now() < end {
            let mut chunk = [0; 128];
            if unsafe { ReadFile(self.0, chunk.as_mut_ptr(), 128, &mut n, null_mut()) } == 0 {
                return None;
            }
            buf.extend_from_slice(chunk.get(..n as usize)?);
            while buf.len() >= 64 {
                if let Some(data) = parse_frame(&buf, cmd, sub) {
                    return Some(data);
                }
                buf.drain(..64);
            }
        }
        None
    }
}
fn request(cmd: u8, sub: u8) -> [u8; 64] {
    let mut r = [0; 64];
    r[0] = 2;
    r[1] = cmd;
    r[2] = sub;
    r
}
fn parse_frame(r: &[u8], cmd: u8, sub: u8) -> Option<[u8; 61]> {
    if r.get(1) != Some(&cmd) || r.get(2) != Some(&sub) {
        return None;
    }
    r.get(3..64)?.try_into().ok()
}
fn parse_battery(data: &[u8]) -> Option<(u8, bool)> {
    let b = *data.get(1)?;
    Some(((b & 0x7f).min(100), b & 0x80 != 0))
}
pub fn driver() -> Box<dyn Driver> {
    simple("atk_serial", poll)
}
fn poll(ctx: &mut PollCtx) {
    for (name, model) in find_ports() {
        let Some(&(pid, model_name, wired)) = MODELS.get(model) else { continue };
        if ctx.is_claimed(0x373b, pid) {
            continue;
        }
        let Some(port) = Serial::open(&name) else { continue };
        let Some(data) = port.command(0, 4) else { continue };
        let connected = data.get(1) == Some(&1);
        let state = if connected {
            match port.command(0, 3).and_then(|d| parse_battery(&d)) {
                Some((percent, charging)) => State::Battery { percent, charging },
                None => State::Unreachable,
            }
        } else {
            State::Off
        };
        ctx.add(
            Observation::new(format!("atk-serial:373b:{pid:04x}"), state)
                .name(model_name)
                .kind(Kind::Headset)
                .via(if wired { "USB cable" } else { "2.4 GHz receiver" }),
            0x373b,
            pid,
        );
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn serial_frames() {
        let req = request(0, 3);
        assert_eq!(&req[..4], &[2, 0, 3, 0]);
        assert!(req[3..].iter().all(|b| *b == 0));
        let mut r = req;
        r[4] = 0x80 | 70;
        let data = parse_frame(&r, 0, 3).expect("valid fixture");
        assert_eq!(parse_battery(&data), Some((70, true)));
        r[4] = 127;
        assert_eq!(parse_frame(&r, 0, 3).and_then(|d| parse_battery(&d)), Some((100, false)));
        assert!(parse_frame(&r, 0, 4).is_none());
        assert!(parse_frame(&r[..63], 0, 3).is_none());
        assert_eq!(parse_battery(&[]), None);
    }
    #[test]
    fn port_models() {
        for (i, (pid, _, _)) in MODELS.iter().enumerate() {
            assert_eq!(model_from_instance(&format!("USB\\VID_373b&PID_{pid:04x}\\1")), Some(i));
        }
        assert_eq!(model_from_instance("USB\\VID_373B&PID_1216"), None);
    }
}
