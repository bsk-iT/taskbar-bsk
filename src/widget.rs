//! Window plumbing.
//!
//! - A hidden top-level "controller" window receives broadcasts (TaskbarCreated after an
//!   Explorer restart, theme changes, device changes), owns the timer and the context menu.
//! - The visible "widget" is a layered child window of the taskbar (Shell_TrayWnd), placed
//!   just left of the notification area. It dies with Explorer and gets recreated.
//! - A worker thread scans devices and posts WM_APP_UPDATE to the controller.

use crate::device::{Reading, Status, Tracker};
use crate::{diag, drivers, init_com, notify, render, scan, settings, tray, wide};
use std::cell::RefCell;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows_sys::Win32::System::Threading::{CreateMutexW, GetCurrentProcess, SetProcessWorkingSetSize};
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::HiDpi::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const WM_APP_UPDATE: u32 = WM_APP + 1;
const WM_APP_MENU: u32 = WM_APP + 2;
const TIMER_PLACE: usize = 1;
const WM_APP_TRAY: u32 = WM_APP + 3;
const CMD_REFRESH: usize = 1;
const CMD_AUTOSTART: usize = 2;
const CMD_EXIT: usize = 3;
const CMD_ALERTS: usize = 4;
const CMD_COPY_DIAGNOSTICS: usize = 5;
const CMD_MODE_WIDGET: usize = 6;
const CMD_MODE_TRAY: usize = 7;
const CMD_HIDE_LAST_KNOWN: usize = 8;
const CMD_BACKGROUND: usize = 9;
/// WM_POWERBROADCAST: resumed from sleep/hibernate.
const PBT_APMRESUMEAUTOMATIC: usize = 0x12;
const CMD_DEVICE_BASE: usize = 100;

/// The worker wakes this often; each driver's own interval decides whether it is polled.
const TICK: Duration = drivers::FAST;

static READINGS: Mutex<Option<Vec<Reading>>> = Mutex::new(None);
static WAKE: OnceLock<Sender<()>> = OnceLock::new();

struct App {
    controller: HWND,
    widget: HWND,
    tooltip: HWND,
    taskbar_created: u32,
    light: bool,
    /// Rendered widget size; (0, 0) when there is nothing to show.
    size: (i32, i32),
    /// Taskbar layout the current bitmap was rendered for.
    layout: Option<render::Layout>,
    dpi: u32,
    want_widget: bool,
    use_widget: bool,
    hide_last_known: bool,
    background: bool,
    tray: Option<tray::Tray>,
}

thread_local! {
    static APP: RefCell<App> = const {
        RefCell::new(App {
            controller: null_mut(),
            widget: null_mut(),
            tooltip: null_mut(),
            taskbar_created: 0,
            light: false,
            size: (0, 0),
            layout: None,
            dpi: 0,
            want_widget: true,
            use_widget: false,
            hide_last_known: false,
            background: false,
            tray: Some(tray::Tray::new()),
        })
    };
}

/// Short borrows only; never hold one across a Win32 call that can dispatch messages.
fn with<R>(f: impl FnOnce(&mut App) -> R) -> R {
    APP.with(|a| f(&mut a.borrow_mut()))
}

