//! Registry-backed settings: start-with-Windows and the taskbar theme (plus the Windows
//! version, for diagnostics).

use crate::wide;
use std::ffi::c_void;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::*;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const APP_KEY: &str = r"Software\taskbar-bsk";
const RUN_VALUE: &str = "TaskbarBsk";

pub fn light_theme() -> bool {
    read_dword(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize", "SystemUsesLightTheme") == Some(1)
}

pub fn autostart_enabled() -> bool {
    let (k, v) = (wide(RUN_KEY), wide(RUN_VALUE));
    let mut size = 0u32;
    unsafe {
        RegGetValueW(HKEY_CURRENT_USER, k.as_ptr(), v.as_ptr(), RRF_RT_REG_SZ, null_mut(), null_mut(), &mut size)
            == ERROR_SUCCESS
    }
}

pub fn set_autostart(on: bool) {
    let (k, v) = (wide(RUN_KEY), wide(RUN_VALUE));
    unsafe {
        if on {
            let Ok(exe) = std::env::current_exe() else { return };
            let data = wide(&format!("\"{}\"", exe.display()));
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                k.as_ptr(),
                v.as_ptr(),
                REG_SZ,
                data.as_ptr() as *const c_void,
                (data.len() * 2) as u32,
            );
        } else {
            RegDeleteKeyValueW(HKEY_CURRENT_USER, k.as_ptr(), v.as_ptr());
        }
    }
    write_dword(APP_KEY, "AutostartOptOut", u32::from(!on));
}

/// Release builds register themselves to start with Windows (and refresh the path if the
/// exe moved), unless the user turned it off from the menu.
pub fn ensure_autostart() {
    if cfg!(debug_assertions) || read_dword(APP_KEY, "AutostartOptOut") == Some(1) {
        return;
    }
    set_autostart(true);
}

pub fn alerts_enabled() -> bool {
    read_dword(APP_KEY, "LowBatteryAlerts") != Some(0)
}

pub fn set_alerts(on: bool) {
    write_dword(APP_KEY, "LowBatteryAlerts", u32::from(on));
}

const NOTIFICATION_ICON: &[u8] = include_bytes!("../assets/icon-64.png");

/// Registers the AppUserModelID so Windows accepts our toast notifications, with our icon
/// (written to %LOCALAPPDATA%\taskbar-bsk\icon.png, since Windows needs a file).
pub fn register_app_id() {
    let key = format!(r"Software\Classes\AppUserModelId\{}", crate::notify::APP_ID);
    write_string(&key, "DisplayName", "taskbar-bsk");
    if let Some(dir) = std::env::var_os("LOCALAPPDATA").map(|d| std::path::PathBuf::from(d).join("taskbar-bsk")) {
        let icon = dir.join("icon.png");
        if std::fs::create_dir_all(&dir).is_ok() && std::fs::write(&icon, NOTIFICATION_ICON).is_ok() {
            write_string(&key, "IconUri", &icon.to_string_lossy());
        }
    }
}

fn write_string(key: &str, name: &str, value: &str) {
    let (k, n, data) = (wide(key), wide(name), wide(value));
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            k.as_ptr(),
            n.as_ptr(),
            REG_SZ,
            data.as_ptr() as *const c_void,
            (data.len() * 2) as u32,
        );
    }
}

/// Device ids the user unchecked in the menu.
pub fn hidden_devices() -> Vec<String> {
    read_string(APP_KEY, "Hidden").map(|s| s.lines().map(str::to_string).collect()).unwrap_or_default()
}

pub fn toggle_hidden(id: &str) {
    let mut hidden = hidden_devices();
    match hidden.iter().position(|h| h == id) {
        Some(i) => {
            hidden.remove(i);
        }
        None => hidden.push(id.to_string()),
    }
    let (k, n, data) = (wide(APP_KEY), wide("Hidden"), wide(&hidden.join("\n")));
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            k.as_ptr(),
            n.as_ptr(),
            REG_SZ,
            data.as_ptr() as *const c_void,
            (data.len() * 2) as u32,
        );
    }
}

pub fn widget_mode() -> bool {
    read_dword(APP_KEY, "TaskbarWidget") != Some(0)
}

pub fn set_widget_mode(on: bool) {
    write_dword(APP_KEY, "TaskbarWidget", u32::from(on));
}

pub fn hide_last_known() -> bool {
    read_dword(APP_KEY, "HideLastKnownDevices") == Some(1)
}

pub fn set_hide_last_known(on: bool) {
    write_dword(APP_KEY, "HideLastKnownDevices", u32::from(on));
}

pub fn widget_background() -> bool {
    read_dword(APP_KEY, "WidgetBackground") == Some(1)
}

pub fn set_widget_background(on: bool) {
    write_dword(APP_KEY, "WidgetBackground", u32::from(on));
}

