//! Bluetooth batteries, two ways:
//! - Bluetooth LE: the standard GATT Battery Service (0x180F / Battery Level 0x2A19). Most BLE
//!   mice and keyboards expose it.
//! - Classic Bluetooth (e.g. headsets over Hands-Free): Windows stores the level the device
//!   reports in a device property, the same one Settings > Bluetooth shows. Read with
//!   SetupAPI only, no WinRT.

use super::{simple, Driver, PollCtx};
use crate::device::{Kind, Observation};
use crate::diag;
use crate::hid;
use crate::wide;
use std::collections::HashSet;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use windows::Devices::Bluetooth::GenericAttributeProfile::{
    GattCharacteristicUuids, GattCommunicationStatus, GattDeviceService, GattServiceUuids,
};
use windows::Devices::Bluetooth::{BluetoothCacheMode, BluetoothConnectionStatus, BluetoothLEDevice};
use windows::Foundation::Collections::IVectorView;
use windows::Storage::Streams::DataReader;
use windows_sys::core::GUID;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::*;
use windows_sys::Win32::Devices::Properties::*;

/// DEVPKEY for the battery level Windows shows for Bluetooth (Hands-Free) devices.
const BATTERY_LEVEL_KEY: DEVPROPKEY =
    DEVPROPKEY { fmtid: GUID::from_u128(0x104EA319_6EE2_4701_BD47_8DDBF425BBE5), pid: 2 };
/// DEVPKEY_Device_IsConnected (on BTHENUM\DEV_ and BTHLE\DEV_ nodes).
const CONNECTED_KEY: DEVPROPKEY =
    DEVPROPKEY { fmtid: GUID::from_u128(0x83DA6326_97A6_4088_9453_A1923F573B29), pid: 15 };
/// DEVPKEY_Bluetooth_ClassOfDevice.
const CLASS_OF_DEVICE_KEY: DEVPROPKEY =
    DEVPROPKEY { fmtid: GUID::from_u128(0x2BD67D8B_8BEB_48D5_87E0_6CDA3428040A), pid: 10 };
/// DEVPKEY_Device_FriendlyName: the name shown in Settings > Bluetooth.
const FRIENDLY_NAME_KEY: DEVPROPKEY =
    DEVPROPKEY { fmtid: GUID::from_u128(0xA45C254E_DF1C_4EFD_8020_67D146A850E0), pid: 14 };

pub fn driver() -> Box<dyn Driver> {
    simple("bluetooth", poll)
}

fn poll(ctx: &mut PollCtx) {
    scan_classic(ctx);
    scan_ble(ctx);
}

/// Paired LE devices are found through SetupAPI rather than a WinRT device query: the query
/// syntax (AQS) loads Windows' search and language stack, which costs more memory than the
/// rest of the app. Devices SetupAPI reports as disconnected are skipped without touching
/// WinRT, so its Bluetooth DLLs only load when an LE device is actually connected.
fn scan_ble(ctx: &mut PollCtx) {
    let mut seen = HashSet::new();
    for node in nodes("BTHLE") {
        let Some(addr) = device_address(&node.id) else { continue };
        if node.connected == Some(false) || !seen.insert(addr) {
            continue;
        }
        if ctx.has_bt_container(&node.container) || reported_container(ctx, &node.container) {
            continue;
        }
        let key = format!("ble#{}", diag::bt_tag(addr));
        let dev = match BluetoothLEDevice::FromBluetoothAddressAsync(addr).and_then(|op| op.get()) {
            Ok(dev) => dev,
            Err(e) => {
                diag::event(&key, format!("open failed: {e}"));
                continue;
            }
        };
        if dev.ConnectionStatus().is_ok_and(|s| s == BluetoothConnectionStatus::Connected) {
            match read_ble(&dev) {
                Ok(Ok(mut o)) => {
                    if ctx.out.iter().any(|old| old.name == o.name) {
                        let _ = dev.Close();
                        continue;
                    }
                    if let Some((vid, pid)) = container_ids(ctx, &node.container) {
                        o.kind = Some(bt_kind(0, &node.category, vid, pid, ctx, o.name.as_deref().unwrap_or_default()));
                    }
                    ctx.out.push(o);
                    ctx.add_bt_container(node.container.clone());
                }
                Ok(Err(reason)) => diag::event(&key, reason),
                Err(e) => diag::event(&key, format!("error: {e}")),
            }
        }
        let _ = dev.Close();
    }
}

