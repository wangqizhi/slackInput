//! Explicit, temporary global recording. Raw events never leave memory.
use super::Step;
use std::{
    cell::RefCell,
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use windows::Win32::{
    Foundation::{LPARAM, LRESULT, WPARAM},
    System::LibraryLoader::GetModuleHandleW,
    UI::WindowsAndMessaging::*,
};

#[derive(Clone, Copy)]
struct Event {
    key: u32,
    down: bool,
    at: Instant,
}
type Sink = (mpsc::SyncSender<Event>, Arc<AtomicBool>, Arc<AtomicBool>);
thread_local! { static SINK: RefCell<Option<Sink>> = const { RefCell::new(None) }; }
unsafe extern "system" fn hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == 0 {
        let key = unsafe { &*(lp.0 as *const KBDLLHOOKSTRUCT) };
        if !key.flags.contains(LLKHF_INJECTED) {
            let down = matches!(wp.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
            SINK.with(|sink| {
                if let Some((tx, overflow, stop)) = sink.borrow().as_ref() {
                    if key.vkCode == 0x7B && !down {
                        stop.store(true, Ordering::SeqCst);
                    }
                    if tx
                        .try_send(Event {
                            key: key.vkCode,
                            down,
                            at: Instant::now(),
                        })
                        .is_err()
                    {
                        overflow.store(true, Ordering::SeqCst);
                    }
                }
            });
            // Reserve F12 for stopping; do not send it to the foreground game.
            if key.vkCode == 0x7B {
                return LRESULT(1);
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wp, lp) }
}
fn modifier(key: u32) -> Option<&'static str> {
    match key {
        0x10 | 0xA0 | 0xA1 => Some("Shift"),
        0x11 | 0xA2 | 0xA3 => Some("Ctrl"),
        0x12 | 0xA4 | 0xA5 => Some("Alt"),
        0x5B | 0x5C => Some("Win"),
        _ => None,
    }
}
fn key_name(key: u32) -> Option<String> {
    if (0x30..=0x39).contains(&key) || (0x41..=0x5A).contains(&key) {
        return Some(char::from_u32(key)?.to_string());
    }
    if (0x70..=0x87).contains(&key) {
        return Some(format!("F{}", key - 0x6F));
    }
    [
        "Left",
        "Right",
        "Up",
        "Down",
        "Tab",
        "Esc",
        "Enter",
        "Space",
        "Backspace",
        "Insert",
        "Delete",
        "Home",
        "End",
        "PageUp",
        "PageDown",
    ]
    .into_iter()
    .find(|name| crate::parse_token(name).is_some_and(|k| u32::from(k.0) == key))
    .map(str::to_string)
}
#[derive(Default)]
struct Builder {
    pressed: HashSet<u32>,
    current: Option<(u32, String, Instant)>,
    last_up: Option<Instant>,
    steps: Vec<Step>,
    error: Option<String>,
}
impl Builder {
    fn event(&mut self, e: Event) {
        if self.error.is_some() {
            return;
        }
        if e.down {
            if !self.pressed.insert(e.key) {
                return;
            }
        } else {
            self.pressed.remove(&e.key);
        }
        if modifier(e.key).is_some() {
            if self.current.is_some() {
                self.error=Some("组合键中途改变修饰键，请先按修饰键，最后松开 / Modifier changed during key hold".into());
            }
            return;
        }
        if e.down {
            if self.current.is_some() {
                self.error=Some("暂不支持多个普通键重叠按住，请逐步录制 / Overlapping ordinary keys are unsupported".into());
                return;
            }
            let Some(name) = key_name(e.key) else {
                self.error = Some(format!("不支持的按键 / Unsupported key: {:#x}", e.key));
                return;
            };
            if self.steps.len() >= 128 {
                self.error = Some("最多录制 128 步 / Maximum 128 steps".into());
                return;
            }
            if let Some(up) = self.last_up {
                let delay = e.at.saturating_duration_since(up).as_millis() as u64;
                if delay > 60000 {
                    self.error = Some("步骤间隔超过 60 秒 / Gap exceeds 60 seconds".into());
                    return;
                }
                self.steps.last_mut().unwrap().delay_ms = delay;
            }
            let mut names: Vec<String> = ["Ctrl", "Alt", "Shift", "Win"]
                .into_iter()
                .filter(|m| self.pressed.iter().any(|k| modifier(*k) == Some(*m)))
                .map(str::to_string)
                .collect();
            names.push(name);
            self.current = Some((e.key, names.join("+"), e.at));
        } else if self
            .current
            .as_ref()
            .is_some_and(|(key, _, _)| *key == e.key)
        {
            let (_, keys, down) = self.current.take().unwrap();
            let hold_ms = (e.at.saturating_duration_since(down).as_millis() as u64).max(1);
            if hold_ms > 60000 {
                self.error = Some("按住超过 60 秒 / Hold exceeds 60 seconds".into());
                return;
            }
            self.steps.push(Step {
                keys,
                hold_ms,
                delay_ms: 0,
            });
            self.last_up = Some(e.at);
        }
    }
    fn finish(self) -> Result<Vec<Step>, String> {
        if let Some(e) = self.error {
            return Err(e);
        }
        if self.current.is_some() {
            return Err("请松开全部按键后结束录制 / Release keys before stopping".into());
        }
        if self.steps.is_empty() {
            return Err("未录制到按键 / No keys recorded".into());
        }
        Ok(self.steps)
    }
}
pub struct Recording {
    stop: Arc<AtomicBool>,
    overflow: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    rx: mpsc::Receiver<Event>,
    builder: Builder,
    ready: Instant,
    done: bool,
}
impl Recording {
    pub fn start() -> Result<Self, String> {
        let (tx, rx) = mpsc::sync_channel(1024);
        let (ready_tx, ready_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let signal = stop.clone();
        let overflow = Arc::new(AtomicBool::new(false));
        let full = overflow.clone();
        let thread = std::thread::spawn(move || {
            SINK.with(|sink| *sink.borrow_mut() = Some((tx, full.clone(), signal.clone())));
            let result = unsafe {
                GetModuleHandleW(None).and_then(|module| {
                    SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), Some(module.into()), 0)
                })
            };
            match result {
                Ok(handle) => {
                    let _ = ready_tx.send(Ok(()));
                    let mut msg = MSG::default();
                    let started = Instant::now();
                    while !signal.load(Ordering::SeqCst) {
                        let permitted = {
                            let state = crate::app_state().lock().unwrap();
                            super::permitted(&state)
                        };
                        if !permitted
                            || full.load(Ordering::SeqCst)
                            || started.elapsed() > Duration::from_secs(303)
                        {
                            full.store(true, Ordering::SeqCst);
                            break;
                        }
                        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() } {
                            unsafe {
                                let _ = TranslateMessage(&msg);
                                DispatchMessageW(&msg);
                            }
                        }
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    let _ = unsafe { UnhookWindowsHookEx(handle) };
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e.to_string()));
                }
            }
            SINK.with(|sink| *sink.borrow_mut() = None);
        });
        ready_rx.recv().map_err(|e| e.to_string())??;
        Ok(Self {
            stop,
            overflow,
            thread: Some(thread),
            rx,
            builder: Builder::default(),
            ready: Instant::now() + Duration::from_secs(3),
            done: false,
        })
    }
    pub fn poll(&mut self) -> bool {
        for e in self.rx.try_iter() {
            if e.key == 0x7B {
                if !e.down {
                    self.done = true;
                }
                continue;
            }
            if e.at >= self.ready && !self.done {
                self.builder.event(e);
            }
        }
        if self.overflow.load(Ordering::SeqCst) {
            self.builder.error = Some("录制已中断：权限关闭、事件溢出或超时 / Recording interrupted: disabled, overflow or timeout".into());
        }
        if Instant::now().saturating_duration_since(self.ready) > Duration::from_secs(300) {
            self.builder.error = Some("录制超过 5 分钟 / Recording exceeded 5 minutes".into());
        }
        self.done || self.builder.error.is_some()
    }
    pub fn countdown(&self) -> u64 {
        self.ready
            .saturating_duration_since(Instant::now())
            .as_secs()
            + 1
    }
    pub fn armed(&self) -> bool {
        Instant::now() >= self.ready
    }
    pub fn finish(mut self) -> Result<Vec<Step>, String> {
        self.shutdown();
        self.poll();
        std::mem::take(&mut self.builder).finish()
    }
    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