pub fn run() {
    unsafe {
        let mutex_name = wide(r"Local\taskbar-bsk");
        CreateMutexW(null(), 0, mutex_name.as_ptr());
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return; // already running
        }
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        settings::ensure_autostart();
        settings::register_app_id();
        diag::start();

        let hinst = GetModuleHandleW(null());
        let ctrl_class = wide("TaskbarBskController");
        register(hinst, &ctrl_class, controller_proc);
        register(hinst, &wide("TaskbarBskWidget"), widget_proc);

        let taskbar_created = RegisterWindowMessageW(wide("TaskbarCreated").as_ptr());
        let controller = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            ctrl_class.as_ptr(),
            ctrl_class.as_ptr(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            hinst,
            null(),
        );
        with(|a| {
            a.controller = controller;
            a.taskbar_created = taskbar_created;
            a.light = settings::light_theme();
            a.want_widget = settings::widget_mode();
            a.hide_last_known = settings::hide_last_known();
            a.background = settings::widget_background();
        });

        let (tx, rx) = mpsc::channel();
        let _ = WAKE.set(tx);
        let controller_addr = controller as usize;
        std::thread::spawn(move || worker(controller_addr, rx));

        ensure_widget();
        redraw();
        SetTimer(controller, TIMER_PLACE, 1000, None);

        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

unsafe fn register(
    hinst: HINSTANCE,
    class: &[u16],
    proc_: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
) {
    let wc = WNDCLASSW {
        style: 0,
        lpfnWndProc: Some(proc_),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinst,
        hIcon: null_mut(),
        hCursor: LoadCursorW(null_mut(), IDC_ARROW),
        hbrBackground: null_mut(),
        lpszMenuName: null(),
        lpszClassName: class.as_ptr(),
    };
    RegisterClassW(&wc);
}

/// Debug builds: `--demo` shows fixed fake readings covering every visual state.
pub static DEMO: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn demo_readings() -> Vec<Reading> {
    use crate::device::Kind;
    let r = |name: &str, kind, percent, charging, stale| {
        Reading::new(name, name, kind, Status::Battery { percent, charging, stale })
    };
    vec![
        r("Headset", Kind::Headset, 64, true, false),
        r("Earbuds", Kind::Earbuds, 12, false, false),
        r("Mouse", Kind::Mouse, 87, false, true),
        r("Keyboard", Kind::Keyboard, 45, false, false),
        r("Phone", Kind::Phone, 72, false, false),
    ]
}

fn worker(controller: usize, rx: Receiver<()>) {
    if DEMO.load(std::sync::atomic::Ordering::Relaxed) {
        *READINGS.lock().unwrap() = Some(demo_readings());
        unsafe { PostMessageW(controller as HWND, WM_APP_UPDATE, 0, 0) };
        return;
    }
    init_com();
    let mut scanner = scan::Scanner::default();
    let mut tracker = Tracker::default();
    let mut alerts = notify::LowBatteryAlerts::default();
    let mut forced = true;

    loop {
        let readings = tracker.update(scanner.scan(forced));
        alerts.check(&readings);
        *READINGS.lock().unwrap() = Some(readings);
        unsafe { PostMessageW(controller as HWND, WM_APP_UPDATE, 0, 0) };
        trim_working_set();

        forced = match rx.recv_timeout(TICK) {
            Ok(()) => {
                // Device changes come in bursts; let things settle, then scan once.
                std::thread::sleep(Duration::from_millis(1500));
                while rx.try_recv().is_ok() {}
                true
            }
            Err(RecvTimeoutError::Timeout) => false,
            Err(RecvTimeoutError::Disconnected) => return,
        };
    }
}

/// The app is idle between scans, so hand its pages back to Windows until the next one.
/// They move to the standby list and come back cheaply when touched.
fn trim_working_set() {
    unsafe { SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX) };
}

fn wake() {
    if let Some(tx) = WAKE.get() {
        let _ = tx.send(());
    }
}

fn readings() -> Vec<Reading> {
    let mut devices = READINGS.lock().unwrap().clone().unwrap_or_default();
    sort_devices(&mut devices, &settings::device_order());
    devices
}

fn sort_devices(devices: &mut [Reading], order: &[String]) {
    devices.sort_by(|a, b| {
        let rank = |r: &Reading| order.iter().position(|id| id == &r.id).unwrap_or(usize::MAX);
        rank(a)
            .cmp(&rank(b))
            .then_with(|| a.kind.cmp(&b.kind))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.id.cmp(&b.id))
    });
}

fn is_shown(reading: &Reading, hidden: &[String], hide_last_known: bool) -> bool {
    !hidden.contains(&reading.id)
        && matches!(reading.status,
        Status::Battery { stale, .. } if !hide_last_known || !stale)
}