/// Device nodes (not profile/service nodes):
/// "BTHLE\DEV_F4EE25FBBA46\..." or "BTHENUM\DEV_F5CFD77BCB0B\..." -> the address
fn device_address(id: &str) -> Option<u64> {
    let mac = id.split('\\').nth(1)?.strip_prefix("DEV_")?;
    (mac.len() == 12).then(|| u64::from_str_radix(mac, 16).ok()).flatten()
}

/// The device, or why it isn't shown (for diagnostics).
fn read_ble(dev: &BluetoothLEDevice) -> windows::core::Result<Result<Observation, String>> {
    // Appearance: skip phones (0x0040..) and computers (0x0080..); classify HID and audio.
    let appearance = dev.Appearance()?.RawValue()?;
    let skip = |why: &str| Ok(Err(format!("appearance 0x{appearance:04X}: {why}")));
    let kind = match appearance >> 6 {
        0x01 | 0x02 => return skip("phone or computer, skipped"),
        0x0F => match appearance & 0x3F {
            1 => Some(Kind::Keyboard),
            2 => Some(Kind::Mouse),
            3 | 4 => Some(Kind::Gamepad),
            _ => None,
        },
        // Wearable audio device: 0x0941 earbud, 0x0942 headset, 0x0943 headphones.
        0x25 => match appearance & 0x3F {
            1 => Some(Kind::Earbuds),
            2 | 3 => Some(Kind::Headset),
            _ => None, // generic: decide by name
        },
        _ => None,
    };

    let result = dev
        .GetGattServicesForUuidWithCacheModeAsync(GattServiceUuids::Battery()?, BluetoothCacheMode::Cached)?
        .get()?;
    let status = result.Status()?;
    if status != GattCommunicationStatus::Success {
        return skip(&format!("Battery Service lookup failed ({})", gatt_status(status)));
    }
    let services = result.Services()?;
    let level = read_level(&services);
    for i in 0..services.Size()? {
        let _ = services.GetAt(i)?.Close();
    }
    let percent = match level? {
        Ok(percent) => percent,
        Err(why) => return skip(&why),
    };

    let name = dev.Name()?.to_string();
    if name.to_ascii_lowercase().contains("xbox") {
        return skip("Xbox controller, reported by the Xbox source"); // more precise there
    }
    // Not the name: users rename Bluetooth devices.
    diag::event(
        &format!("ble#{}", diag::bt_tag(dev.BluetoothAddress()?)),
        format!("appearance 0x{appearance:04X}: battery via GATT"),
    );
    Ok(Ok(Observation::battery(
        format!("ble:{:012x}", dev.BluetoothAddress()?),
        name.clone(),
        kind.unwrap_or_else(|| Kind::from_name(&name)),
        percent.min(100),
        false,
    )
    .via("Bluetooth")))
}

fn read_level(services: &IVectorView<GattDeviceService>) -> windows::core::Result<Result<u8, String>> {
    if services.Size()? == 0 {
        return Ok(Err("no Battery Service (0x180F)".into()));
    }
    let chars = services
        .GetAt(0)?
        .GetCharacteristicsForUuidWithCacheModeAsync(
            GattCharacteristicUuids::BatteryLevel()?,
            BluetoothCacheMode::Cached,
        )?
        .get()?;
    if chars.Status()? != GattCommunicationStatus::Success {
        return Ok(Err(format!("Battery Level lookup failed ({})", gatt_status(chars.Status()?))));
    }
    if chars.Characteristics()?.Size()? == 0 {
        return Ok(Err("no Battery Level characteristic (0x2A19)".into()));
    }
    let value = chars.Characteristics()?.GetAt(0)?.ReadValueWithCacheModeAsync(BluetoothCacheMode::Uncached)?.get()?;
    if value.Status()? != GattCommunicationStatus::Success {
        return Ok(Err(format!("reading Battery Level failed ({})", gatt_status(value.Status()?))));
    }
    Ok(Ok(DataReader::FromBuffer(&value.Value()?)?.ReadByte()?))
}

