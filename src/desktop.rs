//! Notification-area icon and a separate, non-activating focus-ring window.
//! The timer keeps working even while eframe's main window is hidden.
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::sync::{OnceLock, mpsc};
use std::thread;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, PCWSTR, Result, w};

use crate::{app_state, current_language, game_text, report_process_error};

const TRAY_MESSAGE: u32 = WM_APP + 41;
const STOP_MESSAGE: u32 = WM_APP + 42;
const RING_SIZE: i32 = 48;
const TRANSPARENT: COLORREF = COLORREF(0x00ff00ff);
static MAIN: AtomicIsize = AtomicIsize::new(0);
static CONTROLLER: AtomicIsize = AtomicIsize::new(0);
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
static CONTEXT: OnceLock<egui::Context> = OnceLock::new();
use eframe::egui;

thread_local! {
    static TRAY_ICON: std::cell::Cell<HICON> = const { std::cell::Cell::new(HICON(std::ptr::null_mut())) };
    static DRAGGING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static TRAY_READY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub struct Desktop {
    thread: Option<thread::JoinHandle<()>>,
}

impl Desktop {
    pub fn start(main: isize, context: egui::Context) -> std::result::Result<Self, String> {
        MAIN.store(main, Ordering::Relaxed);
        let _ = CONTEXT.set(context);
        let (tx, rx) = mpsc::sync_channel(1);
        let thread = thread::spawn(move || {
            if let Err(error) = run(&tx) {
                let _ = tx.send(Err(error.to_string()));
            }
        });
        match rx.recv().map_err(|error| error.to_string())? {
            Ok(()) => Ok(Self {
                thread: Some(thread),
            }),
            Err(error) => {
                let _ = thread.join();
                Err(error)
            }
        }
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        let hwnd = HWND(CONTROLLER.load(Ordering::Relaxed) as *mut _);
        if !hwnd.is_invalid() {
            let _ = unsafe { PostMessageW(Some(hwnd), STOP_MESSAGE, WPARAM(0), LPARAM(0)) };
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn tray_data(hwnd: HWND, icon: HICON) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: TRAY_MESSAGE,
        hIcon: icon,
        ..Default::default()
    };
    let tip: Vec<u16> = "SlackInput — Game companion\0".encode_utf16().collect();
    data.szTip[..tip.len()].copy_from_slice(&tip);
    data
}

fn restore() {
    let main = HWND(MAIN.load(Ordering::Relaxed) as *mut _);
    unsafe {
        let _ = ShowWindowAsync(main, SW_RESTORE);
        let _ = SetForegroundWindow(main);
    }
    if let Some(ctx) = CONTEXT.get() {
        ctx.request_repaint();
    }
}

unsafe extern "system" fn controller_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == TASKBAR_CREATED.load(Ordering::Relaxed) && msg != 0 {
        TRAY_ICON.with(|icon| {
            let ok = unsafe { Shell_NotifyIconW(NIM_ADD, &tray_data(hwnd, icon.get())) }.as_bool();
            TRAY_READY.with(|v| v.set(ok));
            // If Explorer cannot recreate the icon, make the app reachable again.
            if !ok {
                restore();
            }
        });
        return LRESULT(0);
    }
    match msg {
        TRAY_MESSAGE => {
            match lp.0 as u32 {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => restore(),
                WM_RBUTTONUP | WM_CONTEXTMENU => unsafe { tray_menu(hwnd) },
                _ => {}
            }
            LRESULT(0)
        }
        STOP_MESSAGE => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

unsafe fn tray_menu(hwnd: HWND) {
    let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
        return;
    };
    let language = current_language();
    let paused = app_state()
        .lock()
        .unwrap()
        .bound_process
        .as_ref()
        .is_some_and(|p| p.paused);
    let locked = app_state().lock().unwrap().config.focus_locked;
    let labels = [
        game_text(language, "Open SlackInput", "打开 SlackInput"),
        game_text(language, "Resume game", "恢复游戏"),
        if locked {
            game_text(language, "Unlock focus ring", "解锁圆点")
        } else {
            game_text(language, "Lock focus ring", "锁定圆点")
        },
        game_text(language, "Exit", "退出"),
    ];
    for (i, label) in labels.iter().enumerate() {
        let wide: Vec<u16> = label.encode_utf16().chain(Some(0)).collect();
        let flags = if i == 1 && !paused {
            MF_STRING | MF_GRAYED
        } else {
            MF_STRING
        };
        let _ = unsafe { AppendMenuW(menu, flags, i + 1, PCWSTR(wide.as_ptr())) };
    }
    let mut point = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut point);
        let _ = SetForegroundWindow(hwnd);
        let command = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            point.x,
            point.y,
            Some(0),
            hwnd,
            None,
        )
        .0;
        let _ = DestroyMenu(menu);
        match command {
            1 => restore(),
            2 => {
                let result = app_state()
                    .lock()
                    .unwrap()
                    .bound_process
                    .as_mut()
                    .map(|p| p.resume())
                    .transpose();
                if let Err(error) = result {
                    report_process_error(language, error);
                    restore();
                }
            }
            3 => {
                app_state().lock().unwrap().config.focus_locked = !locked;
            }
            4 => {
                restore();
                if let Some(ctx) = CONTEXT.get() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            _ => {}
        }
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
    }
    if let Some(ctx) = CONTEXT.get() {
        ctx.request_repaint();
    }
}