fn move_device(devices: &[Reading], id: &str, direction: isize) {
    let hidden = settings::hidden_devices();
    let hide_last_known = with(|a| a.hide_last_known);
    let mut order: Vec<_> =
        devices.iter().filter(|r| is_shown(r, &hidden, hide_last_known)).map(|r| r.id.clone()).collect();
    let Some(index) = order.iter().position(|key| key == id) else { return };
    let Some(other) = index.checked_add_signed(direction).filter(|i| *i < order.len()) else { return };
    order.swap(index, other);
    for old in settings::device_order() {
        if !order.contains(&old) {
            order.push(old);
        }
    }
    settings::set_device_order(&order);
}

unsafe fn destroy_widget() {
    let (widget, tip) = with(|a| {
        let handles = (a.widget, a.tooltip);
        a.widget = null_mut();
        a.tooltip = null_mut();
        a.size = (0, 0);
        a.layout = None;
        a.dpi = 0;
        handles
    });
    if !tip.is_null() && IsWindow(tip) != 0 {
        DestroyWindow(tip);
    }
    if !widget.is_null() && IsWindow(widget) != 0 {
        DestroyWindow(widget);
    }
}

unsafe fn sync_tray(shown: &[Reading], light: bool, enabled: bool) {
    let (owner, mut icons) = with(|a| (a.controller, a.tray.take().unwrap_or_else(tray::Tray::new)));
    if enabled {
        icons.sync(owner, WM_APP_TRAY, shown, light);
    } else {
        icons.clear(owner);
    }
    with(|a| a.tray = Some(icons));
}

unsafe fn find_tray() -> HWND {
    FindWindowW(wide("Shell_TrayWnd").as_ptr(), null())
}

unsafe fn ensure_widget() {
    let tray = find_tray();
    let want = with(|a| a.want_widget);
    let horizontal = !tray.is_null() && matches!(taskbar_layout(tray), render::Layout::Row { .. });
    if !want || !horizontal {
        destroy_widget();
        let changed = with(|a| {
            let changed = a.use_widget;
            a.use_widget = false;
            changed
        });
        if changed {
            redraw();
        }
        return;
    }
    let widget = with(|a| a.widget);
    if !widget.is_null() && IsWindow(widget) != 0 {
        return;
    }
    destroy_widget();
    let class = wide("TaskbarBskWidget");
    let widget = CreateWindowExW(
        WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        class.as_ptr(),
        null(),
        WS_CHILD | WS_CLIPSIBLINGS,
        0,
        0,
        1,
        1,
        tray,
        null_mut(),
        GetModuleHandleW(null()),
        null(),
    );
    let tip = if widget.is_null() { null_mut() } else { create_tooltip(with(|a| a.controller), widget) };
    with(|a| {
        a.widget = widget;
        a.tooltip = tip;
        a.use_widget = !widget.is_null();
    });
    redraw();
}

/// Hover tooltip listing each device's full name and status.
unsafe fn create_tooltip(owner: HWND, widget: HWND) -> HWND {
    let icc = INITCOMMONCONTROLSEX { dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32, dwICC: ICC_WIN95_CLASSES };
    InitCommonControlsEx(&icc);
    let tip = CreateWindowExW(
        WS_EX_TOPMOST,
        wide("tooltips_class32").as_ptr(),
        null(),
        WS_POPUP | TTS_NOPREFIX | TTS_ALWAYSTIP,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        owner,
        null_mut(),
        GetModuleHandleW(null()),
        null(),
    );
    if tip.is_null() {
        return tip;
    }
    let mut text = wide("");
    let mut ti = tool_info(widget, owner, &mut text);
    SendMessageW(tip, TTM_ADDTOOLW, 0, &mut ti as *mut _ as LPARAM);
    SendMessageW(tip, TTM_SETMAXTIPWIDTH, 0, 600); // enables multi-line text
    tip
}