fn gatt_status(s: GattCommunicationStatus) -> &'static str {
    match s.0 {
        0 => "success",
        1 => "unreachable",
        2 => "protocol error",
        3 => "access denied",
        _ => "unknown status",
    }
}

/// Entirely SetupAPI: the battery level sits on the device's profile nodes
/// ("BTHENUM\{uuid}_..."), its name, class and connection state on the device node
/// ("BTHENUM\DEV_<address>").
fn scan_classic(ctx: &mut PollCtx) {
    let all: Vec<_> = ["BTHENUM", "BTHLE", "BTHLEDEVICE"].into_iter().flat_map(nodes).collect();
    let mut seen = HashSet::new();
    for node in &all {
        let Some(percent) = node.level.filter(|p| *p <= 100) else { continue };
        let cid = &node.container;
        if seen.contains(cid) || reported_container(ctx, cid) {
            continue;
        }
        let main = all.iter().find(|n| n.container == *cid && device_address(&n.id).is_some());
        let connected = main.and_then(|n| n.connected).or(node.connected).unwrap_or(true);
        if !connected {
            continue;
        }
        // Prefer a live service node when Windows retains battery properties on several profiles.
        let source = all
            .iter()
            .find(|n| n.container == *cid && n.level.is_some_and(|p| p <= 100) && n.connected == Some(true))
            .unwrap_or(node);
        let name = main
            .and_then(|n| n.name.as_deref())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| source.name.as_deref().filter(|n| !n.is_empty()).unwrap_or("Bluetooth device"));
        let name = strip_service_suffix(name);
        if ctx.out.iter().any(|o| o.name.as_deref() == Some(name)) {
            continue;
        }
        let (vid, pid) = container_ids(ctx, cid).unwrap_or((0, 0));
        let kind = bt_kind(
            main.and_then(|n| n.class).unwrap_or(0),
            main.map(|n| n.category.as_str()).unwrap_or(""),
            vid,
            pid,
            ctx,
            name,
        );
        ctx.out.push(
            Observation::battery(format!("bt:{cid}"), name, kind, source.level.unwrap_or(percent), false)
                .via("Bluetooth"),
        );
        ctx.add_bt_container(cid.clone());
        seen.insert(cid.clone());
    }
}

fn bt_kind(cod: u32, category: &str, vid: u16, pid: u16, ctx: &PollCtx, name: &str) -> Kind {
    let minor = (cod >> 2) & 63;
    let kind = match (cod >> 8) & 31 {
        4 => Kind::Headset,
        5 if minor & 15 == 1 || minor & 15 == 2 => Kind::Gamepad,
        5 if minor >> 4 == 1 => Kind::Keyboard,
        5 if minor >> 4 == 2 => Kind::Mouse,
        _ => Kind::Other,
    };
    if kind != Kind::Other {
        return kind;
    }
    let kind = match category {
        "Input.Keyboard" => Kind::Keyboard,
        "Input.Mouse" => Kind::Mouse,
        "Input.Gaming" => Kind::Gamepad,
        "Audio.Headphones" => Kind::Headset,
        c if c.starts_with("Communication.Headset") => Kind::Headset,
        _ => Kind::Other,
    };
    if kind != Kind::Other {
        return kind;
    }
    if (vid, pid) == (0x3151, 0x4013) {
        return Kind::Keyboard;
    }
    let kind = hid::kind_from_collections(ctx.colls, vid, pid);
    if kind == Kind::Other {
        Kind::from_name(name)
    } else {
        kind
    }
}