unsafe extern "system" fn ring_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_NCHITTEST => {
            if app_state().lock().unwrap().config.focus_locked {
                LRESULT(HTTRANSPARENT as isize)
            } else {
                LRESULT(HTCAPTION as isize)
            }
        }
        WM_ENTERSIZEMOVE => {
            DRAGGING.with(|v| v.set(true));
            LRESULT(0)
        }
        WM_EXITSIZEMOVE => {
            DRAGGING.with(|v| v.set(false));
            let mut rect = RECT::default();
            if unsafe { GetWindowRect(hwnd, &mut rect) }.is_ok() {
                let pid = app_state()
                    .lock()
                    .unwrap()
                    .bound_process
                    .as_ref()
                    .map(|p| p.pid);
                if let Some(game) = pid.and_then(game_rect) {
                    let mut state = app_state().lock().unwrap();
                    state.config.focus_x = ((rect.left + RING_SIZE / 2 - game.left) as f32
                        / (game.right - game.left) as f32)
                        .clamp(0.0, 1.0);
                    state.config.focus_y = ((rect.top + RING_SIZE / 2 - game.top) as f32
                        / (game.bottom - game.top) as f32)
                        .clamp(0.0, 1.0);
                }
            }
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = unsafe { BeginPaint(hwnd, &mut ps) };
            let diameter = app_state().lock().unwrap().config.focus_diameter as i32;
            unsafe {
                let background = CreateSolidBrush(TRANSPARENT);
                let _ = FillRect(
                    dc,
                    &RECT {
                        left: 0,
                        top: 0,
                        right: RING_SIZE,
                        bottom: RING_SIZE,
                    },
                    background,
                );
                let _ = DeleteObject(background.into());
                let brush = SelectObject(dc, GetStockObject(NULL_BRUSH));
                let pen = CreatePen(PS_SOLID, 3, COLORREF(0x00ffffff));
                let previous = SelectObject(dc, pen.into());
                let start = (RING_SIZE - diameter) / 2;
                let _ = Ellipse(dc, start, start, start + diameter, start + diameter);
                SelectObject(dc, previous);
                SelectObject(dc, brush);
                let _ = DeleteObject(pen.into());
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

struct GameWindow {
    pid: u32,
    rect: Option<RECT>,
    hwnd: HWND,
    area: i64,
}
unsafe extern "system" fn find_game(hwnd: HWND, lp: LPARAM) -> BOOL {
    let search = unsafe { &mut *(lp.0 as *mut GameWindow) };
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    let mut cloaked: u32 = 0;
    if pid == search.pid {
        let _ = unsafe {
            DwmGetWindowAttribute(
                hwnd,
                DWMWA_CLOAKED,
                &mut cloaked as *mut _ as *mut _,
                std::mem::size_of::<u32>() as u32,
            )
        };
    }
    if pid == search.pid
        && cloaked == 0
        && unsafe { IsWindowVisible(hwnd).as_bool() && !IsIconic(hwnd).as_bool() }
    {
        let mut rect = RECT::default();
        if unsafe { GetClientRect(hwnd, &mut rect) }.is_ok() {
            let mut origin = POINT::default();
            if unsafe { ClientToScreen(hwnd, &mut origin) }.as_bool() {
                let area = i64::from(rect.right) * i64::from(rect.bottom);
                if area > search.area {
                    rect.left += origin.x;
                    rect.right += origin.x;
                    rect.top += origin.y;
                    rect.bottom += origin.y;
                    search.rect = Some(rect);
                    search.hwnd = hwnd;
                    search.area = area;
                }
            }
        }
    }
    BOOL(1)
}

fn game_rect(pid: u32) -> Option<RECT> {
    game_window(pid).map(|(_, rect)| rect)
}

fn game_window(pid: u32) -> Option<(HWND, RECT)> {
    let mut search = GameWindow {
        pid,
        rect: None,
        hwnd: HWND::default(),
        area: 0,
    };
    let _ = unsafe { EnumWindows(Some(find_game), LPARAM(&mut search as *mut _ as isize)) };
    search.rect.map(|rect| (search.hwnd, rect))
}

pub fn should_show(enabled: bool, bound: bool, paused: bool, exited: bool) -> bool {
    enabled && bound && !paused && !exited
}

fn update_ring(
    hwnd: HWND,
    last: &mut Option<(i32, i32, bool, u8, u8)>,
    desktops: Option<&IVirtualDesktopManager>,
) {
    let (config, pid) = {
        let state = app_state().lock().unwrap();
        let pid = state
            .bound_process
            .as_ref()
            .filter(|p| should_show(state.config.focus_enabled, true, p.paused, p.exited()))
            .map(|p| p.pid);
        (state.config.clone(), pid)
    };
    let Some((game, rect)) = pid.and_then(game_window) else {
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
        *last = None;
        return;
    };
    if DRAGGING.with(|v| v.get()) {
        return;
    }
    if let Some(desktops) = desktops {
        unsafe {
            if let Ok(id) = desktops.GetWindowDesktopId(game) {
                if desktops.GetWindowDesktopId(hwnd).ok() != Some(id) {
                    let _ = desktops.MoveWindowToDesktop(hwnd, &id);
                }
            }
        }
    }
    let x = rect.left + ((rect.right - rect.left) as f32 * config.focus_x) as i32 - RING_SIZE / 2;
    let y = rect.top + ((rect.bottom - rect.top) as f32 * config.focus_y) as i32 - RING_SIZE / 2;
    let next = (
        x,
        y,
        config.focus_locked,
        config.focus_diameter,
        config.focus_opacity,
    );
    unsafe {
        if *last != Some(next) {
            let mut style = WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST;
            if config.focus_locked {
                style |= WS_EX_TRANSPARENT;
            }
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style.0 as isize);
            let _ = SetLayeredWindowAttributes(
                hwnd,
                TRANSPARENT,
                config.focus_opacity,
                LWA_COLORKEY | LWA_ALPHA,
            );
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                x,
                y,
                RING_SIZE,
                RING_SIZE,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            let _ = InvalidateRect(Some(hwnd), None, false);
            *last = Some(next);
        } else {
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }
}

#[derive(Default)]
struct NativeResources {
    controller: HWND,
    ring: HWND,
    icon: HICON,
}

impl Drop for NativeResources {
    fn drop(&mut self) {
        unsafe {
            if !self.controller.is_invalid() {
                let _ = KillTimer(Some(self.controller), 1);
                let _ = Shell_NotifyIconW(NIM_DELETE, &tray_data(self.controller, self.icon));
                let _ = DestroyWindow(self.controller);
            }
            if !self.ring.is_invalid() {
                let _ = DestroyWindow(self.ring);
            }
            if !self.icon.is_invalid() {
                let _ = DestroyIcon(self.icon);
            }
            CONTROLLER.store(0, Ordering::Relaxed);
        }
    }
}

fn run(ready: &mpsc::SyncSender<std::result::Result<(), String>>) -> Result<()> {
    unsafe {
        struct ComApartment;
        impl Drop for ComApartment {
            fn drop(&mut self) {
                unsafe { CoUninitialize() };
            }
        }
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let _com = ComApartment;
        let desktops: Option<IVirtualDesktopManager> =
            CoCreateInstance(&VirtualDesktopManager, None, CLSCTX_ALL).ok();
        let mut resources = NativeResources::default();
        let instance = GetModuleHandleW(None)?;
        let classes: [(PCWSTR, WNDPROC); 2] = [
            (w!("SlackInput.Tray"), Some(controller_proc)),
            (w!("SlackInput.FocusRing"), Some(ring_proc)),
        ];
        for (name, proc) in classes {
            let class = WNDCLASSW {
                lpfnWndProc: proc,
                hInstance: instance.into(),
                lpszClassName: name,
                hCursor: LoadCursorW(None, IDC_SIZEALL)?,
                ..Default::default()
            };
            if RegisterClassW(&class) == 0 {
                return Err(windows::core::Error::from_thread());
            }
        }
        let controller = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("SlackInput.Tray"),
            w!("SlackInput.Tray"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        resources.controller = controller;
        CONTROLLER.store(controller.0 as isize, Ordering::Relaxed);
        let ring = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
            w!("SlackInput.FocusRing"),
            w!("SlackInput.FocusRing"),
            WS_POPUP,
            0,
            0,
            RING_SIZE,
            RING_SIZE,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        resources.ring = ring;
        let icon = CreateIconFromResourceEx(
            include_bytes!("../assets/icon.png"),
            true,
            0x00030000,
            32,
            32,
            LR_DEFAULTCOLOR,
        )?;
        resources.icon = icon;
        TRAY_ICON.with(|v| v.set(icon));
        TASKBAR_CREATED.store(
            RegisterWindowMessageW(w!("TaskbarCreated")),
            Ordering::Relaxed,
        );
        Shell_NotifyIconW(NIM_ADD, &tray_data(controller, icon)).ok()?;
        TRAY_READY.with(|v| v.set(true));
        if SetTimer(Some(controller), 1, 100, None) == 0 {
            return Err(windows::core::Error::from_thread());
        }
        let _ = ready.send(Ok(()));
        let mut last = None;
        let mut last_retry = std::time::Instant::now();
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            if msg.message == WM_TIMER {
                if !TRAY_READY.with(|v| v.get()) && last_retry.elapsed().as_secs() >= 2 {
                    let ok = Shell_NotifyIconW(NIM_ADD, &tray_data(controller, icon)).as_bool();
                    TRAY_READY.with(|v| v.set(ok));
                    last_retry = std::time::Instant::now();
                }
                let main = HWND(MAIN.load(Ordering::Relaxed) as *mut _);
                if TRAY_READY.with(|v| v.get())
                    && IsIconic(main).as_bool()
                    && IsWindowVisible(main).as_bool()
                {
                    let _ = ShowWindowAsync(main, SW_HIDE);
                }
                update_ring(ring, &mut last, desktops.as_ref());
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    unsafe extern "system" fn test_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
    }

    fn pump(milliseconds: u64) {
        let end = Instant::now() + Duration::from_millis(milliseconds);
        while Instant::now() < end {
            let mut msg = MSG::default();
            unsafe {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn native_tray_and_ring_lifecycle() {
        let config = crate::AppConfig {
            focus_enabled: true,
            focus_locked: true,
            ..Default::default()
        };
        let _ = crate::APP_STATE.set(std::sync::Mutex::new(crate::AppState::new(
            config,
            Vec::new(),
        )));
        app_state().lock().unwrap().bound_process =
            Some(crate::process::BoundProcess::current_for_window_test());
        unsafe {
            let instance = GetModuleHandleW(None).unwrap();
            let class = WNDCLASSW {
                lpfnWndProc: Some(test_proc),
                hInstance: instance.into(),
                lpszClassName: w!("SlackInput.NativeTest"),
                ..Default::default()
            };
            assert_ne!(RegisterClassW(&class), 0);
            let main = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                w!("SlackInput.NativeTest"),
                w!("test main"),
                WS_OVERLAPPEDWINDOW,
                -20000,
                -20000,
                100,
                100,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .unwrap();
            let game = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                w!("SlackInput.NativeTest"),
                w!("test game"),
                WS_POPUP | WS_VISIBLE,
                -10000,
                -10000,
                320,
                240,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .unwrap();
            let desktop = Desktop::start(main.0 as isize, egui::Context::default()).unwrap();
            pump(350);
            let ring = FindWindowW(w!("SlackInput.FocusRing"), w!("SlackInput.FocusRing")).unwrap();
            assert!(IsWindowVisible(ring).as_bool());
            let style = GetWindowLongPtrW(ring, GWL_EXSTYLE) as u32;
            assert_ne!(style & WS_EX_TOOLWINDOW.0, 0);
            assert_ne!(style & WS_EX_NOACTIVATE.0, 0);
            assert_ne!(style & WS_EX_TRANSPARENT.0, 0);
            app_state().lock().unwrap().config.focus_locked = false;
            pump(200);
            assert_eq!(
                GetWindowLongPtrW(ring, GWL_EXSTYLE) as u32 & WS_EX_TRANSPARENT.0,
                0
            );
            assert_eq!(
                SendMessageW(ring, WM_NCHITTEST, None, None).0,
                HTCAPTION as isize
            );
            SendMessageW(ring, WM_ENTERSIZEMOVE, None, None);
            SetWindowPos(
                ring,
                None,
                -10000 + 80 - RING_SIZE / 2,
                -10000 + 180 - RING_SIZE / 2,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            )
            .unwrap();
            SendMessageW(ring, WM_EXITSIZEMOVE, None, None);
            {
                let state = app_state().lock().unwrap();
                assert_eq!((state.config.focus_x, state.config.focus_y), (0.25, 0.75));
            }
            app_state()
                .lock()
                .unwrap()
                .bound_process
                .as_mut()
                .unwrap()
                .paused = true;
            pump(200);
            assert!(!IsWindowVisible(ring).as_bool());
            app_state()
                .lock()
                .unwrap()
                .bound_process
                .as_mut()
                .unwrap()
                .paused = false;
            pump(200);
            assert!(IsWindowVisible(ring).as_bool());
            let _ = ShowWindow(main, SW_SHOWMINIMIZED);
            pump(350);
            assert!(
                !IsWindowVisible(main).as_bool(),
                "minimized main window must leave taskbar"
            );
            PostMessageW(
                Some(HWND(CONTROLLER.load(Ordering::Relaxed) as *mut _)),
                TRAY_MESSAGE,
                WPARAM(1),
                LPARAM(WM_LBUTTONUP as isize),
            )
            .unwrap();
            pump(350);
            assert!(
                IsWindowVisible(main).as_bool() && !IsIconic(main).as_bool(),
                "tray click restores main window"
            );
            app_state().lock().unwrap().bound_process = None;
            pump(200);
            assert!(!IsWindowVisible(ring).as_bool());
            drop(desktop);
            assert!(!IsWindow(Some(ring)).as_bool());
            DestroyWindow(game).unwrap();
            DestroyWindow(main).unwrap();
        }
    }
    #[test]
    fn ring_requires_enabled_live_unpaused_binding() {
        assert!(should_show(true, true, false, false));
        assert!(!should_show(false, true, false, false));
        assert!(!should_show(true, false, false, false));
        assert!(!should_show(true, true, true, false));
        assert!(!should_show(true, true, false, true));
    }
}
