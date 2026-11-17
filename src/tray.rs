//! Per-device notification-area icons, rendered with GDI and no image assets.

use crate::device::{Reading, Status};
use crate::{render::LOW_BATTERY, text, wide};
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::HiDpi::{GetDpiForSystem, GetDpiForWindow, GetSystemMetricsForDpi};
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

struct Entry {
    key: String,
    uid: u32,
    icon: HICON,
    signature: Option<(Status, bool, u32)>,
    tip: String,
    added: bool,
}

pub struct Tray {
    entries: Vec<Entry>,
    next_id: u32,
}

impl Tray {
    pub const fn new() -> Self {
        Self { entries: Vec::new(), next_id: 1 }
    }

    pub unsafe fn clear(&mut self, owner: HWND) {
        for entry in self.entries.drain(..) {
            delete(owner, entry.uid);
            if !entry.icon.is_null() {
                DestroyIcon(entry.icon);
            }
        }
    }

    pub fn explorer_restarted(&mut self) {
        for entry in &mut self.entries {
            entry.added = false;
        }
    }

    pub unsafe fn sync(&mut self, owner: HWND, callback: u32, readings: &[Reading], light: bool) {
        let taskbar = FindWindowW(wide("Shell_TrayWnd").as_ptr(), null());
        let dpi = if taskbar.is_null() { GetDpiForSystem() } else { GetDpiForWindow(taskbar) }.max(96);
        let placeholder = readings.is_empty();
        self.entries.retain_mut(|entry| {
            let keep = if placeholder { entry.key.is_empty() } else { readings.iter().any(|r| r.id == entry.key) };
            if !keep {
                delete(owner, entry.uid);
                if !entry.icon.is_null() {
                    DestroyIcon(entry.icon);
                }
            }
            keep
        });
        let target = Target { owner, callback, light, dpi };
        if placeholder {
            self.apply(
                target,
                "",
                Status::NotConnected,
                "taskbar-bsk\nNenhum dispositivo com bateria encontrado".into(),
            );
        } else {
            for reading in readings {
                self.apply(target, &reading.id, reading.status, format!("{}\n{}", reading.name, status_text(reading)));
            }
        }
    }

    unsafe fn apply(&mut self, target: Target, key: &str, status: Status, tip: String) {
        let Target { owner, callback, light, dpi } = target;
        let index = match self.entries.iter().position(|entry| entry.key == key) {
            Some(index) => index,
            None => {
                let uid = self.next_id;
                self.next_id += 1;
                self.entries.push(Entry {
                    key: key.into(),
                    uid,
                    icon: null_mut(),
                    signature: None,
                    tip: String::new(),
                    added: false,
                });
                self.entries.len() - 1
            }
        };
        let entry = &mut self.entries[index];
        let signature = (status, light, dpi);
        let changed = entry.signature != Some(signature) || entry.icon.is_null();
        if changed {
            let icon = icon_for(status, key.is_empty(), light, dpi);
            if icon.is_null() {
                return;
            }
            if !entry.icon.is_null() {
                DestroyIcon(entry.icon);
            }
            entry.icon = icon;
            entry.signature = Some(signature);
        }
        if entry.added && !changed && entry.tip == tip {
            return;
        }
        let mut data: NOTIFYICONDATAW = zeroed();
        data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = owner;
        data.uID = entry.uid;
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        data.uCallbackMessage = callback;
        data.hIcon = entry.icon;
        copy_tip(&mut data.szTip, &tip);
        let message = if entry.added { NIM_MODIFY } else { NIM_ADD };
        if Shell_NotifyIconW(message, &data) != 0 {
            if !entry.added {
                data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
                Shell_NotifyIconW(NIM_SETVERSION, &data);
            }
            entry.added = true;
            entry.tip = tip;
        } else {
            entry.added = false;
        }
    }
}

/// Where and how one sync draws its icons.
#[derive(Clone, Copy)]
struct Target {
    owner: HWND,
    callback: u32,
    light: bool,
    dpi: u32,
}

unsafe fn delete(owner: HWND, uid: u32) {
    let mut data: NOTIFYICONDATAW = zeroed();
    data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = owner;
    data.uID = uid;
    Shell_NotifyIconW(NIM_DELETE, &data);
}

