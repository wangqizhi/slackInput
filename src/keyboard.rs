use std::sync::{OnceLock, mpsc};
use std::time::Duration;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey,
};
use windows::Win32::UI::WindowsAndMessaging::{MSG, PM_REMOVE, PeekMessageW, WM_HOTKEY};

type Request = (Option<(u32, u32)>, mpsc::Sender<Result<(), String>>);
static REQUESTS: OnceLock<mpsc::Sender<Request>> = OnceLock::new();

/// Registration and message dispatch must happen on the same thread.
pub fn configure(hotkey: Option<(u32, u32)>) -> Result<(), String> {
    let requests = REQUESTS.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Request>();
        std::thread::spawn(move || {
            let mut current = None;
            let mut id = 1;
            let mut pending = false;
            let mut last_trigger = std::time::Instant::now() - Duration::from_secs(1);
            loop {
                match rx.recv_timeout(Duration::from_millis(20)) {
                    Ok((next, reply)) => {
                        let result = if next == current {
                            Ok(())
                        } else {
                            let next_id = 3 - id;
                            let registered = match next {
                                Some((modifiers, key)) => unsafe {
                                    RegisterHotKey(None, next_id, HOT_KEY_MODIFIERS(modifiers) | MOD_NOREPEAT, key)
                                        .map_err(|e| e.to_string())
                                },
                                None => Ok(()),
                            };
                            if registered.is_ok() {
                                pending = false;
                                if current.is_some() {
                                    let _ = unsafe { UnregisterHotKey(None, id) };
                                }
                                current = next;
                                id = next_id;
                            }
                            registered
                        };
                        let _ = reply.send(result);
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                if pending && current.is_some_and(|(modifiers, key)| crate::keyboard_trigger_released(modifiers, key)) {
                    pending = false;
                    last_trigger = std::time::Instant::now();
                    if let Err(error) = crate::trigger_mapping("Keyboard") {
                        crate::report_process_error(crate::current_language(), error);
                    }
                }
                let mut msg = MSG::default();
                while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() } {
                    if msg.message == WM_HOTKEY && msg.wParam.0 == id as usize && current.is_some()
                        && last_trigger.elapsed() >= crate::TRIGGER_COOLDOWN {
                        // Let the physical modifiers go before injecting the mapped shortcut.
                        pending = true;
                    }
                }
            }
        });
        tx
    });
    let (tx, rx) = mpsc::channel();
    requests.send((hotkey, tx)).map_err(|e| e.to_string())?;
    rx.recv().map_err(|e| e.to_string())?
}
