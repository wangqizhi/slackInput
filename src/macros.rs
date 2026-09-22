#[path = "macro_recording.rs"]
mod recording;
static RECORDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub fn is_recording() -> bool {
    RECORDING.load(Ordering::SeqCst)
}
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use windows::Win32::UI::{
    Input::KeyboardAndMouse::{
        HOT_KEY_MODIFIERS, INPUT, MOD_NOREPEAT, RegisterHotKey, SendInput, UnregisterHotKey,
    },
    WindowsAndMessaging::{MSG, PM_REMOVE, PeekMessageW, WM_HOTKEY},
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub keys: String,
    pub hold_ms: u64,
    pub delay_ms: u64,
}
impl Default for Step {
    fn default() -> Self {
        Self {
            keys: "Space".into(),
            hold_ms: 40,
            delay_ms: 100,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Macro {
    pub name: String,
    pub enabled: bool,
    pub trigger: String,
    pub repeats: u32,
    pub steps: Vec<Step>,
}
impl Default for Macro {
    fn default() -> Self {
        Self {
            name: "New macro".into(),
            enabled: false,
            trigger: "F8".into(),
            repeats: 1,
            steps: vec![Step::default()],
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct File {
    version: u32,
    macros: Vec<Macro>,
}
fn validate(macros: &[Macro]) -> Result<(), String> {
    if macros.len() > 64 {
        return Err("最多 64 个宏 / Maximum 64 macros".into());
    }
    let mut triggers = HashSet::new();
    for m in macros {
        let trigger = crate::parse_keyboard_trigger(&m.trigger)
            .flatten()
            .ok_or_else(|| format!("{}: 无效触发键 / Invalid trigger", m.name))?;
        if m.enabled && !triggers.insert(trigger) {
            return Err("触发键重复 / Duplicate trigger".into());
        }
        if m.name.trim().is_empty()
            || !(1..=10000).contains(&m.repeats)
            || m.steps.is_empty()
            || m.steps.len() > 128
        {
            return Err("名称、次数或步骤无效 / Invalid name, repeats or steps".into());
        }
        for s in &m.steps {
            if crate::parse_keyboard_trigger(&s.keys).flatten().is_none()
                || !(1..=60000).contains(&s.hold_ms)
                || s.delay_ms > 60000
            {
                return Err(format!(
                    "{}: 按键或时间无效 / Invalid keys or timing",
                    m.name
                ));
            }
        }
    }
    for m in macros.iter().filter(|m| m.enabled) {
        for step in &m.steps {
            if triggers.contains(&crate::parse_keyboard_trigger(&step.keys).flatten().unwrap()) {
                return Err(
                    "输出不能使用已启用宏的触发键 / Output conflicts with macro trigger".into(),
                );
            }
        }
    }
    Ok(())
}
fn allowed() -> bool {
    let s = crate::app_state().lock().unwrap();
    permitted(&s) && !is_recording()
}
fn permitted(state: &crate::AppState) -> bool {
    state.config.capture_enabled && !state.features_locked()
}
fn unregister(macros: &[Macro]) {
    for (i, m) in macros.iter().enumerate() {
        if m.enabled {
            let _ = unsafe { UnregisterHotKey(None, 100 + i as i32) };
        }
    }
}
fn register(macros: &[Macro]) -> Result<(), String> {
    for (i, m) in macros.iter().enumerate().filter(|(_, m)| m.enabled) {
        let (mods, key) = crate::parse_keyboard_trigger(&m.trigger).flatten().unwrap();
        if let Err(e) = unsafe {
            RegisterHotKey(
                None,
                100 + i as i32,
                HOT_KEY_MODIFIERS(mods) | MOD_NOREPEAT,
                key,
            )
        } {
            for (j, previous) in macros.iter().enumerate().take(i) {
                if previous.enabled {
                    let _ = unsafe { UnregisterHotKey(None, 100 + j as i32) };
                }
            }
            return Err(format!(
                "{}: {} ({e})",
                m.name, "热键被占用 / Hotkey unavailable"
            ));
        }
    }
    Ok(())
}
// Macro input includes physical scan codes for games that do not consume VK-only events.
fn macro_key_input(
    key: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY,
    up: bool,
) -> INPUT {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        KEYEVENTF_EXTENDEDKEY, KEYEVENTF_SCANCODE, MAPVK_VK_TO_VSC_EX, MapVirtualKeyW, VIRTUAL_KEY,
    };
    let scan = unsafe { MapVirtualKeyW(u32::from(key.0), MAPVK_VK_TO_VSC_EX) };
    let mut input = crate::key_input(key, up);
    if scan != 0 && scan >> 8 != 0xE1 {
        let keyboard = unsafe { &mut input.Anonymous.ki };
        keyboard.wVk = VIRTUAL_KEY(0);
        keyboard.wScan = (scan & 0xFF) as u16;
        keyboard.dwFlags |= KEYEVENTF_SCANCODE;
        // Some layouts return the navigation scan without its E0 prefix.
        if scan >> 8 == 0xE0 || matches!(key.0, 0x21..=0x28 | 0x2D | 0x2E | 0x5B | 0x5C) {
            keyboard.dwFlags |= KEYEVENTF_EXTENDEDKEY;
        }
    }
    input
}
fn execution_status(name: &str, en: &str, zh: &str) {
    let language = crate::current_language();
    let text = if language == crate::Language::Chinese {
        zh
    } else {
        en
    };
    crate::set_status(&format!("{text}: {name}"));
}
fn inject(keys: &[windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY], up: bool) -> bool {
    let inputs: Vec<_> = if up {
        keys.iter()
            .rev()
            .map(|&k| macro_key_input(k, true))
            .collect()
    } else {
        keys.iter().map(|&k| macro_key_input(k, false)).collect()
    };
    unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) as usize == inputs.len() }
}
fn wait(ms: u64, cancel: &AtomicU64, generation: u64) -> bool {
    let until = Instant::now() + Duration::from_millis(ms);
    loop {
        if cancel.load(Ordering::SeqCst) != generation || !allowed() {
            return false;
        }
        if Instant::now() >= until {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[derive(Default)]
struct Playback {
    running: AtomicBool,
    paused: AtomicBool,
    index: AtomicUsize,
    session: AtomicU64,
    target_pid: AtomicUsize,
    binding: AtomicU64,
}
impl Playback {
    fn begin(&self, index: usize) -> bool {
        if self
            .running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return false;
        }
        self.session.fetch_add(1, Ordering::SeqCst);
        self.target_pid.store(0, Ordering::SeqCst);
        self.index.store(index, Ordering::SeqCst);
        self.paused.store(false, Ordering::SeqCst);
        true
    }
    fn finish(&self) {
        self.session.fetch_add(1, Ordering::SeqCst);
        self.paused.store(false, Ordering::SeqCst);
        self.running.store(false, Ordering::SeqCst);
    }
}
fn focus_game(playback: &Playback, resume: bool) -> Result<(), String> {
    let state = crate::app_state().lock().unwrap();
    if !permitted(&state) || is_recording() {
        return Err("当前禁止执行宏 / Macro execution disabled".into());
    }
    let game = state
        .bound_process
        .as_ref()
        .ok_or("请先在游戏页绑定进程 / Bind a game first")?;
    let previous = playback.target_pid.load(Ordering::SeqCst) as u32;
    if resume
        && previous != 0
        && (game.pid != previous
            || state.binding_generation != playback.binding.load(Ordering::SeqCst))
    {
        return Err(
            "绑定游戏已改变，请停止后重新运行 / Game binding changed; stop and run again".into(),
        );
    }
    game.activate_window()?;
    playback
        .binding
        .store(state.binding_generation, Ordering::SeqCst);
    playback
        .target_pid
        .store(game.pid as usize, Ordering::SeqCst);
    Ok(())
}
fn target_valid(playback: &Playback) -> bool {
    let pid = playback.target_pid.load(Ordering::SeqCst) as u32;
    if pid == 0 {
        return true;
    }
    let state = crate::app_state().lock().unwrap();
    state.binding_generation == playback.binding.load(Ordering::SeqCst)
        && state
            .bound_process
            .as_ref()
            .is_some_and(|p| p.pid == pid && !p.exited() && !p.paused)
}
// Pausing releases the current chord; resume consumes only its remaining hold time.
fn phase(
    ms: u64,
    keys: &[windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY],
    cancel: &AtomicU64,
    generation: u64,
    playback: &Playback,
) -> Result<bool, ()> {
    let mut remaining = Duration::from_millis(ms);
    let mut held = false;
    let result = loop {
        if cancel.load(Ordering::SeqCst) != generation || !allowed() {
            break Ok(false);
        }
        if !target_valid(playback) {
            break Ok(false);
        }
        let pid = playback.target_pid.load(Ordering::SeqCst) as u32;
        if pid != 0
            && !playback.paused.load(Ordering::SeqCst)
            && crate::process::foreground_pid() != pid
        {
            playback.paused.store(true, Ordering::SeqCst);
            crate::set_status("游戏失去焦点，宏已暂停 / Game lost focus; macro paused");
        }
        if playback.paused.load(Ordering::SeqCst) {
            if held {
                held = false;
                if !inject(keys, true) {
                    break Err(());
                }
            }
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        if remaining.is_zero() {
            break Ok(true);
        }
        if !held && !keys.is_empty() {
            held = true;
            if !inject(keys, false) {
                break Err(());
            }
        }
        let start = Instant::now();
        std::thread::sleep(remaining.min(Duration::from_millis(5)));
        remaining = remaining.saturating_sub(start.elapsed());
    };
    if held && !inject(keys, true) {
        return Err(());
    }
    result
}
// A manual run must not fire into whatever window still holds focus while the game is
// still coming up; wait for the target to hold focus and settle before the first key.
// Never touches the paused flag: a focus flicker during warm-up must not wedge playback.
fn settle_focus(ms: u64, cancel: &AtomicU64, generation: u64, playback: &Playback) -> bool {
    let pid = playback.target_pid.load(Ordering::SeqCst) as u32;
    if pid == 0 {
        return false;
    }
    let deadline = Instant::now() + Duration::from_millis(ms);
    let mut focused_since: Option<Instant> = None;
    loop {
        if cancel.load(Ordering::SeqCst) != generation || !allowed() || !target_valid(playback) {
            return false;
        }
        if crate::process::foreground_pid() == pid {
            let since = focused_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= Duration::from_millis(300) {
                return true;
            }
        } else {
            focused_since = None;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn execute(m: &Macro, cancel: &AtomicU64, generation: u64, playback: &Playback, manual: bool) {
    let shortcut = crate::app_state()
        .lock()
        .unwrap()
        .config
        .keyboard_trigger
        .clone();
    if let Some(trigger) = crate::parse_keyboard_trigger(&shortcut).flatten() {
        if m.steps
            .iter()
            .any(|s| crate::parse_keyboard_trigger(&s.keys).flatten() == Some(trigger))
        {
            crate::set_status(
                "宏输出与快捷键触发冲突 / Macro output conflicts with shortcut trigger",
            );
            return;
        }
    }
    if manual {
        execution_status(
            &m.name,
            "Game activated; starting shortly",
            "游戏已激活，即将运行",
        );
        if !settle_focus(2000, cancel, generation, playback) {
            execution_status(&m.name, "Macro stopped", "宏已停止");
            return;
        }
    }
    execution_status(&m.name, "Running macro", "正在执行宏");
    for _ in 0..m.repeats {
        for s in &m.steps {
            if !wait(0, cancel, generation) {
                execution_status(&m.name, "Macro stopped", "宏已停止");
                return;
            }
            let keys = crate::parse_mapping(&s.keys).unwrap();
            for (ms, output) in [(s.hold_ms, keys.as_slice()), (s.delay_ms, &[][..])] {
                match phase(ms, output, cancel, generation, playback) {
                    Ok(true) => {}
                    Ok(false) => {
                        execution_status(&m.name, "Macro stopped", "宏已停止");
                        return;
                    }
                    Err(()) => {
                        crate::set_status(
                            "宏输入发送失败，请检查目标程序权限 / Macro SendInput failed; check target privileges",
                        );
                        return;
                    }
                }
            }
        }
    }
    execution_status(
        &m.name,
        "Macro input sent (game response not verified)",
        "宏输入已发送（不代表游戏已响应）",
    );
}
type Request = (Vec<Macro>, bool, mpsc::Sender<Result<(), String>>);
pub struct Editor {
    macros: Vec<Macro>,
    tx: mpsc::Sender<Request>,
    cancel: Arc<AtomicU64>,
    message: String,
    applied: Vec<Macro>,
    playback: Arc<Playback>,
    runs: mpsc::Sender<(Macro, u64)>,
    recording: Option<recording::Recording>,
}
impl Editor {
    pub fn new() -> Self {
        let path = crate::config_dir().join("macros.json");
        let loaded = match std::fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str::<File>(&s)
                .map_err(|e| e.to_string())
                .and_then(|f| {
                    if f.version != 1 {
                        return Err("Unsupported macro file version".into());
                    }
                    validate(&f.macros)?;
                    Ok(f.macros)
                }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
            Err(e) => Err(e.to_string()),
        };
        let (macros, message) = match loaded {
            Ok(m) => (m, String::new()),
            Err(e) => (vec![], format!("宏加载失败 / Load failed: {e}")),
        };
        let (tx, rx) = mpsc::channel::<Request>();
        let cancel = Arc::new(AtomicU64::new(0));
        let signal = cancel.clone();
        let playback = Arc::new(Playback::default());
        let playing = playback.clone();
        let (runs, run_rx) = mpsc::channel::<(Macro, u64)>();
        std::thread::spawn(move || {
            let mut active: Vec<Macro> = vec![];
            let mut pending = None;
            loop {
                match rx.try_recv() {
                    Ok((next, save, reply)) => {
                        pending = None;
                        unregister(&active);
                        let result = register(&next).and_then(|()| {
                            if save {
                                let file = File {
                                    version: 1,
                                    macros: next.clone(),
                                };
                                std::fs::create_dir_all(path.parent().unwrap())
                                    .map_err(|e| e.to_string())?;
                                let temp = path.with_extension("json.tmp");
                                std::fs::write(&temp, serde_json::to_string_pretty(&file).unwrap())
                                    .map_err(|e| e.to_string())?;
                                std::fs::rename(&temp, &path).map_err(|e| e.to_string())?;
                            }
                            Ok(())
                        });
                        if result.is_ok() {
                            active = next;
                        } else {
                            unregister(&next);
                            if let Err(e) = register(&active) {
                                crate::set_status(&e);
                            }
                        }
                        let _ = reply.send(result);
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        unregister(&active);
                        break;
                    }
                    _ => {}
                }
                let mut msg = MSG::default();
                if let Ok((macro_, generation)) = run_rx.try_recv() {
                    pending = None;
                    execute(&macro_, &signal, generation, &playing, true);
                    playing.finish();
                    while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() } {}
                }
                while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() } {
                    if msg.message == WM_HOTKEY && allowed() {
                        let i = msg.wParam.0.wrapping_sub(100);
                        if i < active.len() && active[i].enabled {
                            pending = Some((i, signal.load(Ordering::SeqCst)));
                        }
                    }
                }
                if let Some((i, generation)) = pending {
                    let (mods, key) = crate::parse_keyboard_trigger(&active[i].trigger)
                        .flatten()
                        .unwrap();
                    if generation != signal.load(Ordering::SeqCst) || !allowed() {
                        pending = None;
                    } else if crate::keyboard_trigger_released(mods, key) {
                        pending = None;
                        if playing.begin(i) {
                            execute(&active[i], &signal, generation, &playing, false);
                            playing.finish();
                        }
                        // Drop queued and injected hotkeys; macros cannot recursively trigger macros.
                        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() } {}
                    }
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        let macros =
            if cfg!(debug_assertions) && std::env::var_os("SLACKINPUT_UI_SNAPSHOT").is_some() {
                vec![Macro::default()]
            } else {
                macros
            };
        let mut editor = Self {
            macros,
            tx,
            cancel,
            message,
            applied: vec![],
            playback,
            runs,
            recording: None,
        };
        if editor.message.is_empty() && std::env::var_os("SLACKINPUT_UI_SNAPSHOT").is_none() {
            if let Err(e) = editor.apply(false) {
                editor.message = e;
            }
        }
        editor
    }
    fn runtime(&self, macros: Vec<Macro>, save: bool) -> Result<(), String> {
        self.cancel.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        self.tx
            .send((macros, save, tx))
            .map_err(|e| e.to_string())?;
        rx.recv().map_err(|e| e.to_string())?
    }
    fn apply(&mut self, save: bool) -> Result<(), String> {
        validate(&self.macros)?;
        self.runtime(self.macros.clone(), save)?;
        self.applied = self.macros.clone();
        Ok(())
    }
    fn restore_after_recording(&mut self) {
        let keyboard = crate::app_state()
            .lock()
            .unwrap()
            .config
            .keyboard_trigger
            .clone();
        let macro_result = self.runtime(self.applied.clone(), false);
        let keyboard_result = crate::configure_keyboard(&keyboard);
        RECORDING.store(false, Ordering::SeqCst);
        if let Err(e) = macro_result.and(keyboard_result) {
            self.message
                .push_str(&format!("; 恢复热键失败 / Restore hotkeys failed: {e}"));
        }
    }
    fn start_recording(&mut self) -> Result<(), String> {
        if !allowed() {
            return Err("当前禁止录制 / Recording disabled".into());
        }
        if self.macros.len() >= 64 {
            return Err("最多 64 个宏 / Maximum 64 macros".into());
        }
        RECORDING.store(true, Ordering::SeqCst);
        let result = self
            .runtime(vec![], false)
            .and_then(|()| crate::keyboard::configure(None))
            .and_then(|()| recording::Recording::start());
        match result {
            Ok(recording) => {
                self.recording = Some(recording);
                Ok(())
            }
            Err(e) => {
                self.message = e;
                self.restore_after_recording();
                Err(self.message.clone())
            }
        }
    }
    fn finish_recording(&mut self, keep: bool) {
        let Some(recording) = self.recording.take() else {
            return;
        };
        if keep {
            match recording.finish() {
                Ok(steps) => {
                    self.macros.push(Macro {
                        name: format!("Recorded {}", self.macros.len() + 1),
                        steps,
                        ..Default::default()
                    });
                    self.message="录制已生成禁用草稿，请检查触发键并保存 / Recorded draft; review trigger and save".into();
                }
                Err(e) => self.message = e,
            }
        } else {
            drop(recording);
            self.message = "已取消录制，未改变宏 / Recording cancelled".into();
        }
        self.restore_after_recording();
    }
    pub fn tick(&mut self, ctx: &egui::Context, on_tab: bool) {
        if self.recording.is_none() {
            return;
        }
        let permitted = {
            let state = crate::app_state().lock().unwrap();
            permitted(&state)
        };
        if !permitted || !on_tab {
            self.finish_recording(false);
            return;
        }
        if self.recording.as_mut().is_some_and(|r| r.poll()) {
            self.finish_recording(true);
        }
        ctx.request_repaint_after(Duration::from_millis(30));
    }
    pub fn show(&mut self, ui: &mut egui::Ui, language: crate::Language) {
        let t = |en, zh| crate::game_text(language, en, zh);
        crate::theme::heading(ui, "02", t("Key macros", "按键宏"));
        ui.label(t("Release trigger to run. One step = mapping; more steps = sequence; repeats = rapid fire.","松开触发键后执行。单步为映射，多步为组合序列，增加次数实现连发。"));
        let locked = crate::app_state().lock().unwrap().features_locked();
        if locked {
            ui.colored_label(
                crate::theme::MUTED,
                t(
                    "Anti-cheat detected: macro execution and recording disabled.",
                    "发现反作弊程序：按键宏执行与录制已禁用。",
                ),
            );
            return;
        }
        if let Some(recording) = &self.recording {
            let label = if recording.armed() {
                t(
                    "Recording globally — F12 to finish",
                    "正在全局录制，按 F12 结束",
                )
                .to_string()
            } else {
                format!(
                    "{} {}",
                    t("Starts in", "开始录制倒计时"),
                    recording.countdown()
                )
            };
            ui.label(label);
            ui.horizontal(|ui| {
                if ui.button(t("Finish recording", "结束录制")).clicked() {
                    self.finish_recording(true);
                }
                if ui.button(t("Cancel recording", "取消录制")).clicked() {
                    self.finish_recording(false);
                }
            });
            return;
        }
        ui.label(t("Record: starts after 3s; switch to your game. F12 ends. Global keys are captured until stopped; avoid private input.", "录制：3 秒后开始，可切到游戏，F12 结束。期间记录全局按键，请勿输入隐私内容。"));
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    allowed(),
                    egui::Button::new(t("Record new macro", "录制新宏")),
                )
                .clicked()
            {
                if let Err(e) = self.start_recording() {
                    self.message = e;
                }
            }
            if ui.button(t("Add macro", "添加宏")).clicked() {
                self.macros.push(Macro::default());
            }
            if ui.button(t("Save and apply", "保存并应用")).clicked() {
                self.message = match self.apply(true) {
                    Ok(()) => t("Saved macros.json", "已保存 macros.json").into(),
                    Err(e) => e,
                };
            }
            if ui.button(t("Stop all", "停止全部")).clicked() {
                self.cancel.fetch_add(1, Ordering::SeqCst);
            }
        });
        ui.label(&self.message);
        if self.recording.is_some() {
            return;
        }
        ui.label(t("Run/Resume activates the bound game. Losing game focus pauses playback. Pause releases held keys.", "运行/继续会自动切到已绑定游戏；游戏失去焦点时自动暂停，暂停会松开按键。"));
        let mut remove = None;
        let mut run = None;
        let busy = self.playback.running.load(Ordering::SeqCst);
        for (i, m) in self.macros.iter_mut().enumerate() {
            ui.push_id(i, |ui| {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.checkbox(&mut m.enabled, t("Enabled", "启用"));
                    ui.add(egui::TextEdit::singleline(&mut m.name).desired_width(140.0));
                    if ui
                        .add_enabled(!busy, egui::Button::new(t("Delete", "删除")))
                        .clicked()
                    {
                        remove = Some(i);
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(t("Trigger", "触发键"));
                    ui.add(egui::TextEdit::singleline(&mut m.trigger).desired_width(110.0));
                    ui.label(t("Repeats", "次数"));
                    ui.add(egui::DragValue::new(&mut m.repeats).range(1..=10000));
                });
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!busy && allowed(), egui::Button::new(t("Run", "运行")))
                        .clicked()
                    {
                        run = Some((i, m.clone()));
                    }
                    let mine = busy && self.playback.index.load(Ordering::SeqCst) == i;
                    let paused = self.playback.paused.load(Ordering::SeqCst);
                    if ui
                        .add_enabled(
                            mine,
                            egui::Button::new(if paused {
                                t("Resume", "继续")
                            } else {
                                t("Pause", "暂停")
                            }),
                        )
                        .clicked()
                    {
                        if paused {
                            match focus_game(&self.playback, true) {
                                Ok(()) => {
                                    self.playback.paused.store(false, Ordering::SeqCst);
                                    self.message =
                                        t("Game activated; resumed", "游戏已激活，继续执行").into();
                                }
                                Err(e) => self.message = e,
                            }
                        } else {
                            self.playback.paused.store(true, Ordering::SeqCst);
                            self.message = t(
                                "Paused; held keys will be released",
                                "已请求暂停，将释放按键",
                            )
                            .into();
                        }
                    }
                    if mine {
                        ui.label(if paused {
                            t("Paused", "已暂停")
                        } else {
                            t("Running", "运行中")
                        });
                    }
                });
                let mut action = None;
                for (j, s) in m.steps.iter_mut().enumerate() {
                    ui.push_id(j, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(format!("{}.", j + 1));
                            ui.add(egui::TextEdit::singleline(&mut s.keys).desired_width(100.0));
                            ui.label(t("Hold ms", "按住 ms"));
                            ui.add(egui::DragValue::new(&mut s.hold_ms).range(1..=60000));
                            ui.label(t("Gap ms", "间隔 ms"));
                            ui.add(egui::DragValue::new(&mut s.delay_ms).range(0..=60000));
                        });
                        ui.horizontal(|ui| {
                            if ui.add_enabled(j > 0, egui::Button::new("↑")).clicked() {
                                action = Some((j, true));
                            }
                            if ui.button(t("Remove step", "删除步骤")).clicked() {
                                action = Some((j, false));
                            }
                        });
                    });
                }
                if let Some((j, up)) = action {
                    if up {
                        m.steps.swap(j, j - 1);
                    } else {
                        m.steps.remove(j);
                    }
                }
                if ui.button(t("Add step", "添加步骤")).clicked() {
                    m.steps.push(Step::default());
                }
            });
        }
        if let Some(i) = remove {
            self.macros.remove(i);
        }
        if let Some((i, m)) = run {
            let mut validation = Vec::new();
            let mut draft = m.clone();
            draft.enabled = false; // Manual tests need no registered trigger.
            validation.push(draft);
            let result = validate(&validation).and_then(|()| {
                for step in &m.steps {
                    let output = crate::parse_keyboard_trigger(&step.keys).flatten();
                    if self.applied.iter().any(|active| {
                        active.enabled
                            && crate::parse_keyboard_trigger(&active.trigger).flatten() == output
                    }) {
                        return Err(
                            "输出与已启用宏触发键冲突 / Output conflicts with active macro trigger"
                                .into(),
                        );
                    }
                }
                if !self.playback.begin(i) {
                    return Err("已有宏正在运行 / A macro is running".into());
                }
                if let Err(e) = focus_game(&self.playback, false) {
                    self.playback.finish();
                    return Err(e);
                }
                let generation = self.cancel.load(Ordering::SeqCst);
                if let Err(e) = self.runs.send((m, generation)) {
                    self.playback.finish();
                    return Err(e.to_string());
                }
                Ok(())
            });
            if let Err(e) = result {
                self.message = e;
            }
        }
    }
}
impl Drop for Editor {
    fn drop(&mut self) {
        if let Some(recording) = self.recording.take() {
            drop(recording);
        }
        RECORDING.store(false, Ordering::SeqCst);
        self.cancel.fetch_add(1, Ordering::SeqCst);
        // Wait for key release and unregistration before the process can exit.
        let (tx, rx) = mpsc::channel();
        if self.tx.send((vec![], false, tx)).is_ok() {
            let _ = rx.recv();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn playback_rejects_concurrent_runs_and_invalidates_old_resume() {
        let playback = Playback::default();
        assert!(playback.begin(2));
        let session = playback.session.load(Ordering::SeqCst);
        assert!(!playback.begin(3));
        assert_eq!(playback.index.load(Ordering::SeqCst), 2);
        playback.paused.store(true, Ordering::SeqCst);
        playback.finish();
        assert!(playback.begin(3));
        assert_ne!(session, playback.session.load(Ordering::SeqCst));
        assert!(!playback.paused.load(Ordering::SeqCst));
    }
    #[test]
    fn paused_playback_can_be_cancelled_without_resuming() {
        let playback = Playback::default();
        playback.begin(0);
        playback.paused.store(true, Ordering::SeqCst);
        assert_eq!(
            phase(60000, &[], &AtomicU64::new(1), 0, &playback),
            Ok(false)
        );
    }
    #[test]
    fn settle_focus_never_blocks_on_cancellation() {
        let playback = Playback::default();
        playback.begin(0);
        playback
            .target_pid
            .store(u64::from(u32::MAX) as usize, Ordering::SeqCst);
        let started = Instant::now();
        assert!(!settle_focus(60000, &AtomicU64::new(1), 0, &playback));
        assert!(started.elapsed() < Duration::from_secs(1));
        // Without a target pid there is nothing to wait for: refuse instead of firing
        // into the current foreground window.
        playback.target_pid.store(0, Ordering::SeqCst);
        assert!(!settle_focus(60000, &AtomicU64::new(1), 0, &playback));
    }
    #[test]
    fn macro_scan_codes_preserve_release_and_extended_keys() {
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE,
        };
        for name in ["W", "Ctrl", "Alt", "Shift", "Left", "Delete", "Win"] {
            let key = crate::parse_token(name).unwrap();
            let down = unsafe { macro_key_input(key, false).Anonymous.ki };
            let up = unsafe { macro_key_input(key, true).Anonymous.ki };
            assert_ne!(down.wScan, 0, "{name}");
            assert_eq!(down.wVk.0, 0);
            assert!(down.dwFlags.contains(KEYEVENTF_SCANCODE));
            assert!(!down.dwFlags.contains(KEYEVENTF_KEYUP));
            assert_eq!(up.wScan, down.wScan);
            assert_eq!(up.dwFlags, down.dwFlags | KEYEVENTF_KEYUP);
            assert_eq!(
                down.dwFlags.contains(KEYEVENTF_EXTENDEDKEY),
                matches!(name, "Left" | "Delete" | "Win"),
                "{name}: scan={:x}",
                down.wScan
            );
        }
    }
    #[test]
    fn anti_cheat_lock_blocks_macros_and_recording_until_confirmed_clear() {
        let mut state = crate::AppState::new(crate::AppConfig::default(), vec![]);
        state.bound_process = Some(crate::process::BoundProcess::current_for_window_test());
        assert!(permitted(&state));
        let report = |outcome| crate::anti_cheat::Report {
            outcome,
            checked_at: Instant::now(),
        };
        state.accept_anti_cheat_report(
            0,
            report(crate::anti_cheat::Outcome::Detected(vec!["test".into()])),
        );
        assert!(!permitted(&state));
        state.accept_anti_cheat_report(
            0,
            report(crate::anti_cheat::Outcome::Unavailable(
                "scan failed".into(),
            )),
        );
        assert!(!permitted(&state));
        state.accept_anti_cheat_report(0, report(crate::anti_cheat::Outcome::NoKnownProcess));
        assert!(permitted(&state));
        state.config.capture_enabled = false;
        assert!(!permitted(&state));
    }
    #[test]
    fn validates_and_round_trips() {
        let m = Macro::default();
        validate(&[m.clone()]).unwrap();
        let f = File {
            version: 1,
            macros: vec![m],
        };
        assert_eq!(
            serde_json::from_str::<File>(&serde_json::to_string(&f).unwrap()).unwrap(),
            f
        );
    }
    #[test]
    fn rejects_output_recursion_and_timing_limits() {
        let mut m = Macro {
            enabled: true,
            ..Default::default()
        };
        m.steps[0].keys = "F8".into();
        assert!(validate(&[m.clone()]).is_err());
        m.steps[0] = Step::default();
        m.steps[0].delay_ms = 60001;
        assert!(validate(&[m.clone()]).is_err());
        m.steps[0].delay_ms = 60000;
        m.steps[0].hold_ms = 60000;
        validate(&[m.clone()]).unwrap();
        m.repeats = 0;
        assert!(validate(&[m]).is_err());
    }
    #[test]
    fn cancellation_does_not_wait_for_long_delay() {
        let signal = AtomicU64::new(2);
        let started = Instant::now();
        assert!(!wait(60000, &signal, 1));
        assert!(started.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn rejects_bad_sequences_and_conflicts() {
        let mut m = Macro {
            enabled: true,
            ..Default::default()
        };
        assert!(validate(&[m.clone(), m.clone()]).is_err());
        for key in ["", "Ctrl", "A+B", "Ctrl++Q"] {
            m.steps[0].keys = key.into();
            assert!(validate(&[m.clone()]).is_err());
        }
        m.steps[0] = Step::default();
        m.steps[0].hold_ms = 0;
        assert!(validate(&[m.clone()]).is_err());
        m.steps.clear();
        assert!(validate(&[m]).is_err());
    }
}
