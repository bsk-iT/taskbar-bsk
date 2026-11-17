//! Standard HID battery usages; never query digitizers or power supplies.
use super::{simple, Driver, PollCtx};
use crate::device::{Kind, Observation};
use crate::hid::{self, HidColl};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Devices::HumanInterfaceDevice::*;
use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};

pub fn driver() -> Box<dyn Driver> {
    simple("generic_hid", poll)
}

fn percent(vid: u16, value: u32, lo: i32, hi: i32) -> Option<u8> {
    if vid == 0x05ac && value <= 100 {
        return Some(value as u8);
    }
    if hi > lo {
        return Some(
            ((i64::from(value as i32) - i64::from(lo)) * 100 / (i64::from(hi) - i64::from(lo))).clamp(0, 100) as u8
        );
    }
    (value <= 100).then_some(value as u8)
}

fn poll(ctx: &mut PollCtx) {
    for c in ctx.colls {
        if ctx.is_claimed(c.vid, c.pid)
            || [0x84, 0x85].contains(&c.page)
            || ctx.colls.iter().any(|d| d.vid == c.vid && d.pid == c.pid && d.page == 0x0d)
        {
            continue;
        }
        if ctx.has_bt_container(&super::bluetooth::container_of(&c.path)) {
            continue;
        }
        let Some(pct) = read_battery(c) else { continue };
        let name = hid::product(c);
        let name = if name.is_empty() { "Wireless device".into() } else { name };
        let kind = hid::kind_from_collections(ctx.colls, c.vid, c.pid);
        let kind = if kind == Kind::Other { Kind::from_name(&name) } else { kind };
        ctx.add(
            Observation::battery(format!("hid:{:04x}:{:04x}", c.vid, c.pid), name, kind, pct, false).via("HID"),
            c.vid,
            c.pid,
        );
    }
}

fn read_battery(c: &HidColl) -> Option<u8> {
    unsafe {
        let h =
            CreateFileW(c.path.as_ptr(), 0, FILE_SHARE_READ | FILE_SHARE_WRITE, null(), OPEN_EXISTING, 0, null_mut());
        if h == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut pp = 0;
        if HidD_GetPreparsedData(h, &mut pp) == 0 {
            CloseHandle(h);
            return None;
        }
        let mut result = None;
        let mut caps: HIDP_CAPS = std::mem::zeroed();
        if HidP_GetCaps(pp, &mut caps) == HIDP_STATUS_SUCCESS {
            for (ty, count, len) in [
                (HidP_Feature, caps.NumberFeatureValueCaps, c.feat_len),
                (HidP_Input, caps.NumberInputValueCaps, c.in_len),
            ] {
                if count == 0 || len == 0 {
                    continue;
                }
                let mut values = vec![std::mem::zeroed::<HIDP_VALUE_CAPS>(); usize::from(count)];
                let mut n = count;
                if HidP_GetValueCaps(ty, values.as_mut_ptr(), &mut n, pp) != HIDP_STATUS_SUCCESS {
                    continue;
                }
                for v in values.iter().take(usize::from(n)) {
                    let usage = if v.IsRange != 0 { v.Anonymous.Range.UsageMin } else { v.Anonymous.NotRange.Usage };
                    if !matches!((v.UsagePage, usage), (6, 0x20) | (0x85, 0x65)) {
                        continue;
                    }
                    let mut report = vec![0; usize::from(len)];
                    if let Some(id) = report.first_mut() {
                        *id = v.ReportID;
                    }
                    let got = if ty == HidP_Feature {
                        HidD_GetFeature(h, report.as_mut_ptr().cast(), u32::from(len))
                    } else {
                        HidD_GetInputReport(h, report.as_mut_ptr().cast(), u32::from(len))
                    };
                    if got == 0 {
                        continue;
                    }
                    let mut value = 0;
                    if HidP_GetUsageValue(
                        ty,
                        v.UsagePage,
                        v.LinkCollection,
                        usage,
                        &mut value,
                        pp,
                        report.as_ptr(),
                        u32::from(len),
                    ) == HIDP_STATUS_SUCCESS
                    {
                        result = percent(c.vid, value, v.LogicalMin, v.LogicalMax);
                        if result.is_some() {
                            break;
                        }
                    }
                }
                if result.is_some() {
                    break;
                }
            }
        }
        HidD_FreePreparsedData(pp);
        CloseHandle(h);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn logical_ranges_and_apple_quirk() {
        assert_eq!(percent(0x05ac, 75, 0, 255), Some(75));
        assert_eq!(percent(1, 75, 0, 255), Some(29));
        assert_eq!(percent(1, 60, 10, 110), Some(50));
        assert_eq!(percent(1, 101, 0, 0), None);
        assert_eq!(percent(1, 100, 0, 0), Some(100));
    }
}
