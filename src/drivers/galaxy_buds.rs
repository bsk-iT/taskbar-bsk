//! Read-only Samsung SPP status frames; no bytes are sent to earbuds.
use super::{Driver, PollCtx};
use crate::device::{Kind, Observation, State};
use std::{
    mem::{size_of, zeroed},
    ptr::{null, null_mut},
    time::{Duration, Instant},
};
use windows_sys::{
    core::GUID,
    Win32::{
        Devices::{
            Bluetooth::{AF_BTH, BTHPROTO_RFCOMM, SOCKADDR_BTH},
            DeviceAndDriverInstallation::*,
            Properties::{DEVPROPKEY, DEVPROP_TYPE_BOOLEAN, DEVPROP_TYPE_STRING},
        },
        Networking::WinSock::*,
    },
};
const CONNECTED: DEVPROPKEY = DEVPROPKEY { fmtid: GUID::from_u128(0x83da6326_97a6_4088_9453_a1923f573b29), pid: 15 };
const FRIENDLY: DEVPROPKEY = DEVPROPKEY { fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0), pid: 14 };
const SERVICES: &[(GUID, bool)] = &[
    (GUID::from_u128(0x2e73a4ad_332d_41fc_90e2_16bef06523f2), false),
    (GUID::from_u128(0x00001101_0000_1000_8000_00805f9b34fb), false),
    (GUID::from_u128(0x00001102_0000_1000_8000_00805f9b34fd), true),
];
#[derive(Debug, PartialEq, Eq)]
struct Status {
    left: i32,
    right: i32,
    case: i32,
}
fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for b in data {
        crc ^= u16::from(*b) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}
