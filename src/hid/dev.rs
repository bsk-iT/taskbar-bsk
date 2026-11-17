//! An open HID collection. Closed on drop.

use super::HidColl;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Devices::HumanInterfaceDevice::*;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_IO_PENDING, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
    WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows_sys::Win32::System::IO::{CancelIo, GetOverlappedResult, OVERLAPPED};

pub struct HidDev {
    h: HANDLE,
    overlapped: bool,
    in_len: usize,
    out_len: usize,
    feat_len: usize,
}

// The handle is owned and only used through &self/&mut self; Win32 HID handles may be used
// from any thread.
unsafe impl Send for HidDev {}

impl HidDev {
    /// Read/write, overlapped. Fails on mice and keyboards, which Windows opens exclusively.
    pub fn open(c: &HidColl) -> Option<Self> {
        Self::open_with(c, GENERIC_READ | GENERIC_WRITE, true)
    }

    /// Read only, overlapped: for push-only collections another app may hold for writing.
    pub fn open_read_only(c: &HidColl) -> Option<Self> {
        Self::open_with(c, GENERIC_READ, true)
    }

    /// Zero access: enough for feature and input-report requests over the control pipe, and
    /// works on mice/keyboards. `read`/`write` are unavailable on such a handle.
    pub fn open_feature_only(c: &HidColl) -> Option<Self> {
        Self::open_with(c, 0, false)
    }

    fn open_with(c: &HidColl, access: u32, overlapped: bool) -> Option<Self> {
        let flags = if overlapped { FILE_FLAG_OVERLAPPED } else { 0 };
        let h = unsafe {
            CreateFileW(
                c.path.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null(),
                OPEN_EXISTING,
                flags,
                null_mut(),
            )
        };
        (h != INVALID_HANDLE_VALUE).then_some(Self {
            h,
            overlapped,
            in_len: c.in_len.into(),
            out_len: c.out_len.into(),
            feat_len: c.feat_len.into(),
        })
    }

    /// Sends an output report over the interrupt pipe, padded to the report length.
    pub fn write(&self, data: &[u8], timeout_ms: u32) -> bool {
        if !self.overlapped || self.out_len == 0 || data.is_empty() {
            return false;
        }
        let buf = padded(data, self.out_len);
        let mut done = 0u32;
        unsafe {
            let Some(ov) = Overlapped::new() else { return false };
            let started = WriteFile(self.h, buf.as_ptr(), buf.len() as u32, null_mut(), ov.ptr()) != 0
                || GetLastError() == ERROR_IO_PENDING;
            started && self.finish(&ov, timeout_ms, &mut done)
        }
    }

    /// Reads one input report (hidapi semantics, see module docs). `None` on timeout/error.
    pub fn read(&self, timeout_ms: u32) -> Option<Vec<u8>> {
        if !self.overlapped || self.in_len == 0 {
            return None;
        }
        let mut tmp = vec![0u8; self.in_len];
        let mut got = 0u32;
        unsafe {
            let ov = Overlapped::new()?;
            let started = ReadFile(self.h, tmp.as_mut_ptr(), tmp.len() as u32, null_mut(), ov.ptr()) != 0
                || GetLastError() == ERROR_IO_PENDING;
            if !started || !self.finish(&ov, timeout_ms, &mut got) {
                return None;
            }
        }
        strip_report_id(tmp, got as usize)
    }

    /// For collections without an interrupt OUT pipe (where `write` fails): arms a read, sends
    /// `data` with `HidD_SetOutputReport`, and returns the first input report like `read`.
    pub fn set_output_and_read(&self, data: &[u8], timeout_ms: u32) -> Option<Vec<u8>> {
        if !self.overlapped || self.in_len == 0 || self.out_len == 0 || data.is_empty() {
            return None;
        }
        let out = padded(data, self.out_len);
        let mut tmp = vec![0u8; self.in_len];
        let mut got = 0u32;
        unsafe {
            let ov = Overlapped::new()?;
            let started = ReadFile(self.h, tmp.as_mut_ptr(), tmp.len() as u32, null_mut(), ov.ptr()) != 0
                || GetLastError() == ERROR_IO_PENDING;
            if !started {
                return None;
            }
            let sent = HidD_SetOutputReport(self.h, out.as_ptr().cast(), out.len() as u32) != 0;
            let done = self.finish(&ov, if sent { timeout_ms } else { 0 }, &mut got);
            if !(sent && done) {
                return None;
            }
        }
        strip_report_id(tmp, got as usize)
    }