fn strip_service_suffix(name: &str) -> &str {
    for suffix in [" Hands-Free AG", " Hands-Free", " Avrcp Transport", " Stereo", " Audio Gateway"] {
        if let Some(n) = name.strip_suffix(suffix).filter(|n| !n.is_empty()) {
            return n;
        }
    }
    name
}

fn container_ids(ctx: &PollCtx, cid: &str) -> Option<(u16, u16)> {
    ctx.colls.iter().find(|c| container_of(&c.path) == cid).map(|c| (c.vid, c.pid))
}

fn reported_container(ctx: &PollCtx, cid: &str) -> bool {
    ctx.colls.iter().any(|c| ctx.is_claimed(c.vid, c.pid) && container_of(&c.path) == cid)
}

pub(super) fn container_of(path: &[u16]) -> String {
    unsafe {
        let set = SetupDiCreateDeviceInfoList(null(), null_mut());
        if set as isize == -1 {
            return String::new();
        }
        let mut iface: SP_DEVICE_INTERFACE_DATA = zeroed();
        iface.cbSize = size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
        let mut data: SP_DEVINFO_DATA = zeroed();
        data.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;
        let mut result = String::new();
        if SetupDiOpenDeviceInterfaceW(set, path.as_ptr(), 0, &mut iface) != 0 {
            SetupDiGetDeviceInterfaceDetailW(set, &iface, null_mut(), 0, null_mut(), &mut data);
            if data.DevInst != 0 {
                result = property(set, &data, &DEVPKEY_Device_ContainerId)
                    .and_then(|b| container_string(&b))
                    .unwrap_or_default();
            }
        }
        SetupDiDestroyDeviceInfoList(set);
        result
    }
}

fn container_string(b: &[u8]) -> Option<String> {
    let a = u32::from_le_bytes(b.get(..4)?.try_into().ok()?);
    let c = u16::from_le_bytes(b.get(4..6)?.try_into().ok()?);
    let d = u16::from_le_bytes(b.get(6..8)?.try_into().ok()?);
    let tail = b.get(8..16)?;
    Some(format!(
        "{{{a:08X}-{c:04X}-{d:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        tail[0], tail[1], tail[2], tail[3], tail[4], tail[5], tail[6], tail[7]
    ))
}

/// A present device node and the Bluetooth properties it carries.
struct Node {
    id: String,
    level: Option<u8>,
    connected: Option<bool>,
    name: Option<String>,
    class: Option<u32>,
    container: String,
    category: String,
}

/// Every present device node of `enumerator`.
fn nodes(enumerator: &str) -> Vec<Node> {
    let mut out = Vec::new();
    unsafe {
        let enumerator = wide(enumerator);
        let set = SetupDiGetClassDevsW(null(), enumerator.as_ptr(), null_mut(), DIGCF_ALLCLASSES | DIGCF_PRESENT);
        if set as isize == -1 {
            return out;
        }
        let mut i = 0;
        loop {
            let mut data: SP_DEVINFO_DATA = zeroed();
            data.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;
            if SetupDiEnumDeviceInfo(set, i, &mut data) == 0 {
                break;
            }
            i += 1;
            let mut id = [0u16; 512];
            let mut needed = 0u32;
            if SetupDiGetDeviceInstanceIdW(set, &data, id.as_mut_ptr(), id.len() as u32, &mut needed) == 0 {
                continue;
            }
            let prop = |key: &DEVPROPKEY, ty| property_typed(set, &data, key, Some(ty));
            let level = prop(&BATTERY_LEVEL_KEY, DEVPROP_TYPE_BYTE).and_then(|b| b.first().copied());
            let connected = prop(&CONNECTED_KEY, DEVPROP_TYPE_BOOLEAN).and_then(|b| b.first().map(|v| *v != 0));
            let class = prop(&CLASS_OF_DEVICE_KEY, DEVPROP_TYPE_UINT32)
                .and_then(|b| Some(u32::from_le_bytes(b.get(..4)?.try_into().ok()?)));
            let container = prop(&DEVPKEY_Device_ContainerId, DEVPROP_TYPE_GUID)
                .and_then(|b| container_string(&b))
                .unwrap_or_else(|| String::from_utf16_lossy(&id).trim_end_matches('\0').to_string());
            let category = property_typed(set, &data, &DEVPKEY_DeviceContainer_Category, None)
                .map(|b| decode_string(&b))
                .unwrap_or_default();
            let name = prop(&FRIENDLY_NAME_KEY, DEVPROP_TYPE_STRING)
                .or_else(|| prop(&DEVPKEY_Device_DeviceDesc, DEVPROP_TYPE_STRING))
                .map(|b| decode_string(&b));
            let len = id.iter().position(|c| *c == 0).unwrap_or(id.len());
            let mut st = 0;
            let mut problem = 0;
            if CM_Get_DevNode_Status(&mut st, &mut problem, data.DevInst, 0) == 0 && st & DN_HAS_PROBLEM != 0 {
                continue;
            }
            out.push(Node {
                id: String::from_utf16_lossy(&id[..len]),
                level,
                connected,
                name,
                class,
                container,
                category,
            });
        }
        SetupDiDestroyDeviceInfoList(set);
    }
    out
}

