//! Win32 side of the main window (step 8): DWM dark frame + rounded corners, window rect /
//! work area, single instance (same mutex + restore port as Python, so Python and Rust never
//! run at the same time), and the "Start with Windows" Run-key entry.

use eframe::egui;
use std::sync::mpsc::Sender;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HWND, POINT, RECT};
use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows_sys::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetWindowLongPtrW, GetWindowPlacement, GetWindowRect, IsIconic, IsZoomed, SetForegroundWindow,
    SetWindowLongPtrW, SetWindowPlacement, SetWindowPos, ShowWindow, GWL_EXSTYLE, SWP_NOACTIVATE, SWP_NOZORDER, SW_HIDE,
    SW_RESTORE, SW_SHOWMINNOACTIVE, WINDOWPLACEMENT, WPF_RESTORETOMAXIMIZED, WPF_SETMINPOSITION, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};

const SINGLE_INSTANCE_MUTEX: &str = "ClipsVideoPlayerApp_SingleInstance_Mutex";
/// Loopback port used only as a signal channel between app instances (127.0.0.1 only).
const RESTORE_PORT: u16 = 51873;
const STARTUP_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const STARTUP_NAME: &str = "Feather Player";
const STARTUP_LEGACY_NAME: &str = "Clips";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The main window handle, from eframe's creation context.
pub fn hwnd_of(cc: &eframe::CreationContext<'_>) -> HWND {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match cc.window_handle().map(|h| h.as_raw()) {
        Ok(RawWindowHandle::Win32(h)) => h.hwnd.get() as HWND,
        _ => std::ptr::null_mut(),
    }
}

fn colorref(c: egui::Color32) -> u32 {
    c.r() as u32 | (c.g() as u32) << 8 | (c.b() as u32) << 16
}

/// Python `_apply_dark_titlebar`: dark mode, caption colour = BG (only a fallback), rounded
/// corners (Windows 11) and an outline in the BG colour so it is invisible.
pub fn apply_dark_frame(hwnd: HWND, bg: egui::Color32) {
    if hwnd.is_null() {
        return;
    }
    let set = |attr: u32, v: u32| unsafe {
        DwmSetWindowAttribute(hwnd, attr, &v as *const u32 as *const _, 4);
    };
    set(20, 1); // DWMWA_USE_IMMERSIVE_DARK_MODE
    set(35, colorref(bg)); // DWMWA_CAPTION_COLOR
    set(33, 2); // DWMWA_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND
    set(34, colorref(bg)); // DWMWA_BORDER_COLOR
}

/// winit message hook (every message, before it is dispatched): keeps frames coming while the
/// mouse moves. Windows hands out WM_PAINT (= eframe's next frame) only when no input is
/// waiting, and while the mouse moves fast there almost always is a new WM_MOUSEMOVE, so
/// frames came 200-600 ms apart and the video stuttered (measured 2026-11-28: each frame
/// took 0-4 ms, they just weren't delivered). On mouse input, a waiting WM_PAINT is taken
/// out of the queue and handled first, at most every 12 ms.
pub fn msg_hook(msg: *const std::ffi::c_void) -> bool {
    use std::cell::Cell;
    use std::time::{Duration, Instant};
    use windows_sys::Win32::UI::WindowsAndMessaging::{DispatchMessageW, PeekMessageW, MSG, PM_REMOVE, WM_INPUT, WM_MOUSEMOVE, WM_NCMOUSEMOVE, WM_PAINT};
    thread_local! {
        static LAST_PAINT: Cell<Option<Instant>> = const { Cell::new(None) };
    }
    if msg.is_null() {
        return false;
    }
    let m = unsafe { &*(msg as *const MSG) };
    match m.message {
        WM_PAINT => LAST_PAINT.with(|l| l.set(Some(Instant::now()))),
        WM_MOUSEMOVE | WM_NCMOUSEMOVE | WM_INPUT => {
            let due = LAST_PAINT.with(|l| l.get()).is_none_or(|t| t.elapsed() >= Duration::from_millis(12));
            if due {
                let mut p: MSG = unsafe { std::mem::zeroed() };
                if unsafe { PeekMessageW(&mut p, std::ptr::null_mut(), WM_PAINT, WM_PAINT, PM_REMOVE) } != 0 {
                    LAST_PAINT.with(|l| l.set(Some(Instant::now())));
                    unsafe { DispatchMessageW(&p) };
                }
            }
        }
        _ => {}
    }
    false // always dispatch the message itself as usual
}