impl Drop for Recording {
    fn drop(&mut self) {
        self.shutdown();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn records_chords_repeat_hold_and_gaps() {
        let start = Instant::now();
        let mut b = Builder::default();
        for (key, down, ms) in [
            (0xA2, true, 0),
            (0x51, true, 10),
            (0x51, true, 20),
            (0x51, false, 110),
            (0xA2, false, 120),
            (0x20, true, 310),
            (0x20, false, 360),
        ] {
            b.event(Event {
                key,
                down,
                at: start + Duration::from_millis(ms),
            });
        }
        assert_eq!(
            b.finish().unwrap(),
            vec![
                Step {
                    keys: "Ctrl+Q".into(),
                    hold_ms: 100,
                    delay_ms: 200
                },
                Step {
                    keys: "Space".into(),
                    hold_ms: 50,
                    delay_ms: 0
                }
            ]
        );
    }
    #[test]
    fn rejects_overlap_and_unreleased_keys() {
        let mut b = Builder::default();
        let at = Instant::now();
        b.event(Event {
            key: 65,
            down: true,
            at,
        });
        b.event(Event {
            key: 66,
            down: true,
            at,
        });
        assert!(b.finish().is_err());
        let mut b = Builder::default();
        b.event(Event {
            key: 65,
            down: true,
            at,
        });
        assert!(b.finish().is_err());
    }
    #[test]
    fn rejects_unsupported_and_long_holds() {
        let mut b = Builder::default();
        let at = Instant::now();
        b.event(Event {
            key: 0xBA,
            down: true,
            at,
        });
        assert!(b.finish().is_err());
        let mut b = Builder::default();
        b.event(Event {
            key: 65,
            down: true,
            at,
        });
        b.event(Event {
            key: 65,
            down: false,
            at: at + Duration::from_secs(61),
        });
        assert!(b.finish().is_err());
    }
}
