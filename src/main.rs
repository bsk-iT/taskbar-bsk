#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod device;
mod diag;
mod drivers;
mod hid;
mod notify;
mod render;
mod scan;
mod settings;
mod text;
mod tray;
mod widget;

pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Initializes COM (multithreaded) on the calling thread, needed for the WinRT APIs.
pub(crate) fn init_com() {
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
    }
}

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("--test-toast") if cfg!(debug_assertions) => {
            init_com();
            settings::register_app_id();
            if let Err(e) = notify::show("Bateria fraca", "G Pro Wireless está com 12%. Hora de carregar.") {
                eprintln!("toast failed: {e}");
            }
        }
        Some("--list") => list_to_console(false),
        Some("--dump") => list_to_console(true),
        Some("--quit") => unsafe {
            use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW, WM_CLOSE};
            let window = FindWindowW(wide("TaskbarBskController").as_ptr(), std::ptr::null());
            if !window.is_null() {
                PostMessageW(window, WM_CLOSE, 0, 0);
            }
        },
        Some("--demo") if cfg!(debug_assertions) => {
            widget::DEMO.store(true, std::sync::atomic::Ordering::Relaxed);
            widget::run();
        }
        _ => widget::run(),
    }
}

struct Console {
    handle: windows_sys::Win32::Foundation::HANDLE,
    owned: bool,
}

impl Console {
    fn parent() -> Option<Self> {
        use windows_sys::Win32::Foundation::{GENERIC_WRITE, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_WRITE, OPEN_EXISTING};
        use windows_sys::Win32::System::Console::{
            AttachConsole, GetStdHandle, ATTACH_PARENT_PROCESS, STD_OUTPUT_HANDLE,
        };
        unsafe {
            let handle = GetStdHandle(STD_OUTPUT_HANDLE);
            if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
                return Some(Self { handle, owned: false });
            }
            if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
                return None;
            }
            let handle = CreateFileW(
                wide("CONOUT$").as_ptr(),
                GENERIC_WRITE,
                FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            );
            (!handle.is_null() && handle != INVALID_HANDLE_VALUE).then_some(Self { handle, owned: true })
        }
    }

    fn write(&self, text: &str) {
        use windows_sys::Win32::Storage::FileSystem::WriteFile;
        let mut remaining = text.as_bytes();
        while !remaining.is_empty() {
            let mut written = 0;
            let size = remaining.len().min(u32::MAX as usize) as u32;
            let ok = unsafe { WriteFile(self.handle, remaining.as_ptr(), size, &mut written, std::ptr::null_mut()) };
            if ok == 0 || written == 0 {
                break;
            }
            remaining = &remaining[written as usize..];
        }
    }
}

impl Drop for Console {
    fn drop(&mut self) {
        if self.owned {
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(self.handle);
            }
        }
    }
}

fn list_to_console(dump: bool) {
    use std::fmt::Write;
    let console = Console::parent();
    init_com();
    diag::start();
    let mut scanner = scan::Scanner::default();
    let mut observations = scanner.scan(true);
    if dump {
        let mut text =
            format!("taskbar-bsk {} | {}\nHID collections\n", env!("CARGO_PKG_VERSION"), settings::windows_version());
        for collection in scanner.collections() {
            let _ = writeln!(
                text,
                "vid={:04x} pid={:04x} iface={} page={:04x} usage={:04x} input={} output={} feature={} product={}",
                collection.vid,
                collection.pid,
                collection.iface,
                collection.page,
                collection.usage,
                collection.in_len,
                collection.out_len,
                collection.feat_len,
                hid::product(collection)
            );
        }
        text.push_str("\nXenta frames\n");
        text.push_str(&drivers::misc_mice::dump_xenta_frames());
        let path = std::env::temp_dir().join("taskbar-bsk-dump.txt");
        let saved = std::fs::write(&path, format!("\u{feff}{}", text.replace('\n', "\r\n")));
        if let Some(console) = console {
            console.write(&text);
            match saved {
                Ok(()) => console.write(&format!("\nTambém salvo em {}\n", path.display())),
                Err(error) => console.write(&format!("\nNão foi possível salvar {}: {error}\n", path.display())),
            }
        } else if saved.is_ok() {
            unsafe {
                windows_sys::Win32::UI::Shell::ShellExecuteW(
                    std::ptr::null_mut(),
                    wide("open").as_ptr(),
                    wide(&path.to_string_lossy()).as_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
                );
            }
        }
        return;
    }
    // Push-based sources need a second pass after their first status report arrives.
    std::thread::sleep(std::time::Duration::from_millis(2500));
    observations.extend(scanner.scan(true));
    let readings = device::Tracker::default().update(observations);
    if let Some(console) = console {
        if readings.is_empty() {
            console.write("Nenhum dispositivo com bateria encontrado.\n");
        }
        for reading in readings {
            let voltage = reading.millivolts.map(|mv| format!(", {mv} mV")).unwrap_or_default();
            console.write(&format!(
                "{}  {}{}   ({})\n",
                reading.name,
                tray::status_text(&reading),
                voltage,
                reading.id
            ));
        }
    }
}