unsafe fn tool_info(widget: HWND, owner: HWND, text: &mut [u16]) -> TTTOOLINFOW {
    let mut ti: TTTOOLINFOW = zeroed();
    ti.cbSize = size_of::<TTTOOLINFOW>() as u32;
    ti.uFlags = TTF_IDISHWND | TTF_SUBCLASS;
    ti.hwnd = owner;
    ti.uId = widget as usize;
    ti.lpszText = text.as_mut_ptr();
    ti
}

unsafe fn set_tooltip_text(lines: &[String]) {
    let (tip, widget, controller) = with(|a| (a.tooltip, a.widget, a.controller));
    if tip.is_null() {
        return;
    }
    let mut text = wide(&lines.join("\r\n"));
    let ti = tool_info(widget, controller, &mut text);
    SendMessageW(tip, TTM_UPDATETIPTEXTW, 0, &ti as *const _ as LPARAM);
}

unsafe fn redraw() {
    let (widget, light, use_widget, hide_last_known, background) =
        with(|a| (a.widget, a.light, a.use_widget, a.hide_last_known, a.background));
    let hidden = settings::hidden_devices();
    let shown: Vec<_> = readings().into_iter().filter(|r| is_shown(r, &hidden, hide_last_known)).collect();
    sync_tray(&shown, light, !use_widget);
    if !use_widget || widget.is_null() || IsWindow(widget) == 0 {
        return;
    }
    let taskbar = find_tray();
    if taskbar.is_null() {
        return;
    }
    let layout = taskbar_layout(taskbar);
    let dpi = GetDpiForWindow(taskbar).max(96);
    set_tooltip_text(&shown.iter().map(|r| format!("{}: {}", r.name, tray::status_text(r))).collect::<Vec<_>>());
    let size = if shown.is_empty() { (0, 0) } else { render::draw(widget, &shown, dpi, layout, light, background) };
    with(|a| {
        a.size = size;
        a.dpi = dpi;
        a.layout = Some(layout);
    });
    place();
}

/// A taskbar docked left/right is taller than wide; lay the widget out as a column there.
unsafe fn taskbar_layout(tray: HWND) -> render::Layout {
    let mut tr: RECT = zeroed();
    GetWindowRect(tray, &mut tr);
    let (w, h) = (tr.right - tr.left, tr.bottom - tr.top);
    if h > w {
        render::Layout::Column { width: w }
    } else {
        render::Layout::Row { height: h }
    }
}

/// Keeps the widget next to the notification area (left of it on a horizontal taskbar, above
/// it on a vertical one) and above the taskbar's XAML content.
unsafe fn place() {
    let (widget, (width, height), rendered, dpi) = with(|a| (a.widget, a.size, a.layout, a.dpi));
    if widget.is_null() || IsWindow(widget) == 0 {
        return;
    }
    let tray = find_tray();
    if tray.is_null() {
        return;
    }
    // Taskbar moved to another edge or resized: the bitmap was drawn for the old shape.
    if rendered != Some(taskbar_layout(tray)) || dpi != GetDpiForWindow(tray).max(96) {
        redraw(); // sets the new layout, then calls back here
        return;
    }
    if width == 0 {
        ShowWindow(widget, SW_HIDE); // nothing to show
        return;
    }
    let mut tr: RECT = zeroed();
    GetWindowRect(tray, &mut tr);
    let mut nr: RECT = zeroed();
    let notify = FindWindowExW(tray, null_mut(), wide("TrayNotifyWnd").as_ptr(), null());
    let has_notify = !notify.is_null() && GetWindowRect(notify, &mut nr) != 0 && nr.right > nr.left;
    let margin = 4 * GetDpiForWindow(tray).max(96) as i32 / 96;
    let (x, y) = match rendered {
        Some(render::Layout::Column { .. }) => {
            let bottom = if has_notify { nr.top } else { tr.bottom };
            (0, bottom - tr.top - height - margin)
        }
        _ => {
            let right = if has_notify { nr.left } else { tr.right };
            (right - tr.left - width - margin, 0)
        }
    };

    let mut cur: RECT = zeroed();
    GetWindowRect(widget, &mut cur);
    let moved = cur.left - tr.left != x
        || cur.top - tr.top != y
        || cur.right - cur.left != width
        || cur.bottom - cur.top != height;
    let on_top = above_content(tray, widget);
    if moved || !on_top || IsWindowVisible(widget) == 0 {
        SetWindowPos(widget, HWND_TOP, x, y, width, height, SWP_NOACTIVATE | SWP_SHOWWINDOW);
    }
}

