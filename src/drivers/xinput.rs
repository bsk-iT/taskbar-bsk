//! Precise Windows.Gaming.Input capacity, then coarse XInput controller/headset fallback.
use super::{simple_every, Driver, PollCtx, FAST};
use crate::device::{Kind, Observation};
use crate::{diag, wide};
use std::sync::LazyLock;
use windows::Gaming::Input::RawGameController;
use windows::System::Power::BatteryStatus;
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32};

pub fn driver() -> Box<dyn Driver> {
    simple_every("xinput", FAST, poll)
}

#[repr(C)]
#[derive(Default, Copy, Clone, PartialEq)]
struct Battery {
    kind: u8,
    level: u8,
}
type GetBattery = unsafe extern "system" fn(u32, u8, *mut Battery) -> u32;
// XINPUT_STATE is a DWORD packet number followed by a 12-byte XINPUT_GAMEPAD.
type GetState = unsafe extern "system" fn(u32, *mut [u32; 4]) -> u32;
struct Api {
    battery: GetBattery,
    state: GetState,
}

fn api() -> Option<&'static Api> {
    static API: LazyLock<Option<Api>> = LazyLock::new(|| unsafe {
        for name in ["xinput1_4.dll", "xinput9_1_0.dll"] {
            let m = LoadLibraryExW(wide(name).as_ptr(), std::ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32);
            if m.is_null() {
                continue;
            }
            let battery = GetProcAddress(m, c"XInputGetBatteryInformation".as_ptr().cast());
            let state = GetProcAddress(m, c"XInputGetState".as_ptr().cast());
            if let (Some(b), Some(s)) = (battery, state) {
                return Some(Api {
                    battery: std::mem::transmute::<unsafe extern "system" fn() -> isize, GetBattery>(b),
                    state: std::mem::transmute::<unsafe extern "system" fn() -> isize, GetState>(s),
                });
            }
        }
        None
    });
    API.as_ref()
}

fn poll_wgi(ctx: &mut PollCtx) -> windows::core::Result<(usize, usize)> {
    let pads = RawGameController::RawGameControllers()?;
    let (mut precise, mut third_party) = (0, 0);
    for i in 0..pads.Size()? {
        let Ok(raw) = pads.GetAt(i) else { continue };
        let (vid, pid) = (raw.HardwareVendorId().unwrap_or(0), raw.HardwareProductId().unwrap_or(0));
        if vid != 0x045e && vid != 0x3537 {
            continue;
        }
        let Ok(report) = raw.TryGetBatteryReport() else { continue };
        let Ok(status) = report.Status() else { continue };
        if status == BatteryStatus::NotPresent {
            continue;
        }
        let (Ok(remaining), Ok(full)) =
            (report.RemainingCapacityInMilliwattHours(), report.FullChargeCapacityInMilliwattHours())
        else {
            continue;
        };
        let (Ok(remaining), Ok(full)) = (remaining.Value(), full.Value()) else { continue };
        if full <= 0 {
            continue;
        }
        let percent = (i64::from(remaining) * 100 / i64::from(full)).clamp(0, 100) as u8;
        let name = if vid == 0x3537 {
            "GameSir controller".into()
        } else {
            raw.DisplayName()
                .map(|s| s.to_string())
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "Xbox controller".into())
        };
        let id = raw.NonRoamableId().map(|s| s.to_string()).unwrap_or_else(|_| i.to_string());
        ctx.add(
            Observation::battery(
                format!("xinput:{vid:04x}:{pid:04x}:{id}"),
                name,
                Kind::Gamepad,
                percent,
                status == BatteryStatus::Charging,
            )
            .via("Xbox Wireless / Bluetooth"),
            vid,
            pid,
        );
        precise += 1;
        third_party += usize::from(vid != 0x045e);
    }
    Ok((precise, third_party))
}

fn query(api: &Api, user: u32, kind: u8) -> Option<Battery> {
    let mut b = Battery::default();
    (unsafe { (api.battery)(user, kind, &mut b) } == 0 && b.kind != 0 && b.kind != 1).then_some(b)
}

fn coarse(level: u8) -> (u8, &'static str) {
    match level {
        0 => (5, "Empty"),
        1 => (25, "Low"),
        2 => (60, "Medium"),
        _ => (100, "Full"),
    }
}

fn poll(ctx: &mut PollCtx) {
    let bt_xbox = ctx.out.iter().any(|o| o.via == "Bluetooth" && o.name.as_deref().is_some_and(|n| n.contains("Xbox")));
    let (precise, third_party) = if bt_xbox {
        (0, 0)
    } else {
        poll_wgi(ctx).unwrap_or_else(|e| {
            diag::event("xinput", format!("error: {e}"));
            (0, 0)
        })
    };
    let Some(api) = api() else { return };
    for user in 0..4 {
        let mut state = [0u32; 4];
        if unsafe { (api.state)(user, &mut state) } != 0 {
            continue;
        }
        let pad = query(api, user, 0);
        for kind in [0, 1] {
            if kind == 0 && (bt_xbox || precise > 0) {
                continue;
            }
            if kind == 1 && precise > 0 && third_party == precise {
                continue;
            }
            let Some(b) = (if kind == 0 { pad } else { query(api, user, 1) }) else { continue };
            if kind == 1 && (b.kind == 0xff || pad == Some(b)) {
                continue;
            }
            let (pct, text) = coarse(b.level);
            let name = if kind == 0 { format!("Xbox controller {}", user + 1) } else { "Xbox headset".into() };
            ctx.out.push(
                Observation::battery(
                    format!("xinput:045e:0000:{user}:{kind}"),
                    name,
                    if kind == 0 { Kind::Gamepad } else { Kind::Headset },
                    pct,
                    false,
                )
                .via("Xbox Wireless")
                .level_text(text),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn battery_levels() {
        assert_eq!(coarse(0), (5, "Empty"));
        assert_eq!(coarse(1), (25, "Low"));
        assert_eq!(coarse(2), (60, "Medium"));
        assert_eq!(coarse(3), (100, "Full"));
    }
}
