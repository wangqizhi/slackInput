use std::cell::Cell;

use eframe::egui;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{HTCAPTION, HTCLIENT, WM_NCHITTEST};
use windows::core::Result;

const SUBCLASS_ID: usize = 0x534c4143;

/// Installed, updated and removed on the eframe window's thread.
pub struct Titlebar {
    hwnd: HWND,
    region: Box<Cell<RECT>>,
}

impl Titlebar {
    pub fn install(hwnd: HWND) -> Result<Self> {
        let region = Box::new(Cell::new(RECT::default()));
        unsafe {
            SetWindowSubclass(
                hwnd,
                Some(window_proc),
                SUBCLASS_ID,
                region.as_ref() as *const Cell<RECT> as usize,
            )
            .ok()?;
        }
        Ok(Self { hwnd, region })
    }

    pub fn update(&self, rect: egui::Rect, pixels_per_point: f32) {
        self.region.set(RECT {
            left: (rect.left() * pixels_per_point).round() as i32,
            top: (rect.top() * pixels_per_point).round() as i32,
            right: (rect.right() * pixels_per_point).round() as i32,
            bottom: (rect.bottom() * pixels_per_point).round() as i32,
        });
    }
}

impl Drop for Titlebar {
    fn drop(&mut self) {
        // Remove the callback before releasing its stable heap allocation.
        let _ = unsafe { RemoveWindowSubclass(self.hwnd, Some(window_proc), SUBCLASS_ID) };
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _id: usize,
    data: usize,
) -> LRESULT {
    let result = unsafe { DefSubclassProc(hwnd, msg, wp, lp) };
    if msg == WM_NCHITTEST && result.0 == HTCLIENT as isize {
        // Signed screen coordinates also support monitors left/above the primary.
        let mut point = POINT {
            x: lp.0 as i16 as i32,
            y: (lp.0 >> 16) as i16 as i32,
        };
        if unsafe { ScreenToClient(hwnd, &mut point) }.as_bool() {
            let rect = unsafe { &*(data as *const Cell<RECT>) }.get();
            if point.x >= rect.left
                && point.x < rect.right
                && point.y >= rect.top
                && point.y < rect.bottom
            {
                return LRESULT(HTCAPTION as isize);
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::Gdi::ClientToScreen;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::w;

    unsafe extern "system" fn test_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
    }

    #[test]
    fn native_titlebar_hit_test_excludes_buttons_and_scales() {
        unsafe {
            let instance = GetModuleHandleW(None).unwrap();
            let class = WNDCLASSW {
                lpfnWndProc: Some(test_proc),
                hInstance: instance.into(),
                lpszClassName: w!("SlackInput.TitlebarTest"),
                ..Default::default()
            };
            assert_ne!(RegisterClassW(&class), 0);
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                w!("SlackInput.TitlebarTest"),
                w!("titlebar test"),
                WS_POPUP,
                -2000,
                -2000,
                1000,
                800,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .unwrap();
            let titlebar = Titlebar::install(hwnd).unwrap();
            let hit = |x, y| {
                let mut point = POINT { x, y };
                assert!(ClientToScreen(hwnd, &mut point).as_bool());
                let lp = LPARAM(((point.y as u16 as u32) << 16 | point.x as u16 as u32) as isize);
                SendMessageW(hwnd, WM_NCHITTEST, None, Some(lp)).0
            };
            let rect = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(600.0, 80.0));
            titlebar.update(rect, 1.0);
            assert_eq!(hit(30, 30), HTCAPTION as isize); // icon
            assert_eq!(hit(450, 30), HTCAPTION as isize); // empty header
            assert_eq!(hit(620, 30), HTCLIENT as isize); // window buttons
            assert_eq!(hit(100, 100), HTCLIENT as isize); // settings
            titlebar.update(rect, 1.5);
            assert_eq!(hit(850, 110), HTCAPTION as isize);
            assert_eq!(hit(920, 30), HTCLIENT as isize);
            drop(titlebar);
            assert_eq!(hit(30, 30), HTCLIENT as isize);
            DestroyWindow(hwnd).unwrap();
        }
    }
}