/// True when the widget sits above the taskbar's XAML content. Other widgets may stay above it:
/// demanding the very first slot every tick fights them forever, and a sibling walking the
/// children while we jump to the top can miss us and draw over the widget.
unsafe fn above_content(tray: HWND, widget: HWND) -> bool {
    let bridge = wide("Windows.UI.Composition.DesktopWindowContentBridge");
    let mut class = [0u16; 64];
    let mut child = GetWindow(tray, GW_CHILD);
    // Bounded: siblings re-ordering mid-walk could otherwise send us around in circles.
    for _ in 0..64 {
        if child.is_null() {
            break;
        }
        if child == widget {
            return true;
        }
        let len = GetClassNameW(child, class.as_mut_ptr(), class.len() as i32).max(0) as usize;
        if class[..len] == bridge[..bridge.len() - 1] {
            return false;
        }
        child = GetWindow(child, GW_HWNDNEXT);
    }
    false
}

unsafe fn show_menu(hwnd: HWND) {
    let devices = readings();
    let hidden = settings::hidden_devices();
    let (want_widget, hide_last_known, background) = with(|a| (a.want_widget, a.hide_last_known, a.background));
    let shown: Vec<_> = devices.iter().filter(|r| is_shown(r, &hidden, hide_last_known)).collect();
    let menu = CreatePopupMenu();
    let checked = |on: bool| if on { MF_CHECKED } else { MF_UNCHECKED };
    for (i, reading) in devices.iter().enumerate() {
        let sub = CreatePopupMenu();
        let base = CMD_DEVICE_BASE + i * 4;
        let position = shown.iter().position(|r| r.id == reading.id);
        AppendMenuW(
            sub,
            MF_STRING | checked(!hidden.contains(&reading.id)),
            base,
            wide(if want_widget { "Mostrar na barra de tarefas" } else { "Mostrar na bandeja" }).as_ptr(),
        );
        AppendMenuW(sub, MF_SEPARATOR, 0, null());
        AppendMenuW(
            sub,
            MF_STRING | if position.is_some_and(|p| p > 0) { 0 } else { MF_GRAYED },
            base + 1,
            wide("Mover para a esquerda").as_ptr(),
        );
        AppendMenuW(
            sub,
            MF_STRING | if position.is_some_and(|p| p + 1 < shown.len()) { 0 } else { MF_GRAYED },
            base + 2,
            wide("Mover para a direita").as_ptr(),
        );
        let label = format!("{}:  {}", reading.name, tray::status_text(reading)).replace('&', "&&");
        AppendMenuW(menu, MF_POPUP | checked(!hidden.contains(&reading.id)), sub as usize, wide(&label).as_ptr());
    }
    if devices.is_empty() {
        AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, wide("Nenhum dispositivo com bateria encontrado").as_ptr());
    }
    AppendMenuW(
        menu,
        MF_STRING | checked(hide_last_known),
        CMD_HIDE_LAST_KNOWN,
        wide("Ocultar dispositivos dormindo / último valor").as_ptr(),
    );
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    AppendMenuW(menu, MF_STRING | checked(want_widget), CMD_MODE_WIDGET, wide("Mostrar na barra de tarefas").as_ptr());
    AppendMenuW(
        menu,
        MF_STRING | checked(!want_widget),
        CMD_MODE_TRAY,
        wide("Mostrar como ícones na bandeja").as_ptr(),
    );
    CheckMenuRadioItem(
        menu,
        CMD_MODE_WIDGET as u32,
        CMD_MODE_TRAY as u32,
        if want_widget { CMD_MODE_WIDGET } else { CMD_MODE_TRAY } as u32,
        MF_BYCOMMAND,
    );
    AppendMenuW(
        menu,
        MF_STRING | checked(background) | if want_widget { 0 } else { MF_GRAYED },
        CMD_BACKGROUND,
        wide("Fundo atrás do texto (para barra de tarefas transparente)").as_ptr(),
    );
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    AppendMenuW(menu, MF_STRING, CMD_REFRESH, wide("Atualizar agora").as_ptr());
    AppendMenuW(
        menu,
        MF_STRING | checked(settings::alerts_enabled()),
        CMD_ALERTS,
        wide("Alertas de bateria fraca").as_ptr(),
    );
    AppendMenuW(
        menu,
        MF_STRING | checked(settings::autostart_enabled()),
        CMD_AUTOSTART,
        wide("Iniciar com o Windows").as_ptr(),
    );
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    AppendMenuW(menu, MF_STRING, CMD_COPY_DIAGNOSTICS, wide("Copiar diagnóstico").as_ptr());
    AppendMenuW(menu, MF_STRING, CMD_EXIT, wide("Sair").as_ptr());
    let mut point: POINT = zeroed();
    GetCursorPos(&mut point);
    SetForegroundWindow(hwnd);
    let command = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | TPM_RIGHTALIGN,
        point.x,
        point.y,
        0,
        hwnd,
        null(),
    );
    PostMessageW(hwnd, WM_NULL, 0, 0);
    DestroyMenu(menu);
    match command as usize {
        0 => {}
        CMD_REFRESH => wake(),
        CMD_AUTOSTART => settings::set_autostart(!settings::autostart_enabled()),
        CMD_ALERTS => settings::set_alerts(!settings::alerts_enabled()),
        CMD_COPY_DIAGNOSTICS => copy_diagnostics(hwnd),
        CMD_EXIT => {
            DestroyWindow(hwnd);
        }
        CMD_MODE_WIDGET | CMD_MODE_TRAY => {
            let want = command as usize == CMD_MODE_WIDGET;
            settings::set_widget_mode(want);
            with(|a| a.want_widget = want);
            ensure_widget();
            redraw();
        }
        CMD_HIDE_LAST_KNOWN => {
            settings::set_hide_last_known(!hide_last_known);
            with(|a| a.hide_last_known = !hide_last_known);
            redraw();
        }
        CMD_BACKGROUND => {
            settings::set_widget_background(!background);
            with(|a| a.background = !background);
            redraw();
        }
        command if command >= CMD_DEVICE_BASE => {
            let offset = command - CMD_DEVICE_BASE;
            if let Some(reading) = devices.get(offset / 4) {
                match offset % 4 {
                    0 => settings::toggle_hidden(&reading.id),
                    1 => move_device(&devices, &reading.id, -1),
                    2 => move_device(&devices, &reading.id, 1),
                    _ => return,
                }
                redraw();
            }
        }
        _ => {}
    }
}