/// Stop the raw mouse input winit registers for (`DeviceEvent::MouseMotion`, which nothing
/// here uses). A high polling-rate mouse sends thousands of WM_INPUT messages a second, and
/// Windows only delivers WM_PAINT (= the next frame) when the queue is empty, so moving the
/// mouse starved the redraws and the video stuttered, worse the faster it moved. Normal
/// WM_MOUSEMOVE / button messages (what egui uses) are not affected. winit registers only
/// once, at start-up, so this is called once after the window exists.
pub fn stop_raw_mouse_input() {
    use windows_sys::Win32::UI::Input::{RegisterRawInputDevices, RAWINPUTDEVICE, RIDEV_REMOVE};
    let dev = RAWINPUTDEVICE { usUsagePage: 0x01, usUsage: 0x02, dwFlags: RIDEV_REMOVE, hwndTarget: std::ptr::null_mut() };
    unsafe { RegisterRawInputDevices(&dev, 1, std::mem::size_of::<RAWINPUTDEVICE>() as u32) };
}

/// Fullscreen: square corners and no outline (Windows 11 rounds the window corners, which
/// cut the video's corners off); back to `apply_dark_frame`'s look when leaving.
pub fn set_fullscreen_frame(hwnd: HWND, fullscreen: bool, bg: egui::Color32) {
    if hwnd.is_null() {
        return;
    }
    if !fullscreen {
        apply_dark_frame(hwnd, bg);
        return;
    }
    let set = |attr: u32, v: u32| unsafe {
        DwmSetWindowAttribute(hwnd, attr, &v as *const u32 as *const _, 4);
    };
    set(33, 1); // DWMWA_WINDOW_CORNER_PREFERENCE = DWMWCP_DONOTROUND
    set(34, 0xFFFF_FFFE); // DWMWA_BORDER_COLOR = DWMWA_COLOR_NONE
}

/// (x, y, w, h) of the window's outer rect in screen pixels.
pub fn window_rect(hwnd: HWND) -> Option<(i32, i32, i32, i32)> {
    let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    (!hwnd.is_null() && unsafe { GetWindowRect(hwnd, &mut r) } != 0).then(|| (r.left, r.top, r.right - r.left, r.bottom - r.top))
}

/// Size the window had before it was maximised.
pub fn normal_size(hwnd: HWND) -> Option<(i32, i32)> {
    let mut wp: WINDOWPLACEMENT = unsafe { std::mem::zeroed() };
    wp.length = std::mem::size_of::<WINDOWPLACEMENT>() as u32;
    if hwnd.is_null() || unsafe { GetWindowPlacement(hwnd, &mut wp) } == 0 {
        return None;
    }
    let r = wp.rcNormalPosition;
    Some((r.right - r.left, r.bottom - r.top))
}

pub fn is_iconic(hwnd: HWND) -> bool {
    !hwnd.is_null() && unsafe { IsIconic(hwnd) } != 0
}

pub fn is_zoomed(hwnd: HWND) -> bool {
    !hwnd.is_null() && unsafe { IsZoomed(hwnd) } != 0
}

/// Python `_work_area`: usable rect (no taskbar) of the window's monitor.
pub fn work_area(hwnd: HWND) -> Option<(i32, i32, i32, i32)> {
    unsafe {
        let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(mon, &mut mi) == 0 {
            return None;
        }
        let r = mi.rcWork;
        Some((r.left, r.top, r.right - r.left, r.bottom - r.top))
    }
}

fn set_ex_style(hwnd: HWND, add: u32, remove: u32) {
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ((ex | add) & !remove) as isize);
    }
}

