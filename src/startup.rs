//! The current user's Run entry is the single source of truth for both controls.
use std::os::windows::ffi::OsStrExt;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND};
use windows::Win32::System::Registry::*;
use windows::core::{PCWSTR, w};

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE: PCWSTR = w!("SlackInput");

pub fn enabled() -> Result<bool, String> {
    read(RUN_KEY)
}

fn read(key: PCWSTR) -> Result<bool, String> {
    let mut size = 0;
    let result = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key,
            VALUE,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut size),
        )
    };
    if result == ERROR_FILE_NOT_FOUND || result == ERROR_PATH_NOT_FOUND {
        return Ok(false);
    }
    result.ok().map_err(|error| error.to_string())?;
    Ok(size > 2)
}

fn write(key: PCWSTR, enabled: bool) -> Result<(), String> {
    let result = if enabled {
        let exe = std::env::current_exe().map_err(|error| error.to_string())?;
        // Quote the executable path, including paths containing spaces or Unicode.
        let command: Vec<u16> = std::iter::once(b'"' as u16)
            .chain(exe.as_os_str().encode_wide())
            .chain([b'"' as u16, 0])
            .collect();
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key,
                VALUE,
                REG_SZ.0,
                Some(command.as_ptr().cast()),
                (command.len() * 2) as u32,
            )
        }
    } else {
        unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key, VALUE) }
    };
    if !enabled && (result == ERROR_FILE_NOT_FOUND || result == ERROR_PATH_NOT_FOUND) {
        return Ok(());
    }
    result.ok().map_err(|error| error.to_string())
}

pub fn set_enabled(enabled: bool) -> bool {
    // Screenshots and tests must never modify the real login startup entry.
    if cfg!(test) || std::env::var_os("SLACKINPUT_UI_SNAPSHOT").is_some() {
        return true;
    }
    let language = crate::current_language();
    match write(RUN_KEY, enabled) {
        Ok(()) => true,
        Err(error) => {
            crate::set_status(&format!(
                "{}: {error}",
                crate::game_text(
                    language,
                    "Failed to update startup setting",
                    "开机自启动设置失败"
                )
            ));
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_by_default_and_toggle_round_trips() {
        // Exercise the actual registry APIs outside the Windows startup location.
        let path: Vec<u16> = format!("Software\\SlackInput-StartupTest-{}\0", std::process::id())
            .encode_utf16()
            .collect();
        let key = PCWSTR(path.as_ptr());
        struct Cleanup(PCWSTR);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                unsafe {
                    let _ = RegDeleteTreeW(HKEY_CURRENT_USER, self.0);
                }
            }
        }
        let _cleanup = Cleanup(key);
        assert!(!read(key).unwrap());
        write(key, false).unwrap();
        write(key, true).unwrap();
        assert!(read(key).unwrap());
        write(key, false).unwrap();
        assert!(!read(key).unwrap());
        write(key, false).unwrap();
    }
}