/// Puts the diagnostics log on the clipboard and confirms with a toast.
unsafe fn copy_diagnostics(hwnd: HWND) {
    let (title, body) = match diag::report() {
        Some(text) if set_clipboard(hwnd, &text) => {
            ("Diagnóstico copiado", "Cole no seu relatório de dispositivo no GitHub.")
        }
        Some(_) => {
            ("Não foi possível copiar o diagnóstico", "Outro app está usando a área de transferência. Tente de novo.")
        }
        None => ("Ainda não há diagnóstico", "Nada foi registrado até agora."),
    };
    // Toasts are WinRT, and COM is only initialized on the worker thread.
    std::thread::spawn(move || {
        init_com();
        let _ = notify::show(title, body);
    });
}

unsafe fn set_clipboard(hwnd: HWND, text: &str) -> bool {
    const CF_UNICODETEXT: u32 = 13;
    let units: Vec<u16> = text.replace('\n', "\r\n").encode_utf16().chain(std::iter::once(0)).collect();
    if OpenClipboard(hwnd) == 0 {
        return false;
    }
    let mut ok = false;
    let mem = GlobalAlloc(GMEM_MOVEABLE, units.len() * 2);
    if !mem.is_null() {
        let dst = GlobalLock(mem) as *mut u16;
        if !dst.is_null() {
            std::ptr::copy_nonoverlapping(units.as_ptr(), dst, units.len());
            GlobalUnlock(mem);
            EmptyClipboard();
            ok = !SetClipboardData(CF_UNICODETEXT, mem).is_null();
        }
        if !ok {
            GlobalFree(mem); // on success the clipboard owns it
        }
    }
    CloseClipboard();
    ok
}

