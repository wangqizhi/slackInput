use windows::Win32::Foundation::{CloseHandle, HANDLE, NTSTATUS, WAIT_OBJECT_0};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_SUSPEND_RESUME, PROCESS_SYNCHRONIZE, WaitForSingleObject,
};
use windows::core::Result;

// Native API declarations: https://github.com/winsiderss/phnt/blob/master/ntpsapi.h
#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtSuspendProcess(process: HANDLE) -> NTSTATUS;
    fn NtResumeProcess(process: HANDLE) -> NTSTATUS;
}

struct OwnedHandle(HANDLE);

// Kernel process handles can be used across threads. The owner closes the handle
// only after exclusive access; BoundProcess is protected by the application's mutex.
unsafe impl Send for OwnedHandle {}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

pub struct BoundProcess {
    pub speed: Option<crate::trainer::SpeedSession>,
    handle: OwnedHandle,
    pub pid: u32,
    pub name: String,
    pub paused: bool,
}

impl BoundProcess {
    pub fn trainer_target(
        &self,
        generation: u64,
    ) -> std::result::Result<crate::trainer::Target, String> {
        Ok(crate::trainer::Target {
            pid: self.pid,
            name: self.name.clone(),
            generation,
            created: crate::trainer::creation_time(self.handle.0)?,
        })
    }
    #[cfg(test)]
    pub fn current_for_window_test() -> Self {
        Self {
            handle: OwnedHandle(
                unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, std::process::id()) }.unwrap(),
            ),
            pid: std::process::id(),
            name: "Window test".into(),
            speed: None,
            paused: false,
        }
    }

    pub fn exited(&self) -> bool {
        unsafe { WaitForSingleObject(self.handle.0, 0) == WAIT_OBJECT_0 }
    }

    pub fn suspend(&mut self) -> Result<()> {
        if !self.paused && !self.exited() {
            unsafe { NtSuspendProcess(self.handle.0).ok()? };
            self.paused = true;
        }
        Ok(())
    }

    pub fn resume(&mut self) -> Result<()> {
        if self.paused {
            if !self.exited() {
                unsafe { NtResumeProcess(self.handle.0).ok()? };
            }
            self.paused = false;
        }
        Ok(())
    }
}

impl BoundProcess {
    pub fn restore(&mut self) -> Result<()> {
        let speed_result = self.speed.as_mut().map(|speed| {
            speed.set(1).map_err(|e| windows::core::Error::new(
                windows::core::HRESULT(0x80004005u32 as i32), e))
        }).transpose();
        let resume_result = self.resume();
        speed_result.and(resume_result)
    }
}

impl Drop for BoundProcess {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

pub fn enumerate() -> Result<Vec<BoundProcess>> {
    let snapshot = OwnedHandle(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)? });
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    unsafe { Process32FirstW(snapshot.0, &mut entry)? };
    let mut processes = Vec::new();
    loop {
        if entry.th32ProcessID > 4 && entry.th32ProcessID != std::process::id() {
            // Keep the opened handle from selection through resume, so PID reuse
            // cannot redirect an action to another process after the game exits.
            if let Ok(handle) = unsafe {
                OpenProcess(
                    PROCESS_SUSPEND_RESUME
                        | PROCESS_SYNCHRONIZE
                        | windows::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION,
                    false,
                    entry.th32ProcessID,
                )
            } {
                let end = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                processes.push(BoundProcess {
                    handle: OwnedHandle(handle),
                    pid: entry.th32ProcessID,
                    name: String::from_utf16_lossy(&entry.szExeFile[..end]),
                    speed: None,
                    paused: false,
                });
            }
        }
        if let Err(error) = unsafe { Process32NextW(snapshot.0, &mut entry) } {
            if error.code() != windows::Win32::Foundation::ERROR_NO_MORE_FILES.to_hresult() {
                return Err(error);
            }
            break;
        }
    }
    processes.sort_by_cached_key(|p| (p.name.to_lowercase(), p.pid));
    Ok(processes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    struct TestChild(Child);
    impl Drop for TestChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn repeated_suspend_needs_only_one_resume_and_exit_is_safe() {
        let mut child = TestChild(
            Command::new("powershell.exe")
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "Start-Sleep -Milliseconds 800",
                ])
                .creation_flags(0x08000000) // CREATE_NO_WINDOW
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("start disposable test process"),
        );
        let handle = unsafe {
            OpenProcess(
                PROCESS_SUSPEND_RESUME | PROCESS_SYNCHRONIZE,
                false,
                child.0.id(),
            )
        }
        .unwrap();
        let mut process = BoundProcess {
            handle: OwnedHandle(handle),
            pid: child.0.id(),
            name: "test".into(),
            speed: None,
            paused: false,
        };
        process.suspend().unwrap();
        process.suspend().unwrap();
        std::thread::sleep(Duration::from_millis(1800));
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "suspended child must not finish"
        );
        assert!(process.paused);
        process.resume().unwrap();
        process.resume().unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while child.0.try_wait().unwrap().is_none() {
            assert!(
                Instant::now() < deadline,
                "single resume must allow child to finish"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(process.exited());
        process.suspend().unwrap();
        assert!(!process.paused);
        process.resume().unwrap();
    }
}

