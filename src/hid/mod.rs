//! Thin Win32 HID layer shared by every driver.
//!
//! Report buffers follow hidapi conventions, so protocol code from other projects maps 1:1:
//! - `write`/`set_feature`: `data[0]` is the report ID (0 when the device does not number its
//!   reports); the buffer is zero-padded to the collection's report length.
//! - `read`: a leading 0 report ID is stripped, so `buf[0]` is the first payload byte for
//!   unnumbered reports and the report ID for numbered ones.
//! - `get_feature`/`get_input_report`: the returned buffer starts with the report ID.

mod dev;

pub use dev::HidDev;

use crate::device::Kind;
use std::ptr::{null, null_mut};
use windows_sys::core::GUID;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::*;
use windows_sys::Win32::Devices::HumanInterfaceDevice::*;
use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};

/// One HID top-level collection. A USB device usually exposes several (mouse, keyboard,
/// vendor-defined...), each with its own path and report lengths.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HidColl {
    /// NUL-terminated UTF-16 device path, ready for `CreateFileW`.
    pub path: Vec<u16>,
    pub vid: u16,
    pub pid: u16,
    /// USB interface number (`&mi_xx` in the path), -1 when the path has none.
    pub iface: i32,
    pub page: u16,
    pub usage: u16,
    pub in_len: u16,
    pub out_len: u16,
    pub feat_len: u16,
}

/// Lists every present HID collection. Each one is opened with zero access (no I/O reaches
/// the device) to read its ids and report lengths. `ignored` (vid, pid) pairs are skipped
/// before opening, so a misbehaving device is never touched.
pub fn enumerate(ignored: &[(u16, u16)]) -> Vec<HidColl> {
    let mut out = Vec::new();
    unsafe {
        let mut guid: GUID = std::mem::zeroed();
        HidD_GetHidGuid(&mut guid);
        let set = SetupDiGetClassDevsW(&guid, null(), null_mut(), DIGCF_PRESENT | DIGCF_DEVICEINTERFACE);
        if set == INVALID_HANDLE_VALUE as HDEVINFO {
            return out;
        }
        let mut ifd: SP_DEVICE_INTERFACE_DATA = std::mem::zeroed();
        ifd.cbSize = size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
        let mut i = 0;
        while SetupDiEnumDeviceInterfaces(set, null(), &guid, i, &mut ifd) != 0 {
            i += 1;
            if let Some(path) = interface_path(set, &ifd) {
                let (vid, pid) = ids_from_path(&path);
                if vid != 0 && ignored.contains(&(vid, pid)) {
                    continue;
                }
                if let Some(c) = describe(path).filter(|c| !ignored.contains(&(c.vid, c.pid))) {
                    out.push(c);
                }
            }
        }
        SetupDiDestroyDeviceInfoList(set);
    }
    out
}

unsafe fn interface_path(set: HDEVINFO, ifd: &SP_DEVICE_INTERFACE_DATA) -> Option<Vec<u16>> {
    let mut need = 0u32;
    SetupDiGetDeviceInterfaceDetailW(set, ifd, null_mut(), 0, &mut need, null_mut());
    if need == 0 {
        return None;
    }
    // u32 storage keeps the struct's cbSize field aligned.
    let mut buf = vec![0u32; (need as usize).div_ceil(4)];
    let det = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
    (*det).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
    if SetupDiGetDeviceInterfaceDetailW(set, ifd, det, need, null_mut(), null_mut()) == 0 {
        return None;
    }
    let p = std::ptr::addr_of!((*det).DevicePath) as *const u16;
    let mut len = 0;
    while *p.add(len) != 0 {
        len += 1;
    }
    let mut path = std::slice::from_raw_parts(p, len).to_vec();
    path.push(0);
    Some(path)
}

unsafe fn describe(path: Vec<u16>) -> Option<HidColl> {
    let h = CreateFileW(path.as_ptr(), 0, FILE_SHARE_READ | FILE_SHARE_WRITE, null(), OPEN_EXISTING, 0, null_mut());
    if h == INVALID_HANDLE_VALUE {
        return None;
    }
    let mut c = HidColl { iface: iface_from_path(&path), ..Default::default() };
    let mut attr: HIDD_ATTRIBUTES = std::mem::zeroed();
    attr.Size = size_of::<HIDD_ATTRIBUTES>() as u32;
    if HidD_GetAttributes(h, &mut attr) != 0 {
        c.vid = attr.VendorID;
        c.pid = attr.ProductID;
    }
    let mut pp: PHIDP_PREPARSED_DATA = 0;
    if HidD_GetPreparsedData(h, &mut pp) != 0 {
        let mut caps: HIDP_CAPS = std::mem::zeroed();
        if HidP_GetCaps(pp, &mut caps) == HIDP_STATUS_SUCCESS {
            c.page = caps.UsagePage;
            c.usage = caps.Usage;
            c.in_len = caps.InputReportByteLength;
            c.out_len = caps.OutputReportByteLength;
            c.feat_len = caps.FeatureReportByteLength;
        }
        HidD_FreePreparsedData(pp);
    }
    CloseHandle(h);
    c.path = path;
    Some(c)
}