pub fn device_order() -> Vec<String> {
    let (key, name) = (wide(APP_KEY), wide("DeviceOrder"));
    let mut bytes = 0;
    unsafe {
        if RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_MULTI_SZ,
            null_mut(),
            null_mut(),
            &mut bytes,
        ) != ERROR_SUCCESS
        {
            return Vec::new();
        }
        let mut data = vec![0u16; (bytes as usize).div_ceil(2) + 1];
        if RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_MULTI_SZ,
            null_mut(),
            data.as_mut_ptr().cast(),
            &mut bytes,
        ) != ERROR_SUCCESS
        {
            return Vec::new();
        }
        data.split(|c| *c == 0).take_while(|s| !s.is_empty()).map(String::from_utf16_lossy).collect()
    }
}

pub fn set_device_order(order: &[String]) {
    let (key, name) = (wide(APP_KEY), wide("DeviceOrder"));
    let mut data: Vec<u16> = order.iter().take(64).flat_map(|s| s.encode_utf16().chain(std::iter::once(0))).collect();
    data.push(0);
    if data.len() == 1 {
        data.push(0);
    }
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            REG_MULTI_SZ,
            data.as_ptr().cast(),
            (data.len() * 2) as u32,
        );
    }
}

/// `IgnoreDevices` (REG_SZ): "VID:PID" pairs in hex separated by spaces, commas or semicolons,
/// e.g. "28BD:0947, 046D:C547". No driver ever opens those devices.
pub fn ignored_devices() -> Vec<(u16, u16)> {
    read_string(APP_KEY, "IgnoreDevices").map(|s| parse_vid_pid_list(&s)).unwrap_or_default()
}

fn parse_vid_pid_list(s: &str) -> Vec<(u16, u16)> {
    s.split([' ', ',', ';', '\t', '\n'])
        .filter_map(|pair| {
            let (v, p) = pair.trim().split_once(':')?;
            Some((u16::from_str_radix(v, 16).ok()?, u16::from_str_radix(p, 16).ok()?))
        })
        .collect()
}

/// E.g. "Windows 11 24H2 build 26100.2033", for diagnostics.
pub fn windows_version() -> String {
    const KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let build: u32 = read_string_in(HKEY_LOCAL_MACHINE, KEY, "CurrentBuild").and_then(|b| b.parse().ok()).unwrap_or(0);
    let ubr = read_dword_in(HKEY_LOCAL_MACHINE, KEY, "UBR").unwrap_or(0);
    let release = read_string_in(HKEY_LOCAL_MACHINE, KEY, "DisplayVersion").unwrap_or_default();
    // ProductName still says "Windows 10" on Windows 11; the build tells them apart.
    let name = if build >= 22000 { "Windows 11" } else { "Windows 10" };
    format!("{name} {release} build {build}.{ubr}")
}

fn read_string(key: &str, name: &str) -> Option<String> {
    read_string_in(HKEY_CURRENT_USER, key, name)
}

fn read_string_in(root: HKEY, key: &str, name: &str) -> Option<String> {
    let (k, n) = (wide(key), wide(name));
    let mut size = 0u32;
    unsafe {
        if RegGetValueW(root, k.as_ptr(), n.as_ptr(), RRF_RT_REG_SZ, null_mut(), null_mut(), &mut size) != ERROR_SUCCESS
        {
            return None;
        }
        let mut buf = vec![0u16; (size as usize).div_ceil(2)];
        if RegGetValueW(
            root,
            k.as_ptr(),
            n.as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            buf.as_mut_ptr() as *mut c_void,
            &mut size,
        ) != ERROR_SUCCESS
        {
            return None;
        }
        let len = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..len]))
    }
}

fn read_dword(key: &str, name: &str) -> Option<u32> {
    read_dword_in(HKEY_CURRENT_USER, key, name)
}

fn read_dword_in(root: HKEY, key: &str, name: &str) -> Option<u32> {
    let (k, n) = (wide(key), wide(name));
    let (mut value, mut size) = (0u32, 4u32);
    let r = unsafe {
        RegGetValueW(
            root,
            k.as_ptr(),
            n.as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            &mut value as *mut u32 as *mut c_void,
            &mut size,
        )
    };
    (r == ERROR_SUCCESS).then_some(value)
}

fn write_dword(key: &str, name: &str, value: u32) {
    let (k, n) = (wide(key), wide(name));
    unsafe {
        RegSetKeyValueW(HKEY_CURRENT_USER, k.as_ptr(), n.as_ptr(), REG_DWORD, &value as *const u32 as *const c_void, 4);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ignore_list() {
        assert_eq!(
            parse_vid_pid_list("28BD:0947, 046d:c547;bad 1:2"),
            vec![(0x28BD, 0x0947), (0x046D, 0xC547), (1, 2)]
        );
        assert!(parse_vid_pid_list("").is_empty());
        assert!(parse_vid_pid_list("zzzz:0001").is_empty());
    }
}
