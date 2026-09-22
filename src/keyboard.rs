use std::sync::{OnceLock, mpsc};
use std::time::{Duration, Instant};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey,
};
use windows::Win32::UI::WindowsAndMessaging::{MSG, PM_REMOVE, PeekMessageW, WM_HOTKEY};

type Hotkey = Option<(u32, u32)>;
type Request = (Vec<Hotkey>, mpsc::Sender<Result<(), String>>);
static REQUESTS: OnceLock<mpsc::Sender<Request>> = OnceLock::new();

pub fn configure_all(keys: [Hotkey; 3]) -> Result<(), String> {
    request(keys.to_vec())
}

fn unregister(keys: &[Hotkey; 3]) {
    for (i, key) in keys.iter().enumerate() {
        if key.is_some() {
            let _ = unsafe { UnregisterHotKey(None, i as i32 + 1) };
        }
    }
}
fn register(keys: &[Hotkey; 3]) -> Result<(), String> {
    for (i, key) in keys.iter().enumerate() {
        if let Some((modifiers, key)) = key {
            if let Err(e) = unsafe {
                RegisterHotKey(
                    None,
                    i as i32 + 1,
                    HOT_KEY_MODIFIERS(*modifiers) | MOD_NOREPEAT,
                    *key,
                )
            } {
                for j in 0..i {
                    if keys[j].is_some() {
                        let _ = unsafe { UnregisterHotKey(None, j as i32 + 1) };
                    }
                }
                return Err(e.to_string());
            }
        }
    }
    Ok(())
}

// All registration and dispatch stays on this thread; failed edits restore old keys.
fn request(next: Vec<Hotkey>) -> Result<(), String> {
    let requests = REQUESTS.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Request>();
        std::thread::spawn(move || {
            let mut current = [None; 3];
            let mut pending = [false; 3];
            let mut targets = [None; 3];
            let mut last = [Instant::now() - Duration::from_secs(1); 3];
            loop {
                match rx.recv_timeout(Duration::from_millis(20)) {
                    Ok((next, reply)) => {
                        let mut desired = current;
                        desired[..next.len()].copy_from_slice(&next);
                        let result = if desired == current {
                            Ok(())
                        } else {
                            pending = [false; 3];
                            unregister(&current);
                            match register(&desired) {
                                Ok(()) => {
                                    current = desired;
                                    Ok(())
                                }
                                Err(error) => match register(&current) {
                                    Ok(()) => Err(error),
                                    Err(restore) => {
                                        current = [None; 3];
                                        Err(format!(
                                            "{error}; 恢复旧快捷键失败 / Restore failed: {restore}"
                                        ))
                                    }
                                },
                            }
                        };
                        let _ = reply.send(result);
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        unregister(&current);
                        break;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                for i in 0..3 {
                    if pending[i]
                        && current[i]
                            .is_some_and(|(mods, key)| crate::keyboard_trigger_released(mods, key))
                    {
                        pending[i] = false;
                        last[i] = Instant::now();
                        if i == 0 {
                            if let Err(e) = crate::trigger_mapping("Keyboard") {
                                crate::report_process_error(crate::current_language(), e);
                            }
                        } else if targets[i].is_some()
                            && targets[i] == crate::speed_shortcut_target()
                        {
                            crate::change_game_speed(None, i == 1, targets[i]);
                        }
                    }
                }
                let mut msg = MSG::default();
                while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() } {
                    if msg.message == WM_HOTKEY {
                        let i = msg.wParam.0.wrapping_sub(1);
                        if i < 3
                            && current[i].is_some()
                            && last[i].elapsed() >= crate::TRIGGER_COOLDOWN
                        {
                            targets[i] = crate::speed_shortcut_target();
                            pending[i] = i == 0 || targets[i].is_some();
                        }
                    }
                }
            }
        });
        tx
    });
    let (tx, rx) = mpsc::channel();
    requests.send((next, tx)).map_err(|e| e.to_string())?;
    rx.recv().map_err(|e| e.to_string())?
}