/// Largest visible, unowned top-level window belonging to the bound process.
/// Kept separate from the overlay lookup, which intentionally excludes minimized windows.
pub fn foreground_pid() -> u32 {
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut pid));
    }
    pid
}
impl BoundProcess {
    pub fn activate_window(&self) -> std::result::Result<(), String> {
        use windows::Win32::{
            Foundation::{HWND, LPARAM, RECT},
            UI::WindowsAndMessaging::*,
        };
        use windows::core::BOOL;
        if self.exited() {
            return Err("游戏已退出 / Game exited".into());
        }
        if self.paused {
            return Err("游戏已暂停，请先在游戏页恢复 / Resume the game first".into());
        }
        struct Search {
            pid: u32,
            hwnd: HWND,
            area: i64,
        }
        unsafe extern "system" fn visit(hwnd: HWND, lp: LPARAM) -> BOOL {
            let search = unsafe { &mut *(lp.0 as *mut Search) };
            let mut pid = 0;
            unsafe {
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
            }
            if pid == search.pid
                && unsafe { IsWindowVisible(hwnd).as_bool() }
                && unsafe { GetWindow(hwnd, GW_OWNER) }.map_or(true, |owner| owner.0.is_null())
            {
                let mut rect = RECT::default();
                if unsafe { GetWindowRect(hwnd, &mut rect) }.is_ok() {
                    let area =
                        i64::from(rect.right - rect.left) * i64::from(rect.bottom - rect.top);
                    if area > search.area {
                        search.hwnd = hwnd;
                        search.area = area;
                    }
                }
            }
            BOOL(1)
        }
        let mut search = Search {
            pid: self.pid,
            hwnd: HWND::default(),
            area: 0,
        };
        unsafe { EnumWindows(Some(visit), LPARAM(&mut search as *mut _ as isize)) }
            .map_err(|e| e.to_string())?;
        if search.hwnd.0.is_null() {
            return Err("找不到游戏窗口 / No game window found".into());
        }
        if self.exited() {
            return Err("游戏已退出 / Game exited".into());
        }
        unsafe {
            if IsIconic(search.hwnd).as_bool() {
                let _ = ShowWindowAsync(search.hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(search.hwnd);
        }
        // Activation lands asynchronously: ShowWindowAsync only posts the restore, and
        // SetForegroundWindow reaches a busy game through its message pump, so one
        // immediate check reports failure while the switch is still in flight. Poll
        // instead, re-requesting in case the restore consumed the first activation.
        use std::time::{Duration, Instant};
        let deadline = Instant::now() + Duration::from_millis(1500);
        loop {
            if foreground_pid() == self.pid {
                return Ok(());
            }
            if self.exited() {
                return Err("游戏已退出 / Game exited".into());
            }
            if Instant::now() >= deadline {
                return Err("Windows 未允许激活游戏窗口，请切回游戏后使用触发键 / Windows denied game focus; use the hotkey in game".into());
            }
            std::thread::sleep(Duration::from_millis(20));
            unsafe {
                let _ = SetForegroundWindow(search.hwnd);
            }
        }
    }
}

#[cfg(test)]
mod activation_tests {
    use super::*;
    #[test]
    fn refuses_paused_and_windowless_targets() {
        let mut target = BoundProcess::current_for_window_test();
        target.paused = true;
        assert!(
            target
                .activate_window()
                .unwrap_err()
                .contains("Resume the game")
        );
        target.paused = false;
        target.pid = u32::MAX;
        assert!(
            target
                .activate_window()
                .unwrap_err()
                .contains("No game window")
        );
    }
}
