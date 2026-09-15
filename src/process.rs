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
    handle: OwnedHandle,
    pub pid: u32,
    pub name: String,
    pub paused: bool,
}

impl BoundProcess {
    #[cfg(test)]
    pub fn current_for_window_test() -> Self {
        Self {
            handle: OwnedHandle(
                unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, std::process::id()) }.unwrap(),
            ),
            pid: std::process::id(),
            name: "Window test".into(),
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

impl Drop for BoundProcess {
    fn drop(&mut self) {
        let _ = self.resume();
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
                    PROCESS_SUSPEND_RESUME | PROCESS_SYNCHRONIZE,
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