fn copy_tip(dst: &mut [u16], text: &str) {
    let capacity = dst.len().saturating_sub(1);
    let mut end = 0;
    for ch in text.chars() {
        let mut units = [0u16; 2];
        let encoded = ch.encode_utf16(&mut units);
        if end + encoded.len() > capacity {
            break;
        }
        dst[end..end + encoded.len()].copy_from_slice(encoded);
        end += encoded.len();
    }
    if let Some(last) = dst.get_mut(end) {
        *last = 0;
    }
}

pub fn status_text(reading: &Reading) -> String {
    let mut text = match reading.status {
        Status::NotConnected => "desligado / não conectado".into(),
        Status::Battery { percent, charging, stale } => {
            let mut text = if stale {
                format!("{percent}% (último valor)")
            } else {
                reading.level_text.map(|t| text::level(t).to_owned()).unwrap_or_else(|| format!("{percent}%"))
            };
            if !stale {
                if let Some(detail) = reading.detail.as_deref().filter(|s| !s.is_empty()) {
                    text.push_str(&format!(" ({detail})"));
                }
            }
            if charging {
                text.push_str(", carregando");
            }
            if stale {
                text.push_str(", dormindo");
            }
            text
        }
    };
    if !reading.via.is_empty() {
        text.push_str(&format!(" [{}]", text::via(reading.via)));
    }
    text
}

unsafe fn icon_for(status: Status, placeholder: bool, light: bool, dpi: u32) -> HICON {
    let foreground = if light { 0 } else { 0xFFFFFF };
    let (text, percent, charging, dim) = match status {
        Status::Battery { percent, charging, stale } => (percent.to_string(), i32::from(percent), charging, stale),
        Status::NotConnected => (if placeholder { "-" } else { "?" }.into(), -1, false, true),
    };
    let bar = if charging {
        0x00AAFF
    } else if percent <= i32::from(LOW_BATTERY) {
        0xE82030
    } else if percent <= 30 {
        0xFF9600
    } else {
        0x1EBE5A
    };
    let color = if !charging && (0..=i32::from(LOW_BATTERY)).contains(&percent) { 0xF0323C } else { foreground };
    render_icon(
        &text,
        percent,
        color,
        if placeholder { foreground } else { bar },
        foreground,
        dim,
        GetSystemMetricsForDpi(SM_CXSMICON, dpi).max(16),
    )
}