unsafe extern "system" fn controller_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let taskbar_created = with(|a| a.taskbar_created);
    match msg {
        WM_APP_UPDATE => redraw(),
        WM_TIMER => {
            ensure_widget();
            place();
        }
        WM_SETTINGCHANGE => {
            with(|a| a.light = settings::light_theme());
            redraw();
        }
        WM_DPICHANGED | WM_DISPLAYCHANGE => {
            ensure_widget();
            redraw();
        }
        WM_DEVICECHANGE => {
            wake();
            return 1;
        }
        WM_POWERBROADCAST => {
            if wp == PBT_APMRESUMEAUTOMATIC {
                wake(); // PC woke from sleep: devices may have changed meanwhile
            }
            return 1;
        }
        WM_APP_MENU => show_menu(hwnd),
        WM_APP_TRAY => {
            let event = lp as u32 & 0xFFFF;
            if matches!(event, WM_CONTEXTMENU | WM_RBUTTONUP | WM_LBUTTONUP | windows_sys::Win32::UI::Shell::NIN_SELECT)
                || event == windows_sys::Win32::UI::Shell::NIN_SELECT + 1
            {
                show_menu(hwnd);
            }
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
        }
        WM_DESTROY => {
            destroy_widget();
            sync_tray(&[], false, false);
            PostQuitMessage(0);
        }
        m if m == taskbar_created && m != 0 => {
            with(|a| {
                a.widget = null_mut();
                a.use_widget = false;
                if let Some(icons) = a.tray.as_mut() {
                    icons.explorer_restarted();
                }
            });
            ensure_widget();
            redraw();
        }
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    0
}

unsafe extern "system" fn widget_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_LBUTTONUP | WM_RBUTTONUP => {
            let controller = with(|a| a.controller);
            PostMessageW(controller, WM_APP_MENU, 0, 0);
            0
        }
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::Kind;

    fn device(id: &str, name: &str, kind: Kind, stale: bool) -> Reading {
        Reading::new(id, name, kind, Status::Battery { percent: 50, charging: false, stale })
    }

    #[test]
    fn sorts_saved_order_before_kind_and_name() {
        let mut devices = vec![
            device("b", "B mouse", Kind::Mouse, false),
            device("phone", "Phone", Kind::Phone, false),
            device("a", "A mouse", Kind::Mouse, false),
            device("headset", "Headset", Kind::Headset, false),
        ];
        sort_devices(&mut devices, &["phone".into(), "gone".into()]);
        assert_eq!(devices.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["phone", "headset", "a", "b"]);
    }

    #[test]
    fn hides_only_explicitly_hidden_disconnected_or_requested_stale_devices() {
        let mut reading = device("mouse", "Mouse", Kind::Mouse, false);
        assert!(is_shown(&reading, &[], true));
        assert!(!is_shown(&reading, &["mouse".into()], false));
        reading.status = Status::Battery { percent: 50, charging: false, stale: true };
        assert!(is_shown(&reading, &[], false));
        assert!(!is_shown(&reading, &[], true));
        reading.status = Status::NotConnected;
        assert!(!is_shown(&reading, &[], false));
    }
}