fn level(v: u8) -> i32 {
    if (1..=100).contains(&v) {
        i32::from(v)
    } else {
        -1
    }
}
// Modern length uses low ten bits; bit 0x2000 denotes firmware fragments.
fn find_status(data: &[u8], legacy: bool) -> Option<Status> {
    let mut latest = None;
    let mut i = 0;
    while i + 6 <= data.len() {
        let Some(header) = data.get(i..i + 3) else { break };
        if header.first() != Some(&if legacy { 0xfe } else { 0xfd }) {
            i += 1;
            continue;
        }
        let [_, low, high] = header else { break };
        let raw = u16::from_le_bytes([*low, *high]);
        let len = if legacy { usize::from(*high) } else { usize::from(raw & 0x3ff) };
        if (!legacy && raw & 0x2000 != 0) || len < 3 || data.get(i + 3 + len) != Some(&if legacy { 0xee } else { 0xdd })
        {
            i += 1;
            continue;
        }
        let Some(body) = data.get(i + 3..i + 3 + len) else {
            i += 1;
            continue;
        };
        let Some(check) = body.get(len - 2..) else {
            i += 1;
            continue;
        };
        let Some(content) = body.get(..len - 2) else {
            i += 1;
            continue;
        };
        let [low, high] = check else {
            i += 1;
            continue;
        };
        if crc16(content) != u16::from_le_bytes([*low, *high]) {
            i += 1;
            continue;
        }
        let p = content.get(1..).unwrap_or_default();
        let offset = match content.first() {
            Some(0x61) if p.len() >= 4 => 2,
            Some(0x60) if p.len() >= 3 => 1,
            _ => {
                i += 1;
                continue;
            }
        };
        let (Some(l), Some(r)) = (p.get(offset), p.get(offset + 1)) else {
            i += 1;
            continue;
        };
        latest = Some(Status {
            left: level(*l),
            right: level(*r),
            case: if legacy { -1 } else { p.get(offset + 5).copied().map(level).unwrap_or(-1) },
        });
        i += 4 + len;
    }
    latest
}
fn connected_buds() -> Vec<(String, String, u64)> {
    let mut out = Vec::new();
    let enumerator: Vec<u16> = "BTHENUM\0".encode_utf16().collect();
    unsafe {
        let di = SetupDiGetClassDevsW(null(), enumerator.as_ptr(), null_mut(), DIGCF_ALLCLASSES | DIGCF_PRESENT);
        if di == -1 {
            return out;
        }
        let mut dd: SP_DEVINFO_DATA = zeroed();
        dd.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;
        let mut i = 0;
        while SetupDiEnumDeviceInfo(di, i, &mut dd) != 0 {
            i += 1;
            let mut inst = [0u16; 512];
            if SetupDiGetDeviceInstanceIdW(di, &dd, inst.as_mut_ptr(), 512, null_mut()) == 0 {
                continue;
            }
            let id = String::from_utf16_lossy(&inst[..inst.iter().position(|v| *v == 0).unwrap_or(inst.len())]);
            let upper = id.to_ascii_uppercase();
            let Some(hex) = upper.strip_prefix("BTHENUM\\DEV_").and_then(|v| v.get(..12)) else { continue };
            let Ok(addr) = u64::from_str_radix(hex, 16) else { continue };
            if addr == 0 {
                continue;
            }
            let mut ty = 0;
            let mut conn = 0u8;
            if SetupDiGetDevicePropertyW(di, &dd, &CONNECTED, &mut ty, &mut conn, 1, null_mut(), 0) == 0
                || ty != DEVPROP_TYPE_BOOLEAN
                || conn == 0
            {
                continue;
            }
            let mut name = [0u16; 256];
            if SetupDiGetDevicePropertyW(di, &dd, &FRIENDLY, &mut ty, name.as_mut_ptr().cast(), 510, null_mut(), 0) == 0
                || ty != DEVPROP_TYPE_STRING
            {
                continue;
            }
            let name = String::from_utf16_lossy(&name[..name.iter().position(|v| *v == 0).unwrap_or(name.len())]);
            if name.to_lowercase().contains("buds") {
                out.push((name, hex.to_owned(), addr));
            }
        }
        SetupDiDestroyDeviceInfoList(di);
    }
    out
}
fn wait_socket(s: SOCKET, write: bool, ms: i32) -> bool {
    let mut set = FD_SET { fd_count: 1, fd_array: [0; 64] };
    set.fd_array[0] = s;
    let mut err = set;
    let tv = TIMEVAL { tv_sec: ms / 1000, tv_usec: ms % 1000 * 1000 };
    unsafe {
        select(0, if write { null_mut() } else { &mut set }, if write { &mut set } else { null_mut() }, &mut err, &tv)
            > 0
            && set.fd_count != 0
            && err.fd_count == 0
    }
}
struct Socket(SOCKET);
impl Drop for Socket {
    fn drop(&mut self) {
        unsafe {
            closesocket(self.0);
        }
    }
}
fn read_status(addr: u64, service: GUID, legacy: bool) -> Option<Status> {
    unsafe {
        let s = Socket(socket(i32::from(AF_BTH), SOCK_STREAM, BTHPROTO_RFCOMM as i32));
        if s.0 == INVALID_SOCKET {
            return None;
        }
        let mut nonblocking = 1;
        ioctlsocket(s.0, FIONBIO, &mut nonblocking);
        let sa = SOCKADDR_BTH { addressFamily: AF_BTH, btAddr: addr, serviceClassId: service, port: 0 };
        if connect(s.0, (&sa as *const SOCKADDR_BTH).cast(), size_of::<SOCKADDR_BTH>() as i32) != 0
            && !(WSAGetLastError() == WSAEWOULDBLOCK && wait_socket(s.0, true, 4000))
        {
            return None;
        }
        let end = Instant::now() + Duration::from_secs(3);
        let mut bytes = Vec::new();
        while bytes.len() < 8192 {
            let ms = end.saturating_duration_since(Instant::now()).as_millis() as i32;
            if ms == 0 || !wait_socket(s.0, false, ms) {
                break;
            }
            let mut chunk = [0; 512];
            let n = recv(s.0, chunk.as_mut_ptr(), 512, 0);
            if n <= 0 {
                break;
            }
            let Some(chunk) = chunk.get(..n as usize) else { break };
            bytes.extend_from_slice(chunk);
            if let Some(st) = find_status(&bytes, legacy) {
                return Some(st);
            }
        }
    }
    None
}
#[derive(Default)]
struct GalaxyBuds {
    winsock: Option<bool>,
}
pub fn driver() -> Box<dyn Driver> {
    Box::new(GalaxyBuds::default())
}
impl Drop for GalaxyBuds {
    fn drop(&mut self) {
        if self.winsock == Some(true) {
            unsafe {
                WSACleanup();
            }
        }
    }
}
impl Driver for GalaxyBuds {
    fn name(&self) -> &'static str {
        "Galaxy Buds"
    }
    fn poll(&mut self, ctx: &mut PollCtx) {
        let list = connected_buds();
        if list.is_empty() {
            return;
        }
        let ok = *self.winsock.get_or_insert_with(|| unsafe {
            let mut wsa = zeroed();
            WSAStartup(0x0202, &mut wsa) == 0
        });
        if !ok {
            return;
        }
        for (name, hex, addr) in list {
            let Some(s) = SERVICES.iter().find_map(|(uuid, legacy)| read_status(addr, *uuid, *legacy)) else {
                continue;
            };
            let percent = match (s.left, s.right) {
                (l, r) if l >= 0 && r >= 0 => l.min(r),
                (l, _) if l >= 0 => l,
                (_, r) => r,
            };
            let id = format!("galaxybuds:{}", hex.to_ascii_lowercase());
            let obs = if percent >= 0 {
                Observation::battery(id, name, Kind::Headset, percent as u8, false)
            } else {
                Observation::new(id, State::Unreachable).name(name).kind(Kind::Headset)
            };
            let pct = |v| if v >= 0 { format!("{v}%") } else { "-".into() };
            let mut detail = format!("E {}, D {}", pct(s.left), pct(s.right));
            if s.case >= 0 {
                detail.push_str(&format!(", estojo {}", pct(s.case)));
            }
            ctx.out.push(obs.via("Bluetooth").detail(detail).grace(Duration::from_secs(180)));
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn frame(id: u8, payload: &[u8], legacy: bool) -> Vec<u8> {
        let mut body = vec![id];
        body.extend_from_slice(payload);
        let crc = crc16(&body);
        let len = body.len() + 2;
        let mut f = if legacy { vec![0xfe, 0, len as u8] } else { vec![0xfd, len as u8, (len >> 8) as u8] };
        f.extend(body);
        f.extend(crc.to_le_bytes());
        f.push(if legacy { 0xee } else { 0xdd });
        f
    }
    #[test]
    fn captured_frames() {
        let pro = [
            0xFD, 0x31, 0x00, 0x61, 0x0D, 0x04, 0x64, 0x64, 0x01, 0x00, 0x33, 0x58, 0x00, 0x01, 0xBF, 0x22, 0x00, 0x01,
            0x46, 0x01, 0x46, 0x01, 0x00, 0x00, 0x03, 0x35, 0x00, 0x03, 0x00, 0x10, 0x01, 0x01, 0x01, 0x01, 0x32, 0x03,
            0x01, 0x01, 0x01, 0x00, 0x0C, 0xAE, 0x01, 0x00, 0x03, 0x00, 0x01, 0x00, 0x01, 0x01, 0xB6, 0x37, 0xDD,
        ];
        let plus = [
            0xFD, 0x1B, 0x10, 0x61, 0x0D, 0x00, 0x57, 0x54, 0x01, 0x00, 0x11, 0x65, 0x00, 0x00, 0x00, 0x03, 0x00, 0x22,
            0x01, 0x04, 0x01, 0x04, 0x01, 0x00, 0x00, 0x00, 0x03, 0x00, 0x1E, 0x55, 0xDD,
        ];
        let fe = [
            0xFD, 0x31, 0x10, 0x61, 0x02, 0x06, 0x3A, 0x59, 0x01, 0x00, 0x11, 0x00, 0x00, 0x00, 0x8F, 0x33, 0x00, 0x00,
            0x4A, 0x01, 0x4A, 0x01, 0x03, 0x00, 0x04, 0x65, 0x00, 0x01, 0x00, 0x10, 0x00, 0x00, 0x01, 0x01, 0x11, 0x03,
            0x00, 0x00, 0x00, 0x00, 0xE8, 0x03, 0x00, 0x00, 0x9D, 0x0E, 0x1D, 0x00, 0x50, 0x00, 0xC2, 0xCF, 0xDD,
        ];
        let original = [
            0xFE, 0x01, 0x12, 0x61, 0x03, 0x00, 0x46, 0x5F, 0x01, 0x00, 0x00, 0x00, 0x00, 0x02, 0x01, 0x07, 0x00, 0x22,
            0x01, 0xA5, 0x75, 0xEE,
        ];
        assert_eq!(find_status(&pro, false), Some(Status { left: 100, right: 100, case: 88 }));
        assert_eq!(find_status(&plus, false), Some(Status { left: 87, right: 84, case: -1 }));
        assert_eq!(find_status(&fe, false), Some(Status { left: 58, right: 89, case: -1 }));
        assert_eq!(find_status(&original, true), Some(Status { left: 70, right: 95, case: -1 }));
        assert_eq!(find_status(&original, false), None);
        let mut bad = pro;
        bad[6] = 0x50;
        assert_eq!(find_status(&bad, false), None);
        let mut stream = vec![0, 0xfd, 0x12];
        stream.extend(pro);
        stream.extend(frame(0x40, &[1, 2], false));
        let update = frame(0x60, &[0x0d, 0x4b, 0, 1, 0, 0x13, 0x2a, 0], false);
        stream.extend(&update);
        assert_eq!(find_status(&stream, false), Some(Status { left: 75, right: -1, case: 42 }));
        assert_eq!(find_status(&update[..update.len() - 1], false), None);
        let mut frag = update;
        frag[2] |= 0x20;
        assert_eq!(find_status(&frag, false), None);
        assert_eq!(crc16(b"123456789"), 0x31c3);
    }
}