unsafe fn render_icon(
    text: &str,
    percent: i32,
    text_color: u32,
    bar_color: u32,
    foreground: u32,
    dim: bool,
    size: i32,
) -> HICON {
    let bar_height = (size / 8).max(2);
    let text_height = size - bar_height - 1;
    let (width, height) = (size * 3, size * 2);
    let dc = CreateCompatibleDC(null_mut());
    if dc.is_null() {
        return null_mut();
    }
    let mut info: BITMAPINFO = zeroed();
    info.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
    info.bmiHeader.biPlanes = 1;
    info.bmiHeader.biBitCount = 32;
    info.bmiHeader.biCompression = BI_RGB;
    info.bmiHeader.biWidth = width;
    info.bmiHeader.biHeight = -height;
    let mut bits = null_mut();
    let scratch = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
    if scratch.is_null() || bits.is_null() {
        if !scratch.is_null() {
            DeleteObject(scratch);
        }
        DeleteDC(dc);
        return null_mut();
    }
    let old_bitmap = SelectObject(dc, scratch);
    SetBkMode(dc, TRANSPARENT as _);
    SetTextColor(dc, 0xFFFFFF);
    let text = wide(text);
    let pixels = std::slice::from_raw_parts_mut(bits.cast::<u32>(), (width * height) as usize);
    let mut bounds = None;
    for font_height in (6..=text_height * 2).rev() {
        let mut font: LOGFONTW = zeroed();
        font.lfHeight = -font_height;
        font.lfWeight = FW_BOLD as _;
        font.lfQuality = ANTIALIASED_QUALITY as _;
        for (dst, src) in font.lfFaceName.iter_mut().zip("Segoe UI".encode_utf16()) {
            *dst = src;
        }
        let font = CreateFontIndirectW(&font);
        let old_font = SelectObject(dc, font);
        SetTextCharacterExtra(dc, if text.len() > 3 { -1 } else { 0 });
        pixels.fill(0);
        TextOutW(dc, 2, 0, text.as_ptr(), text.len() as i32 - 1);
        GdiFlush();
        SelectObject(dc, old_font);
        DeleteObject(font);
        bounds = glyph_bounds(pixels, width as usize, height as usize);
        match bounds {
            Some((x0, y0, x1, y1)) if x1 - x0 + 1 > size as usize || y1 - y0 + 1 > text_height as usize => {}
            _ => break,
        }
    }
    info.bmiHeader.biWidth = size;
    info.bmiHeader.biHeight = -size;
    let mut output_bits = null_mut();
    let color = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut output_bits, null_mut(), 0);
    let mut icon = null_mut();
    if !color.is_null() && !output_bits.is_null() {
        let output = std::slice::from_raw_parts_mut(output_bits.cast::<u32>(), (size * size) as usize);
        output.fill(0);
        let opacity = if dim { 140 } else { 255 };
        let mut put = |x: i32, y: i32, color: u32, alpha: u32| {
            if x >= 0 && x < size && y >= 0 && y < size {
                output[(y * size + x) as usize] = ((alpha * opacity / 255) << 24) | color;
            }
        };
        if let Some((x0, y0, x1, y1)) = bounds {
            let (gw, gh) = ((x1 - x0 + 1) as i32, (y1 - y0 + 1) as i32);
            for y in 0..gh {
                for x in 0..gw {
                    put(
                        (size - gw) / 2 + x,
                        (text_height - gh) / 2 + y,
                        text_color,
                        pixels[(y0 + y as usize) * width as usize + x0 + x as usize] & 0xFF,
                    );
                }
            }
        }
        let fill = if percent < 0 { 0 } else { ((percent * size + 50) / 100).max(i32::from(percent > 0)) };
        for y in size - bar_height..size {
            for x in 0..size {
                put(x, y, if x < fill { bar_color } else { foreground }, if x < fill { 255 } else { 70 });
            }
        }
        let mask = CreateBitmap(size, size, 1, 1, null());
        if !mask.is_null() {
            let mask_dc = CreateCompatibleDC(null_mut());
            if !mask_dc.is_null() {
                let old_mask = SelectObject(mask_dc, mask);
                PatBlt(mask_dc, 0, 0, size, size, BLACKNESS);
                SelectObject(mask_dc, old_mask);
                DeleteDC(mask_dc);
                let info = ICONINFO { fIcon: 1, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
                icon = CreateIconIndirect(&info);
            }
            DeleteObject(mask);
        }
    }
    SelectObject(dc, old_bitmap);
    DeleteObject(scratch);
    if !color.is_null() {
        DeleteObject(color);
    }
    DeleteDC(dc);
    icon
}

fn glyph_bounds(pixels: &[u32], width: usize, height: usize) -> Option<(usize, usize, usize, usize)> {
    let (mut x0, mut y0, mut x1, mut y1) = (width, height, 0, 0);
    let mut found = false;
    for (i, pixel) in pixels.iter().take(width * height).enumerate() {
        if pixel & 0xFF != 0 {
            let (x, y) = (i % width, i / width);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
            found = true;
        }
    }
    found.then_some((x0, y0, x1, y1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::Kind;

    #[test]
    fn status_includes_coarse_level_details_and_connection() {
        let mut reading =
            Reading::new("test", "Test", Kind::Headset, Status::Battery { percent: 20, charging: true, stale: false });
        reading.level_text = Some("Low");
        reading.detail = Some("E 20%, D 30%".into());
        reading.via = "USB cable";
        assert_eq!(status_text(&reading), "Baixa (E 20%, D 30%), carregando [cabo USB]");
        reading.status = Status::Battery { percent: 20, charging: false, stale: true };
        assert_eq!(status_text(&reading), "20% (último valor), dormindo [cabo USB]");
        reading.status = Status::NotConnected;
        assert_eq!(status_text(&reading), "desligado / não conectado [cabo USB]");
    }

    #[test]
    fn finds_ink_bounds() {
        assert_eq!(glyph_bounds(&[0, 0, 0, 0], 2, 2), None);
        assert_eq!(glyph_bounds(&[0, 255, 0, 0, 0, 255], 3, 2), Some((1, 0, 2, 1)));
    }

    #[test]
    fn tooltip_keeps_terminator_and_whole_surrogate_pairs() {
        let mut tip = [0u16; 4];
        copy_tip(&mut tip, "A\u{1F3AE}B");
        assert_eq!(tip, [65, 0xD83C, 0xDFAE, 0]);
    }
}