fn lower_path(path: &[u16]) -> String {
    String::from_utf16_lossy(path).to_ascii_lowercase()
}

fn hex_after(s: &str, tag: &str) -> Option<u16> {
    let start = s.find(tag)? + tag.len();
    let hex: String = s[start..].chars().take_while(char::is_ascii_hexdigit).take(4).collect();
    u16::from_str_radix(&hex, 16).ok()
}

fn iface_from_path(path: &[u16]) -> i32 {
    hex_after(&lower_path(path), "&mi_").map_or(-1, i32::from)
}

/// Ids parsed from the path (`vid_xxxx&pid_xxxx`), used to skip ignored devices without
/// opening them. Bluetooth paths use `_vid&0002xxxx_pid&xxxx`; those return (0, 0) and are
/// filtered after opening instead.
fn ids_from_path(path: &[u16]) -> (u16, u16) {
    let s = lower_path(path);
    match (hex_after(&s, "vid_"), hex_after(&s, "pid_")) {
        (Some(v), Some(p)) => (v, p),
        _ => (0, 0),
    }
}

/// Product string of a collection, trimmed. Asks the device, so call it only for devices a
/// driver recognised, never for every collection.
pub fn product(c: &HidColl) -> String {
    unsafe {
        let h =
            CreateFileW(c.path.as_ptr(), 0, FILE_SHARE_READ | FILE_SHARE_WRITE, null(), OPEN_EXISTING, 0, null_mut());
        if h == INVALID_HANDLE_VALUE {
            return String::new();
        }
        let mut name = [0u16; 128];
        let ok = HidD_GetProductString(h, name.as_mut_ptr().cast(), (size_of_val(&name) - 2) as u32) != 0;
        CloseHandle(h);
        if !ok {
            return String::new();
        }
        let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
        String::from_utf16_lossy(&name[..end]).trim().to_string()
    }
}

/// Device type from the top-level collections a product exposes (mouse > gamepad > keyboard).
pub fn kind_from_collections(all: &[HidColl], vid: u16, pid: u16) -> Kind {
    let (mut mouse, mut pad, mut kbd) = (false, false, false);
    for c in all.iter().filter(|c| c.vid == vid && c.pid == pid && c.page == 0x01) {
        mouse |= c.usage == 0x02;
        pad |= c.usage == 0x04 || c.usage == 0x05;
        kbd |= c.usage == 0x06;
    }
    if mouse {
        Kind::Mouse
    } else if pad {
        Kind::Gamepad
    } else if kbd {
        Kind::Keyboard
    } else {
        Kind::Other
    }
}

/// Clamps to 0..=100.
pub fn clamp100(v: i32) -> u8 {
    v.clamp(0, 100) as u8
}

/// Linear map lo..hi -> 0..100, clamped.
pub fn map_to_100(v: i32, lo: i32, hi: i32) -> u8 {
    if hi <= lo {
        return clamp100(v);
    }
    clamp100((v - lo) * 100 / (hi - lo))
}

/// Piecewise-linear interpolation through `(mv, pct)` points sorted by descending voltage.
pub fn interpolate(mv: i32, table: &[(i32, i32)]) -> u8 {
    let Some(&(top_mv, top_pct)) = table.first() else { return 0 };
    if mv >= top_mv {
        return clamp100(top_pct);
    }
    for w in table.windows(2) {
        let ((hi_mv, hi_pct), (lo_mv, lo_pct)) = (w[0], w[1]);
        if mv >= lo_mv {
            let span = (hi_mv - lo_mv).max(1);
            return clamp100(lo_pct + (hi_pct - lo_pct) * (mv - lo_mv) / span);
        }
    }
    clamp100(table[table.len() - 1].1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wide;

    #[test]
    fn parses_interface_and_ids() {
        let p = wide(r"\\?\hid#vid_046d&pid_c539&mi_02&col01#8&2d0c2e8&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}");
        assert_eq!(iface_from_path(&p), 2);
        assert_eq!(ids_from_path(&p), (0x046D, 0xC539));
        let bt = wide(r"\\?\hid#{00001124-0000-1000-8000-00805f9b34fb}_vid&0002054c_pid&0ce6#9&1&0000#{...}");
        assert_eq!(iface_from_path(&bt), -1);
        assert_eq!(ids_from_path(&bt), (0, 0));
    }

    #[test]
    fn interpolates_descending_table() {
        let t = [(4200, 100), (3800, 50), (3500, 0)];
        assert_eq!(interpolate(4300, &t), 100);
        assert_eq!(interpolate(4000, &t), 75);
        assert_eq!(interpolate(3650, &t), 25);
        assert_eq!(interpolate(3000, &t), 0);
        assert_eq!(map_to_100(15, 10, 20), 50);
        assert_eq!(map_to_100(25, 10, 20), 100);
    }
}