/// "Hidden" in the tray = a minimized tool window (no taskbar button, no Alt+Tab entry).
/// Not SW_HIDE on its own: a hidden window never gets WM_PAINT, so eframe's redraw request is
/// never answered, its loop spins a whole core and `update` stops running (cloud sync stalls).
/// A minimized window keeps `update` running at ~0 CPU.
pub fn hide_to_tray(hwnd: HWND) {
    if hwnd.is_null() {
        return;
    }
    unsafe {
        // GetWindowPlacement of a maximised window has no WPF_RESTORETOMAXIMIZED (that flag is
        // only set for a window minimised from maximised), so without it the window came back
        // from the tray at its normal size and that size was then saved over "max"
        let was_max = IsZoomed(hwnd) != 0;
        ShowWindow(hwnd, SW_HIDE); // the taskbar only notices the style change while hidden
        set_ex_style(hwnd, WS_EX_TOOLWINDOW, WS_EX_APPWINDOW);
        // a minimized tool window is parked at the bottom-left of the screen (a small caption
        // stub, visible with an auto-hide taskbar), so minimize it off-screen instead
        let mut wp: WINDOWPLACEMENT = std::mem::zeroed();
        wp.length = std::mem::size_of::<WINDOWPLACEMENT>() as u32;
        if GetWindowPlacement(hwnd, &mut wp) != 0 {
            wp.flags |= WPF_SETMINPOSITION;
            if was_max {
                wp.flags |= WPF_RESTORETOMAXIMIZED;
            }
            wp.ptMinPosition = POINT { x: -32000, y: -32000 };
            wp.showCmd = SW_SHOWMINNOACTIVE as _;
            SetWindowPlacement(hwnd, &wp);
        } else {
            ShowWindow(hwnd, SW_SHOWMINNOACTIVE);
        }
    }
}

/// Undoes `hide_to_tray`: taskbar button back, window restored (maximised again if it was)
/// and brought to the front.
pub fn restore_from_tray(hwnd: HWND) {
    if hwnd.is_null() {
        return;
    }
    unsafe {
        ShowWindow(hwnd, SW_HIDE);
        set_ex_style(hwnd, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW);
        ShowWindow(hwnd, SW_RESTORE);
        SetForegroundWindow(hwnd);
    }
}

pub fn set_outer_rect(hwnd: HWND, x: i32, y: i32, w: i32, h: i32) {
    if !hwnd.is_null() {
        unsafe { SetWindowPos(hwnd, std::ptr::null_mut(), x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE) };
    }
}

// ── Single instance ──────────────────────────────────────────────────────────

/// Python `_check_single_instance`: true if this is the first instance (the mutex is kept for
/// the life of the process). Otherwise the running one is asked to show itself.
pub fn check_single_instance() -> bool {
    let name = wide(SINGLE_INSTANCE_MUTEX);
    let mutex = unsafe { CreateMutexW(std::ptr::null(), 1, name.as_ptr()) };
    if unsafe { GetLastError() } != ERROR_ALREADY_EXISTS {
        return true; // handle deliberately never closed
    }
    if !mutex.is_null() {
        unsafe { CloseHandle(mutex) };
    }
    send_restore_signal();
    // best-effort extra nudge (harmless even if the window is hidden)
    let title = wide("Feather Player");
    let hwnd = unsafe { FindWindowW(std::ptr::null(), title.as_ptr()) };
    if !hwnd.is_null() {
        unsafe {
            ShowWindow(hwnd, SW_RESTORE);
            SetForegroundWindow(hwnd);
        }
    }
    false
}

fn send_restore_signal() {
    use std::io::Write;
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], RESTORE_PORT));
    if let Ok(mut s) = std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(1)) {
        let _ = s.write_all(b"restore");
    }
}