    /// Discards queued input reports.
    pub fn flush(&self) {
        unsafe { HidD_FlushQueue(self.h) };
    }

    pub fn set_feature(&self, data: &[u8]) -> bool {
        if self.feat_len == 0 || data.is_empty() {
            return false;
        }
        let buf = padded(data, self.feat_len);
        unsafe { HidD_SetFeature(self.h, buf.as_ptr().cast(), buf.len() as u32) != 0 }
    }

    /// Feature report `report_id`; the returned buffer starts with the report ID.
    pub fn get_feature(&self, report_id: u8) -> Option<Vec<u8>> {
        if self.feat_len == 0 {
            return None;
        }
        let mut buf = vec![0u8; self.feat_len];
        buf[0] = report_id;
        let ok = unsafe { HidD_GetFeature(self.h, buf.as_mut_ptr().cast(), buf.len() as u32) != 0 };
        ok.then_some(buf)
    }

    /// Current input report `report_id` over the control pipe; starts with the report ID.
    pub fn get_input_report(&self, report_id: u8) -> Option<Vec<u8>> {
        if self.in_len == 0 {
            return None;
        }
        let mut buf = vec![0u8; self.in_len];
        buf[0] = report_id;
        let ok = unsafe { HidD_GetInputReport(self.h, buf.as_mut_ptr().cast(), buf.len() as u32) != 0 };
        ok.then_some(buf)
    }

    /// Waits for an overlapped operation; on timeout cancels it (it may still have completed).
    unsafe fn finish(&self, ov: &Overlapped, timeout_ms: u32, n: &mut u32) -> bool {
        if timeout_ms > 0 && WaitForSingleObject(ov.event(), timeout_ms) == WAIT_OBJECT_0 {
            return GetOverlappedResult(self.h, ov.ptr(), n, 0) != 0;
        }
        CancelIo(self.h);
        GetOverlappedResult(self.h, ov.ptr(), n, 1) != 0
    }
}

impl Drop for HidDev {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.h) };
    }
}

/// An OVERLAPPED with its manual-reset event, boxed so its address stays fixed while the
/// kernel holds it.
struct Overlapped(Box<OVERLAPPED>);

impl Overlapped {
    unsafe fn new() -> Option<Self> {
        let ev = CreateEventW(null(), 1, 0, null());
        if ev.is_null() {
            return None;
        }
        let mut ov: Box<OVERLAPPED> = Box::new(std::mem::zeroed());
        ov.hEvent = ev;
        Some(Self(ov))
    }
    fn ptr(&self) -> *mut OVERLAPPED {
        &*self.0 as *const OVERLAPPED as *mut OVERLAPPED
    }
    fn event(&self) -> HANDLE {
        self.0.hEvent
    }
}

impl Drop for Overlapped {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0.hEvent) };
    }
}

fn padded(data: &[u8], len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    let n = data.len().min(len);
    buf[..n].copy_from_slice(&data[..n]);
    buf
}

/// hidapi semantics: drop the leading "no report ID" byte.
fn strip_report_id(mut tmp: Vec<u8>, got: usize) -> Option<Vec<u8>> {
    if got == 0 {
        return None;
    }
    tmp.truncate(got.min(tmp.len()));
    if tmp[0] == 0 {
        tmp.remove(0);
    }
    Some(tmp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pads_and_strips() {
        assert_eq!(padded(&[1, 2, 3], 5), vec![1, 2, 3, 0, 0]);
        assert_eq!(padded(&[1, 2, 3], 2), vec![1, 2]);
        assert_eq!(strip_report_id(vec![0, 9, 8, 7], 3), Some(vec![9, 8]));
        assert_eq!(strip_report_id(vec![5, 9, 8], 3), Some(vec![5, 9, 8]));
        assert_eq!(strip_report_id(vec![0, 1], 0), None);
    }
}