/// Raw bytes of a device property, if the node has it.
unsafe fn property(set: HDEVINFO, data: &SP_DEVINFO_DATA, key: &DEVPROPKEY) -> Option<Vec<u8>> {
    property_typed(set, data, key, Some(DEVPROP_TYPE_GUID))
}

unsafe fn property_typed(
    set: HDEVINFO,
    data: &SP_DEVINFO_DATA,
    key: &DEVPROPKEY,
    expected: Option<u32>,
) -> Option<Vec<u8>> {
    let mut buf = [0u8; 512];
    let (mut ptype, mut size) = (0u32, 0u32);
    let ok = SetupDiGetDevicePropertyW(set, data, key, &mut ptype, buf.as_mut_ptr(), buf.len() as u32, &mut size, 0);
    (ok != 0 && expected.map_or(ptype == DEVPROP_TYPE_STRING || ptype == DEVPROP_TYPE_STRING_LIST, |t| t == ptype))
        .then(|| buf[..(size as usize).min(buf.len())].to_vec())
}

fn decode_string(b: &[u8]) -> String {
    String::from_utf16_lossy(
        &b.as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|u| *u != 0)
            .collect::<Vec<_>>(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_addresses() {
        assert_eq!(device_address(r"BTHLE\DEV_F4EE25FBBA46\7&16AA5920&0&F4EE25FBBA46"), Some(0xF4EE25FBBA46));
        assert_eq!(
            device_address(r"BTHENUM\DEV_F5CFD77BCB0B\7&3057B904&0&BLUETOOTHDEVICE_F5CFD77BCB0B"),
            Some(0xF5CFD77BCB0B)
        );
        assert_eq!(device_address(r"BTHLE\GENERIC_ATTRIBUTE\7&1"), None);
        // Profile nodes carry the battery level but aren't device nodes.
        assert_eq!(
            device_address(
                r"BTHENUM\{0000111E-0000-1000-8000-00805F9B34FB}_VID&0001004C_PID&2014\7&3057B904&0&F5CFD77BCB0B_C00000000"
            ),
            None
        );
    }
}

#[cfg(test)]
mod property_tests {
    use super::*;
    #[test]
    fn containers_suffixes_and_le_kind() {
        assert_eq!(container_string(&[]), None);
        assert_eq!(container_string(&[0; 16]).as_deref(), Some("{00000000-0000-0000-0000-000000000000}"));
        assert_eq!(strip_service_suffix("Headphones Hands-Free AG"), "Headphones");
        let ctx = PollCtx::new(&[]);
        assert_eq!(bt_kind(0, "", 0x3151, 0x4013, &ctx, "K86"), Kind::Keyboard);
        assert_eq!(bt_kind(0, "Input.Mouse", 0, 0, &ctx, ""), Kind::Mouse);
        assert_eq!(bt_kind(0x0504, "", 0, 0, &ctx, ""), Kind::Gamepad);
    }
}