/// Python `_start_restore_listener`: each connection on the restore port = "show yourself".
pub fn start_restore_listener(tx: Sender<()>, ctx: egui::Context) {
    std::thread::spawn(move || {
        use std::io::Read;
        let Ok(srv) = std::net::TcpListener::bind(("127.0.0.1", RESTORE_PORT)) else {
            return; // port taken: the FindWindow nudge still works
        };
        for conn in srv.incoming() {
            let Ok(mut conn) = conn else { continue };
            let _ = conn.set_read_timeout(Some(std::time::Duration::from_secs(1)));
            let _ = conn.read(&mut [0u8; 64]);
            drop(conn);
            if tx.send(()).is_err() {
                return;
            }
            ctx.request_repaint();
        }
    });
}

/// Taskbar grouping id, same as Python.
pub fn set_app_id() {
    let id = wide("FeatherPlayer.App");
    unsafe { windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID(id.as_ptr()) };
}

// ── Clipboard ────────────────────────────────────────────────────────────────

/// Python `_copy_to_clipboard`: put the file on the clipboard as a file (CF_HDROP), so it can
/// be pasted into Discord or Explorer. True on success.
pub fn copy_file_to_clipboard(path: &str) -> bool {
    use windows_sys::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    // DROPFILES header: pFiles = 20, pt = (0, 0), fNC = 0, fWide = 1; then the path + "\0\0"
    let mut data: Vec<u8> = [20u32, 0, 0, 0, 1].iter().flat_map(|v| v.to_le_bytes()).collect();
    data.extend(path.encode_utf16().chain([0, 0]).flat_map(|u| u.to_le_bytes()));
    unsafe {
        let hmem = GlobalAlloc(GMEM_MOVEABLE, data.len());
        if hmem.is_null() {
            return false;
        }
        let ptr = GlobalLock(hmem) as *mut u8;
        if ptr.is_null() {
            return false;
        }
        std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
        GlobalUnlock(hmem);
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return false;
        }
        EmptyClipboard();
        let ok = !SetClipboardData(15, hmem as _).is_null(); // CF_HDROP
        CloseClipboard();
        ok
    }
}

// ── Start with Windows (per-user Run key, no admin needed) ───────────────────

/// Python `_startup_command`: --minimized makes the app start in the tray.
fn startup_command() -> String {
    let exe = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    format!("\"{exe}\" --minimized")
}

fn run_value_exists(name: &str) -> bool {
    let (k, n) = (wide(STARTUP_KEY), wide(name));
    unsafe { RegGetValueW(HKEY_CURRENT_USER, k.as_ptr(), n.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) == 0 }
}

fn set_run_value(name: &str, cmd: &str) -> bool {
    let (k, n, v) = (wide(STARTUP_KEY), wide(name), wide(cmd));
    unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, k.as_ptr(), n.as_ptr(), REG_SZ, v.as_ptr() as *const _, (v.len() * 2) as u32) == 0 }
}

fn delete_run_value(name: &str) -> bool {
    let (k, n) = (wide(STARTUP_KEY), wide(name));
    let r = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, k.as_ptr(), n.as_ptr()) };
    r == 0 || r == 2 // ERROR_FILE_NOT_FOUND = already gone
}

pub fn startup_enabled() -> bool {
    run_value_exists(STARTUP_NAME)
}

/// Python `_set_startup`. True on success.
pub fn set_startup(on: bool) -> bool {
    if on { set_run_value(STARTUP_NAME, &startup_command()) } else { delete_run_value(STARTUP_NAME) }
}

/// Python `_refresh_startup_entry`: if "Start with Windows" is on (also under the old "Clips"
/// name), point it at this exe. Python only does this for the installed exe, never for the
/// script; here a build inside a cargo `target` folder counts as "the script".
pub fn refresh_startup_entry() {
    let dev = std::env::current_exe().map(|p| p.components().any(|c| c.as_os_str().eq_ignore_ascii_case("target"))).unwrap_or(true);
    if dev {
        return;
    }
    let was_on = run_value_exists(STARTUP_NAME) || run_value_exists(STARTUP_LEGACY_NAME);
    delete_run_value(STARTUP_LEGACY_NAME);
    if was_on {
        set_run_value(STARTUP_NAME, &startup_command());
    }
}
