#![windows_subsystem = "windows"]

mod anti_cheat;
mod desktop;
mod keyboard;
mod macros;
mod marker;
mod process;
mod startup;
mod theme;
mod titlebar;
mod trainer;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::collections::HashMap;
use std::env;
use std::ffi::c_void;
use std::fs;
use std::path::PathBuf;
use std::ptr::addr_of;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Color32, FontData, FontDefinitions, FontFamily, Layout, RichText, ScrollArea,
    Sense, TextEdit, vec2,
};
use windows::Win32::Foundation::{HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY,
    VK_CONTROL, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_LEFT, VK_LWIN, VK_MENU, VK_NEXT, VK_PRIOR,
    VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
};
use windows::Win32::UI::Input::{
    GetRawInputData, GetRawInputDeviceInfoW, GetRawInputDeviceList, HRAWINPUT, RAWINPUT,
    RAWINPUTDEVICE, RAWINPUTDEVICELIST, RAWINPUTHEADER, RID_DEVICE_INFO, RID_DEVICE_INFO_HID,
    RID_INPUT, RIDEV_DEVNOTIFY, RIDEV_INPUTSINK, RIDI_DEVICEINFO, RIDI_DEVICENAME, RIM_TYPEHID,
    RegisterRawInputDevices,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GIDC_ARRIVAL, GIDC_REMOVAL, GetMessageW,
    HWND_MESSAGE, MSG, PostQuitMessage, RegisterClassW, TranslateMessage, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_DESTROY, WM_INPUT, WM_INPUT_DEVICE_CHANGE, WNDCLASSW,
};
use windows::core::{Error, Result, w};

const TRIGGER_COOLDOWN: Duration = Duration::from_millis(500);
const RAW_GUIDE_TRIGGER_REPORT: [u8; 16] = [
    0x00, 0x00, 0x80, 0x00, 0x80, 0x00, 0x80, 0x00, 0x80, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00,
];
const HID_USAGE_PAGE_GENERIC_DESKTOP: u16 = 0x01;
const HID_USAGE_GAMEPAD: u16 = 0x05;
const HID_USAGE_JOYSTICK: u16 = 0x04;
const MAX_LOG_CHARS: usize = 32_000;
/// Floating egui windows whose rects must be excluded from the native caption
/// strip, or the WM_NCHITTEST subclass would swallow their close-button clicks.
const PROCESS_PICKER_WINDOW_ID: &str = "process_picker";
const DEBUG_LOG_WINDOW_ID: &str = "debug_log_window";
const CANDIDATE_CJK_FONTS: [&str; 4] = [
    "C:\\Windows\\Fonts\\simhei.ttf",
    "C:\\Windows\\Fonts\\Deng.ttf",
    "C:\\Windows\\Fonts\\simkai.ttf",
    "C:\\Windows\\Fonts\\simsunb.ttf",
];

static APP_STATE: OnceLock<Mutex<AppState>> = OnceLock::new();
static HID_NAMES: OnceLock<Mutex<HashMap<isize, String>>> = OnceLock::new();
static HID_EVENT_COUNTS: OnceLock<Mutex<HashMap<isize, u64>>> = OnceLock::new();
static LAST_TRIGGER_AT: OnceLock<Mutex<Instant>> = OnceLock::new();

#[derive(Clone)]
struct AppConfig {
    keyboard_trigger: String,
    speed_up: String,
    speed_down: String,
    mapping_text: String,
    capture_enabled: bool,
    debug_logging: bool,
    language: Language,
    pause_game_on_trigger: bool,
    /// Executable name bound last time, used for ordering and automatic binding.
    last_process: String,
    auto_load_process: bool,
    focus_enabled: bool,
    focus_locked: bool,
    focus_diameter: u8,
    focus_style: marker::Style,
    focus_opacity: u8,
    focus_x: f32,
    focus_y: f32,
}

impl AppConfig {
    fn copy_focus_from(&mut self, source: &Self) {
        self.focus_enabled = source.focus_enabled;
        self.focus_locked = source.focus_locked;
        self.focus_diameter = source.focus_diameter;
        self.focus_style = source.focus_style;
        self.focus_opacity = source.focus_opacity;
        self.focus_x = source.focus_x;
        self.focus_y = source.focus_y;
    }
}

fn focus_profile_path(name: &str) -> PathBuf {
    // Hex encoding prevents executable names from becoming paths; case is ignored on Windows.
    let key: String = name
        .trim()
        .to_lowercase()
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    config_dir().join("games").join(format!("{key}.ini"))
}

fn focus_body(config: &AppConfig) -> String {
    config_body(config)
        .lines()
        .filter(|line| line.starts_with("focus_"))
        .map(|line| format!("{line}\n"))
        .collect()
}

fn write_focus_profile(path: &std::path::Path, config: &AppConfig) -> std::io::Result<()> {
    fs::create_dir_all(path.parent().expect("profile directory"))?;
    fs::write(path, focus_body(config))
}

fn persist_focus(state: &mut AppState) {
    if cfg!(test) || env::var_os("SLACKINPUT_UI_SNAPSHOT").is_some() {
        return;
    }
    let result = if let Some(process) = &state.bound_process {
        let path = focus_profile_path(&process.name);
        write_focus_profile(&path, &state.config).map_err(|error| error.to_string())
    } else {
        let mut saved = load_config();
        saved.copy_focus_from(&state.config);
        save_config(&saved).map_err(|error| error.to_string())
    };
    if let Err(error) = result {
        state.status = format!("{}: {error}", tr(state.config.language).save_failed);
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            keyboard_trigger: String::new(),
            speed_up: "NumAdd".into(),
            speed_down: "NumSubtract".into(),
            mapping_text: "Ctrl+Win+Left".to_string(),
            capture_enabled: true,
            debug_logging: false,
            language: Language::English,
            pause_game_on_trigger: false,
            last_process: String::new(),
            auto_load_process: true,
            focus_enabled: false,
            focus_locked: false,
            focus_diameter: 16,
            focus_style: marker::Style::Ring,
            focus_opacity: 190,
            focus_x: 0.5,
            focus_y: 0.5,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Language {
    English,
    Chinese,
}

impl Language {
    fn from_config_value(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "en" | "english" => Some(Self::English),
            "zh" | "zh-cn" | "chinese" => Some(Self::Chinese),
            _ => None,
        }
    }

    fn config_value(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::Chinese => "zh-CN",
        }
    }

    /// Native name shown in the language combo box; never translated.
    fn display_name(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::Chinese => "中文",
        }
    }
}

struct I18n {
    title: &'static str,
    controller: &'static str,
    active: &'static str,
    inactive: &'static str,
    preset: &'static str,
    mapping: &'static str,
    mapping_hint: &'static str,
    capture: &'static str,
    debug: &'static str,
    save: &'static str,
    reset: &'static str,
    logs: &'static str,
    language: &'static str,
    status_capture_on: &'static str,
    status_capture_off: &'static str,
    status_saved_prefix: &'static str,
    enabled: &'static str,
    disabled: &'static str,
    invalid_hotkey: &'static str,
    save_failed: &'static str,
    triggered_prefix: &'static str,
    startup_log: &'static str,
    raw_input_started: &'static str,
    raw_input_failed: &'static str,
    raw_input_process_failed: &'static str,
    trigger_log_prefix: &'static str,
    capture_disabled_log: &'static str,
    config_updated_prefix: &'static str,
    font_missing_log: &'static str,
    picker_order_hint: &'static str,
}

fn tr(language: Language) -> I18n {
    match language {
        Language::English => I18n {
            title: "SlackInput",
            controller: "Controller Status",
            active: "Active",
            inactive: "Inactive",
            preset: "Preset",
            mapping: "Xbox Button Mapping",
            mapping_hint: "Examples: Ctrl+Win+Left, Alt+Tab, Ctrl+Shift+Esc",
            capture: "Enable input capture",
            debug: "Debug Window",
            save: "Save",
            reset: "Reset",
            logs: "Debug Log",
            language: "Language",
            status_capture_on: "Status: Input capture enabled",
            status_capture_off: "Status: Input capture disabled",
            status_saved_prefix: "Status: Saved",
            enabled: "enabled",
            disabled: "disabled",
            invalid_hotkey: "Status: Save failed, unsupported hotkey format",
            save_failed: "Status: Save failed",
            triggered_prefix: "Status: Triggered",
            startup_log: "Application started. Raw Input worker is running.",
            raw_input_started: "Raw Input registered: Generic Desktop / Gamepad + Joystick",
            raw_input_failed: "Status: Raw Input worker failed",
            raw_input_process_failed: "Status: Raw Input processing failed",
            trigger_log_prefix: "Triggered mapping",
            capture_disabled_log: "Guide report detected, but input capture is disabled.",
            config_updated_prefix: "Config updated",
            font_missing_log: "No CJK font found. egui will keep using default fonts.",
            picker_order_hint: "The last selected process is listed first.",
        },
        Language::Chinese => I18n {
            title: "SlackInput",
            controller: "手柄状态",
            active: "已连接",
            inactive: "未连接",
            preset: "常用映射",
            mapping: "Xbox 键映射",
            mapping_hint: "格式示例: Ctrl+Win+Left, Alt+Tab, Ctrl+Shift+Esc",
            capture: "启用输入捕获",
            debug: "调试窗口",
            save: "保存配置",
            reset: "恢复默认",
            logs: "调试日志",
            language: "语言",
            status_capture_on: "状态: 输入捕获已启用",
            status_capture_off: "状态: 输入捕获已关闭",
            status_saved_prefix: "状态: 已保存",
            enabled: "启用",
            disabled: "关闭",
            invalid_hotkey: "状态: 保存失败，不支持的热键格式",
            save_failed: "状态: 保存失败",
            triggered_prefix: "状态: 已触发",
            startup_log: "程序已启动，Raw Input 后台线程已启动。",
            raw_input_started: "Raw Input 已注册: Generic Desktop / Gamepad + Joystick",
            raw_input_failed: "状态: Raw Input 线程失败",
            raw_input_process_failed: "状态: Raw Input 处理失败",
            trigger_log_prefix: "触发映射",
            capture_disabled_log: "检测到 Guide 报告，但输入捕获当前已关闭。",
            config_updated_prefix: "配置已更新",
            font_missing_log: "未找到可用中文字体，egui 将继续使用默认字体。",
            picker_order_hint: "上次选择的进程会显示在列表最前面。",
        },
    }
}

struct AppState {
    binding_generation: u64,
    anti_cheat_report: Option<anti_cheat::Report>,
    anti_cheat_locked: bool,
    auto_load_suppressed: bool,
    config: AppConfig,
    mapping_keys: Vec<VIRTUAL_KEY>,
    status: String,
    logs: String,
    connected_devices: Vec<String>,
    bound_process: Option<process::BoundProcess>,
}

impl AppState {
    fn features_locked(&self) -> bool {
        #[cfg(debug_assertions)]
        if let Some(report) = snapshot_anti_cheat() {
            return matches!(report.outcome, anti_cheat::Outcome::Detected(_));
        }
        self.anti_cheat_locked && self.bound_process.as_ref().is_some_and(|p| !p.exited())
    }

    fn bind_process(&mut self, process: process::BoundProcess) {
        self.binding_generation = self.binding_generation.wrapping_add(1);
        self.anti_cheat_report = None;
        self.anti_cheat_locked = false;
        let profile = fs::read_to_string(focus_profile_path(&process.name))
            .map(|body| parse_config(&body))
            .unwrap_or_else(|_| load_config());
        self.config.copy_focus_from(&profile);
        self.bound_process = Some(process);
    }

    fn accept_anti_cheat_report(&mut self, generation: u64, report: anti_cheat::Report) {
        if self.binding_generation == generation
            && self.bound_process.as_ref().is_some_and(|p| !p.exited())
        {
            match &report.outcome {
                anti_cheat::Outcome::Detected(_) => self.anti_cheat_locked = true,
                anti_cheat::Outcome::NoKnownProcess => self.anti_cheat_locked = false,
                anti_cheat::Outcome::Unavailable(_) => {}
            }
            if self.anti_cheat_locked {
                if let Some(process) = self.bound_process.as_mut() {
                    if let Err(error) = process.restore() {
                        self.status = format!(
                            "{}: {error}",
                            game_text(
                                self.config.language,
                                "Failed to resume game",
                                "恢复游戏失败"
                            )
                        );
                    }
                }
            }
            self.anti_cheat_report = Some(report);
        }
    }

    fn auto_load_target(&self) -> Option<String> {
        (self.config.auto_load_process
            && !self.auto_load_suppressed
            && !self.config.last_process.is_empty()
            && self.bound_process.as_ref().is_none_or(|p| p.exited()))
        .then(|| self.config.last_process.clone())
    }

    fn new(config: AppConfig, mapping_keys: Vec<VIRTUAL_KEY>) -> Self {
        let text = tr(config.language);
        let status = if config.capture_enabled {
            text.status_capture_on.to_string()
        } else {
            text.status_capture_off.to_string()
        };
        Self {
            binding_generation: 0,
            anti_cheat_report: None,
            anti_cheat_locked: false,
            auto_load_suppressed: false,
            config,
            mapping_keys,
            status,
            logs: String::new(),
            connected_devices: Vec::new(),
            bound_process: None,
        }
    }
}

struct MapperApp {
    macros: macros::Editor,
    trainer: trainer::TrainerUi,
    shortcut_capture: Option<usize>,
    captured_shortcut: Option<String>,
    settings_tab: usize,
    keyboard_trigger_input: String,
    speed_up_input: String,
    speed_down_input: String,
    mapping_input: String,
    capture_enabled: bool,
    debug_logging: bool,
    selected_preset: usize,
    language: Language,
    debug_window_open: bool,
    pause_game_on_trigger: bool,
    process_picker_open: bool,
    processes: Vec<process::BoundProcess>,
    process_filter: String,
    icon: egui::TextureHandle,
    _desktop: Option<desktop::Desktop>,
    titlebar: Option<titlebar::Titlebar>,
    #[cfg(debug_assertions)]
    snapshot_frames: u32,
}

impl MapperApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let state = app_state().lock().expect("app state mutex poisoned");
        let config = state.config.clone();
        drop(state);

        let icon_data = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png"))
            .expect("embedded icon");
        let icon = cc.egui_ctx.load_texture(
            "app_icon",
            egui::ColorImage::from_rgba_unmultiplied(
                [icon_data.width as usize, icon_data.height as usize],
                &icon_data.rgba,
            ),
            egui::TextureOptions::NEAREST,
        );
        let titlebar = match cc.window_handle().map(|h| h.as_raw()) {
            Ok(RawWindowHandle::Win32(handle)) => {
                match titlebar::Titlebar::install(HWND(handle.hwnd.get() as *mut _)) {
                    Ok(titlebar) => Some(titlebar),
                    Err(error) => {
                        set_status(&format!("Titlebar: {error}"));
                        cc.egui_ctx
                            .send_viewport_cmd(egui::ViewportCommand::Decorations(true));
                        None
                    }
                }
            }
            _ => None,
        };
        let desktop = match cc.window_handle().map(|h| h.as_raw()) {
            Ok(RawWindowHandle::Win32(handle)) => {
                match desktop::Desktop::start(handle.hwnd.get(), cc.egui_ctx.clone()) {
                    Ok(desktop) => Some(desktop),
                    Err(error) => {
                        set_status(&format!("Tray / focus ring unavailable: {error}"));
                        None
                    }
                }
            }
            _ => None,
        };

        let mut app = Self {
            trainer: trainer::TrainerUi::new(config_dir().join("trainers")),
            macros: macros::Editor::new(),
            shortcut_capture: None,
            captured_shortcut: None,
            settings_tab: 0,
            keyboard_trigger_input: config.keyboard_trigger.clone(),
            speed_up_input: config.speed_up.clone(),
            speed_down_input: config.speed_down.clone(),
            selected_preset: preset_index(&config.mapping_text).unwrap_or(0),
            mapping_input: config.mapping_text,
            capture_enabled: config.capture_enabled,
            debug_logging: config.debug_logging,
            language: config.language,
            debug_window_open: config.debug_logging,
            pause_game_on_trigger: config.pause_game_on_trigger,
            process_picker_open: false,
            processes: Vec::new(),
            process_filter: String::new(),
            icon,
            _desktop: desktop,
            titlebar,
            #[cfg(debug_assertions)]
            snapshot_frames: 0,
        };
        if let Err(error) = configure_shortcuts(&config.keyboard_trigger, &config.speed_up, &config.speed_down) {
            set_status(&error);
        }
        app.refresh_processes();
        app.processes.clear();
        #[cfg(debug_assertions)]
        let app = app.with_snapshot_picker();
        app
    }

    /// Pre-opens the process picker for the snapshot harness, so its ordering and
    /// highlight can be captured without a click.
    #[cfg(debug_assertions)]
    fn with_snapshot_picker(mut self) -> Self {
        if env::var_os("SLACKINPUT_UI_SNAPSHOT").is_some()
            && env::var_os("SLACKINPUT_UI_TRAINER").is_some()
        {
            self.trainer.preview();
            self.settings_tab = 1;
        }
        if env::var_os("SLACKINPUT_UI_SNAPSHOT").is_some() {
            self.settings_tab = env::var("SLACKINPUT_UI_TAB")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(self.settings_tab);
        }
        if env::var_os("SLACKINPUT_UI_SNAPSHOT").is_some()
            && env::var_os("SLACKINPUT_UI_PICKER").is_some()
        {
            self.refresh_processes();
            self.process_picker_open = true;
        }
        self
    }

    fn apply_changes(&mut self) {
        if self.shortcut_capture.is_some() {
            self.stop_shortcut_capture();
        }
        let mapping_text = self.mapping_input.trim().to_string();
        let text = tr(self.language);
        let Some(mapping_keys) = parse_mapping(&mapping_text) else {
            set_status(text.invalid_hotkey);
            push_log_force(text.invalid_hotkey);
            return;
        };
        if let Err(error) = configure_shortcuts(self.keyboard_trigger_input.trim(), self.speed_up_input.trim(), self.speed_down_input.trim()) {
            set_status(&error);
            return;
        }

        let config = AppConfig {
            keyboard_trigger: self.keyboard_trigger_input.trim().to_string(),
            speed_up: self.speed_up_input.trim().to_string(),
            speed_down: self.speed_down_input.trim().to_string(),
            mapping_text: mapping_text.clone(),
            capture_enabled: self.capture_enabled,
            debug_logging: self.debug_logging,
            language: self.language,
            pause_game_on_trigger: self.pause_game_on_trigger,
            ..app_state()
                .lock()
                .expect("app state mutex poisoned")
                .config
                .clone()
        };

        {
            let mut state = app_state().lock().expect("app state mutex poisoned");
            state.config = config.clone();
            state.mapping_keys = mapping_keys;
            state.status = format!(
                "{}: {} {}, {} {}",
                text.status_saved_prefix,
                text.capture,
                if config.capture_enabled {
                    text.enabled
                } else {
                    text.disabled
                },
                text.mapping,
                config.mapping_text
            );
        }

        let mut saved = config.clone();
        if app_state().lock().unwrap().bound_process.is_some() {
            saved.copy_focus_from(&load_config());
        }
        match save_config(&saved) {
            Ok(()) => push_log_force(&format!(
                "{}: capture_enabled={}, debug_logging={}, language={}, mapping={}",
                text.config_updated_prefix,
                config.capture_enabled,
                config.debug_logging,
                config.language.config_value(),
                config.mapping_text
            )),
            Err(error) => set_status(&format!("{} - {error}", text.save_failed)),
        }

        self.selected_preset = preset_index(&mapping_text).unwrap_or(self.selected_preset);
    }
}

fn game_text(language: Language, english: &'static str, chinese: &'static str) -> &'static str {
    match language {
        Language::English => english,
        Language::Chinese => chinese,
    }
}

fn report_process_error(language: Language, error: Error) {
    let message = format!(
        "{}: {error}",
        game_text(language, "Process operation failed", "进程操作失败")
    );
    set_status(&message);
    push_log_force(&message);
}

fn remembered_process() -> String {
    app_state()
        .lock()
        .expect("app state mutex poisoned")
        .config
        .last_process
        .clone()
}

fn is_remembered_process(process: &process::BoundProcess, remembered: &str) -> bool {
    !remembered.is_empty() && process.name.eq_ignore_ascii_case(remembered)
}

/// Remembering the bound executable is a convenience for next time, so it is
/// persisted right away instead of waiting for Save. The file is rebuilt from the
/// saved config only: live-but-unsaved settings (focus ring, pause toggle) must
/// not be promoted just because a process was bound.
fn remember_process(name: &str) {
    app_state()
        .lock()
        .expect("app state mutex poisoned")
        .config
        .last_process = name.to_string();

    let mut config = load_config();
    if config.last_process == name {
        return;
    }
    config.last_process = name.to_string();
    if let Err(error) = save_config(&config) {
        set_status(&format!("{} - {error}", tr(current_language()).save_failed));
    }
}

impl MapperApp {
    fn stop_shortcut_capture(&mut self) {
        self.shortcut_capture = None;
        self.captured_shortcut = None;
        let saved = app_state().lock().unwrap().config.keyboard_trigger.clone();
        if let Err(error) = configure_keyboard(&saved) {
            set_status(&error);
        }
    }

    fn update_shortcut_capture(&mut self, ctx: &egui::Context) {
        let Some(target) = self.shortcut_capture else {
            return;
        };
        if !ctx.input(|input| input.focused) || self.settings_tab != 3 {
            self.stop_shortcut_capture();
            return;
        }
        if self.captured_shortcut.is_none() {
            use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
            let held = |vk| unsafe { GetAsyncKeyState(vk) < 0 };
            let name = if held(0x6B) { Some("NumAdd") } else if held(0x6D) { Some("NumSubtract") } else { None };
            if let Some(name) = name {
                let mut parts = Vec::new();
                if held(0x11) { parts.push("Ctrl"); }
                if held(0x12) { parts.push("Alt"); }
                if held(0x10) { parts.push("Shift"); }
                if held(0x5B) || held(0x5C) { parts.push("Win"); }
                parts.push(name);
                self.captured_shortcut = Some(parts.join("+"));
            }
        }
        ctx.input_mut(|input| {
            for event in &input.events {
                if let egui::Event::Key {
                    key,
                    pressed: true,
                    repeat: false,
                    modifiers,
                    ..
                } = event
                {
                    if self.captured_shortcut.is_none() {
                        let win = unsafe {
                            use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
                            GetAsyncKeyState(0x5B) < 0 || GetAsyncKeyState(0x5C) < 0
                        };
                        self.captured_shortcut = captured_key(*key, *modifiers, win);
                    }
                }
            }
            input.events.retain(|event| {
                !matches!(
                    event,
                    egui::Event::Key { .. } | egui::Event::Text(_) | egui::Event::Paste(_)
                )
            });
        });
        if self.captured_shortcut.as_ref().is_some_and(|text| {
            parse_keyboard_trigger(text)
                .flatten()
                .is_some_and(|(modifiers, key)| keyboard_trigger_released(modifiers, key))
        }) && ctx.input(|input| input.keys_down.is_empty() && input.modifiers.is_none())
        {
            let value = self.captured_shortcut.take().unwrap();
            if target == 0 {
                self.selected_preset = preset_index(&value).unwrap_or(self.selected_preset);
                self.mapping_input = value;
            } else {
                match target {
                    1 => self.keyboard_trigger_input = value,
                    2 => self.speed_up_input = value,
                    _ => self.speed_down_input = value,
                }
            }
            self.stop_shortcut_capture();
        }
    }

    fn shortcut_input_row(&mut self, ui: &mut egui::Ui, target: usize) {
        let capturing = self.shortcut_capture == Some(target);
        ui.horizontal(|ui| {
            let width = (ui.available_width() - 184.0).max(80.0);
            let value = match target {
                0 => &mut self.mapping_input,
                1 => &mut self.keyboard_trigger_input,
                2 => &mut self.speed_up_input,
                _ => &mut self.speed_down_input,
            };
            ui.add_enabled(
                self.shortcut_capture.is_none(),
                TextEdit::singleline(value)
                    .desired_width(width)
                    .font(egui::TextStyle::Monospace),
            );
            if ui
                .add_sized(
                    vec2(80.0, 32.0),
                    egui::Button::new(game_text(
                        self.language,
                        if capturing { "Cancel" } else { "Capture" },
                        if capturing { "取消捕获" } else { "捕获" },
                    )),
                )
                .clicked()
            {
                if self.shortcut_capture.is_some() {
                    self.stop_shortcut_capture();
                }
                if !capturing {
                    match keyboard::configure_all([None; 3]) {
                        Ok(()) => {
                            self.shortcut_capture = Some(target);
                            ui.memory_mut(|memory| {
                                if let Some(id) = memory.focused() {
                                    memory.surrender_focus(id);
                                }
                            });
                        }
                        Err(error) => set_status(&error),
                    }
                }
            }
            if ui
                .add_sized(
                    vec2(64.0, 32.0),
                    egui::Button::new(game_text(self.language, "Clear", "清空")),
                )
                .clicked()
            {
                if self.shortcut_capture.is_some() {
                    self.stop_shortcut_capture();
                }
                match target {
                    0 => self.mapping_input.clear(),
                    1 => self.keyboard_trigger_input.clear(),
                    2 => self.speed_up_input.clear(),
                    _ => self.speed_down_input.clear(),
                }
            }
        });
        if capturing {
            ui.label(
                RichText::new(game_text(
                    self.language,
                    "Press and release a key or shortcut…",
                    "请按下并松开要捕获的按键或组合键…",
                ))
                .small()
                .color(theme::MINT),
            );
        }
    }

    fn focus_controls(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing.y = 4.0;
        let language = self.language;
        theme::heading(ui, "03", game_text(language, "Focus ring", "防晕眩圆点"));
        let profile = app_state()
            .lock()
            .unwrap()
            .bound_process
            .as_ref()
            .map(|p| p.name.clone())
            .unwrap_or_else(|| {
                game_text(
                    language,
                    "Default (no game bound)",
                    "默认配置（未绑定游戏）",
                )
                .into()
            });
        ui.label(RichText::new(profile).small().color(theme::MINT));
        ui.label(
            RichText::new(game_text(
                language,
                "A steady reference point for your game.",
                "为游戏画面提供一个稳定的视觉参考点。",
            ))
            .small()
            .color(theme::MUTED),
        );
        let (mut config, available) = {
            let state = app_state().lock().unwrap();
            (
                state.config.clone(),
                state
                    .bound_process
                    .as_ref()
                    .is_some_and(|p| !p.paused && !p.exited()),
            )
        };
        let mut changed = ui
            .checkbox(
                &mut config.focus_enabled,
                game_text(language, "Enable focus ring", "启用防晕眩圆点"),
            )
            .changed();
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), 64.0), Sense::click_and_drag());
        if !config.focus_locked && (response.dragged() || response.clicked()) {
            if let Some(pointer) = response.interact_pointer_pos() {
                config.focus_x = ((pointer.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                config.focus_y = ((pointer.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
                changed = true;
            }
        }
        ui.painter().rect_filled(rect, 0.0, theme::BG);
        let center = rect.min
            + vec2(
                rect.width() * config.focus_x,
                rect.height() * config.focus_y,
            );
        for x in (0..(rect.width() as i32)).step_by(16) {
            ui.painter().line_segment(
                [
                    egui::pos2(rect.left() + x as f32, rect.top()),
                    egui::pos2(rect.left() + x as f32, rect.bottom()),
                ],
                egui::Stroke::new(0.5, theme::EDGE),
            );
        }
        for y in (0..64).step_by(16) {
            ui.painter().line_segment(
                [
                    egui::pos2(rect.left(), rect.top() + y as f32),
                    egui::pos2(rect.right(), rect.top() + y as f32),
                ],
                egui::Stroke::new(0.5, theme::EDGE),
            );
        }
        let origin = center
            - vec2(
                config.focus_diameter as f32 / 2.0,
                config.focus_diameter as f32 / 2.0,
            );
        let painter = ui.painter().with_clip_rect(rect);
        for (x, y, length) in marker::spans(config.focus_diameter, config.focus_style) {
            painter.rect_filled(
                egui::Rect::from_min_size(
                    origin + vec2(x as f32, y as f32),
                    vec2(length as f32, 1.0),
                ),
                0.0,
                Color32::from_white_alpha(config.focus_opacity),
            );
        }
        let style_label = |style| match style {
            marker::Style::Ring => game_text(language, "Hollow", "空心"),
            marker::Style::Solid => game_text(language, "Solid", "实心"),
            marker::Style::Crosshair => game_text(language, "Crosshair", "十字瞄准"),
            marker::Style::X => game_text(language, "X", "叉形"),
        };
        ui.horizontal(|ui| {
            ui.label(game_text(language, "Style", "内部样式"));
            egui::ComboBox::from_id_salt("focus_style")
                .selected_text(style_label(config.focus_style))
                .show_ui(ui, |ui| {
                    for style in marker::Style::ALL {
                        changed |= ui
                            .selectable_value(&mut config.focus_style, style, style_label(style))
                            .changed();
                    }
                });
        });
        ui.label(
            RichText::new(game_text(
                language,
                "DRAG IN PREVIEW TO POSITION",
                "解锁后可在预览区域拖动位置",
            ))
            .small()
            .monospace()
            .color(theme::MUTED),
        );
        changed |= ui
            .add(
                egui::Slider::new(&mut config.focus_diameter, 8..=36)
                    .text(game_text(language, "Size", "大小")),
            )
            .changed();
        changed |= ui
            .add(
                egui::Slider::new(&mut config.focus_opacity, 50..=255).text(game_text(
                    language,
                    "Opacity",
                    "不透明度",
                )),
            )
            .changed();
        ui.horizontal(|ui| {
            let label = if config.focus_locked {
                game_text(language, "Unlock position", "解锁位置")
            } else {
                game_text(language, "Lock position", "锁定位置")
            };
            if ui.button(label).clicked() {
                config.focus_locked = !config.focus_locked;
                changed = true;
            }
            if ui
                .button(game_text(language, "Center", "回到中心"))
                .clicked()
            {
                config.focus_x = 0.5;
                config.focus_y = 0.5;
                changed = true;
            }
        });
        ui.label(
            RichText::new(if config.focus_locked {
                game_text(
                    language,
                    "LOCKED / Mouse clicks pass through to the game.",
                    "已锁定 / 鼠标点击穿透到游戏。",
                )
            } else {
                game_text(
                    language,
                    "UNLOCKED / Drag the marker or its preview.",
                    "已解锁 / 可拖动画面中的圆点或上方预览。",
                )
            })
            .small()
            .color(theme::MINT),
        );
        ui.label(
            RichText::new(if available {
                game_text(
                    language,
                    "Appears over the game while it is running.",
                    "游戏运行时显示，暂停或退出时自动隐藏。",
                )
            } else {
                game_text(
                    language,
                    "Waiting for a bound, running game.",
                    "等待绑定并运行游戏。",
                )
            })
            .small()
            .color(theme::MUTED),
        );
        ui.label(
            RichText::new(game_text(
                language,
                "Changes save automatically for this game.",
                "调整自动保存，按游戏进程分别记忆。",
            ))
            .small()
            .color(theme::MUTED),
        );
        if changed {
            let mut state = app_state().lock().unwrap();
            state.config.focus_enabled = config.focus_enabled;
            state.config.focus_locked = config.focus_locked;
            state.config.focus_diameter = config.focus_diameter;
            state.config.focus_style = config.focus_style;
            state.config.focus_opacity = config.focus_opacity;
            state.config.focus_x = config.focus_x;
            state.config.focus_y = config.focus_y;
            persist_focus(&mut state);
        }
    }

    fn refresh_processes(&mut self) {
        app_state().lock().unwrap().auto_load_suppressed = false;
        match process::enumerate() {
            Ok(processes) => {
                self.processes = processes;
                let mut state = app_state().lock().expect("app state mutex poisoned");
                if state.config.auto_load_process
                    && state.bound_process.as_ref().is_none_or(|p| p.exited())
                {
                    if let Some(index) = self.processes.iter().position(|p| {
                        is_remembered_process(p, &state.config.last_process) && !p.exited()
                    }) {
                        state.bind_process(self.processes.remove(index));
                        state.status = game_text(
                            self.language,
                            "Last game process loaded automatically",
                            "已自动加载上次的游戏进程",
                        )
                        .into();
                    }
                }
            }
            Err(error) => report_process_error(self.language, error),
        }
    }

    fn anti_cheat_badge(&self, ui: &mut egui::Ui) {
        let language = self.language;
        let report = app_state().lock().unwrap().anti_cheat_report.clone();
        #[cfg(debug_assertions)]
        let report = snapshot_anti_cheat().or(report);

        let report = report.as_ref().filter(|report| report.fresh());
        let (label, color, detail) = match report.map(|report| &report.outcome) {
            None => (game_text(language, "AC: checking", "反作弊：检测中"), theme::MUTED,
                game_text(language, "Checking known process names; stale results are discarded.", "正在检查已知进程名；过期结果不会作为当前状态。").to_string()),
            Some(anti_cheat::Outcome::NoKnownProcess) =>
                (game_text(language, "AC: not found*", "未发现已知进程*"), theme::MINT,
                game_text(language, "No known EAC / BattlEye process names found. This does not mean editing is supported or safe. Drivers, in-game protection and other vendors are not checked.",
                    "未发现已知 EAC / BattlEye 进程名，不代表支持修改或修改安全。未检查驱动、游戏内保护或其他厂商。").to_string()),
            Some(anti_cheat::Outcome::Detected(evidence)) =>
                (game_text(language, "AC: detected", "发现反作弊进程"), theme::GOLD,
                format!("{}\n{}", game_text(language,
                    "Known process names found on this PC; association with this game is unverified. Names are not signature-verified.",
                    "本机发现已知进程名；未确认属于当前游戏，也未校验数字签名。"), evidence.join("\n"))),
            Some(anti_cheat::Outcome::Unavailable(error)) =>
                (game_text(language, "AC: unknown", "反作弊：未知"), theme::GOLD,
                format!("{}\n{error}", game_text(language, "Process scan failed; retrying automatically.", "进程检查失败，将自动重试。"))),
        };
        if app_state().lock().unwrap().features_locked() {
            let hint = game_text(
                language,
                "Unlock / block anti-cheat (in development)",
                "解锁/屏蔽反作弊（开发中）",
            );
            let (rect, response) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
            let center = rect.center();
            let stroke = egui::Stroke::new(1.8, theme::GOLD);
            ui.painter().rect_stroke(
                egui::Rect::from_center_size(center + vec2(0.0, -4.0), vec2(9.0, 12.0)),
                5.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            ui.painter().rect_filled(
                egui::Rect::from_center_size(center + vec2(0.0, 4.0), vec2(16.0, 12.0)),
                3.0,
                theme::GOLD,
            );
            ui.painter()
                .circle_filled(center + vec2(0.0, 3.0), 1.6, theme::BG);
            response
                .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, hint));
            if response.clicked() {
                set_status(hint);
            }
            response.on_hover_text(format!("{hint}\n{detail}"));
            return;
        }
        let (rect, response) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
        let center = rect.center();
        let points = vec![
            center + vec2(0.0, -8.0),
            center + vec2(7.0, -5.0),
            center + vec2(6.0, 3.0),
            center + vec2(0.0, 8.0),
            center + vec2(-6.0, 3.0),
            center + vec2(-7.0, -5.0),
        ];
        ui.painter().add(egui::Shape::convex_polygon(
            points,
            color.gamma_multiply(0.15),
            egui::Stroke::new(1.4, color),
        ));
        let symbol = match report.map(|report| &report.outcome) {
            None => "…",
            Some(anti_cheat::Outcome::NoKnownProcess) => "−",
            Some(anti_cheat::Outcome::Detected(_)) => "!",
            Some(anti_cheat::Outcome::Unavailable(_)) => "?",
        };
        ui.painter().text(
            center,
            egui::Align2::CENTER_CENTER,
            symbol,
            egui::FontId::proportional(12.0),
            color,
        );
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, label));
        response.on_hover_text(format!(
            "{label}\n{detail}\n{}",
            game_text(
                language,
                "Read-only process check every 2 seconds. No game data is read or changed.",
                "每 2 秒只读检查进程名，不读取或修改游戏数据。"
            )
        ));
    }

    fn game_controls(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        ui.vertical(|ui| {
            let (label, paused, bound) = {
                let state = app_state().lock().expect("app state mutex poisoned");
                match &state.bound_process {
                    Some(p) => (format!("{} (PID {})", p.name, p.pid), p.paused, true),
                    None => (
                        game_text(language, "No process bound", "未绑定进程").into(),
                        false,
                        false,
                    ),
                }
            };
            #[cfg(debug_assertions)]
            let (label, paused, bound) = if snapshot_anti_cheat().is_some() {
                ("ExampleGame.exe (PID 1234)".to_string(), false, true)
            } else {
                (label, paused, bound)
            };
            ui.add(egui::Label::new(&label).truncate())
                .on_hover_text(&label);
            if bound {
                ui.horizontal(|ui| {
                    ui.label(if paused {
                        game_text(language, "Paused", "已暂停")
                    } else {
                        game_text(language, "Running", "运行中")
                    });
                    self.anti_cheat_badge(ui);
                });
            }
            ui.add_space(8.0);
            let features_locked = app_state().lock().unwrap().features_locked();
            ui.horizontal(|ui| {
                ui.label(game_text(language, "Game speed", "游戏加速"));
                let multiplier = app_state().lock().unwrap().bound_process.as_ref()
                    .and_then(|p| p.speed.as_ref()).map_or(1, |s| s.multiplier);
                for speed in [1, 2, 4] {
                    if ui.add_enabled(bound && (speed == 1 || (!features_locked && !paused)),
                        egui::Button::new(format!("x{speed}")).selected(multiplier == speed)).clicked() {
                        change_game_speed(Some(speed), false, None);
                    }
                }
            });
            let speed_keys = {
                let state = app_state().lock().unwrap();
                let key_label = |key: &str| if key.is_empty() {
                    game_text(language, "Disabled", "未设置").to_string()
                } else { key.replace("NumAdd", "Num +").replace("NumSubtract", "Num −") };
                format!("{}: {} / {} · x1 ↔ x2 ↔ x4",
                    game_text(language, "Shortcuts", "快捷键"),
                    key_label(&state.config.speed_up), key_label(&state.config.speed_down))
            };
            ui.add(egui::Label::new(RichText::new(&speed_keys).small()).truncate())
                .on_hover_text(speed_keys);
            ui.add_enabled_ui(!features_locked, |ui| {
                let button_height = ((ui.available_height() - 132.0) / 2.0).clamp(48.0, 110.0);
                for resume in [false, true] {
                    let enabled = ui.is_enabled() && if resume { paused } else { bound && !paused };
                    let label = if resume {
                        game_text(language, "Resume game", "恢复游戏")
                    } else {
                        game_text(language, "Pause game", "中断游戏")
                    };
                    let highlighted = enabled && resume && paused;
                    let mut button = egui::Button::new("")
                        .corner_radius(14)
                        .min_size(vec2(ui.available_width(), button_height));
                    if highlighted {
                        button = button.fill(theme::MINT);
                    }
                    let response = ui.add_enabled(enabled, button);
                    response.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label)
                    });
                    ui.painter().text(
                        response.rect.center(),
                        egui::Align2::CENTER_CENTER,
                        label,
                        egui::FontId::proportional(19.0),
                        if highlighted {
                            theme::BG
                        } else if enabled {
                            theme::TEXT
                        } else {
                            theme::MUTED
                        },
                    );
                    let center = egui::pos2(response.rect.left() + 36.0, response.rect.center().y);
                    let color = if highlighted {
                        theme::BG
                    } else if enabled {
                        theme::MINT
                    } else {
                        theme::MUTED
                    };
                    if resume {
                        ui.painter().add(egui::Shape::convex_polygon(
                            vec![
                                center + vec2(-10.0, -14.0),
                                center + vec2(14.0, 0.0),
                                center + vec2(-10.0, 14.0),
                            ],
                            color,
                            egui::Stroke::NONE,
                        ));
                    } else {
                        for offset in [-9.0, 5.0] {
                            ui.painter().rect_filled(
                                egui::Rect::from_min_size(
                                    center + vec2(offset, -14.0),
                                    vec2(6.0, 28.0),
                                ),
                                1.0,
                                color,
                            );
                        }
                    }
                    if response.clicked() {
                        let result = {
                            let mut state = app_state().lock().unwrap();
                            if state.features_locked() {
                                return;
                            }
                            state
                                .bound_process
                                .as_mut()
                                .map(|p| if resume { p.resume() } else { p.suspend() })
                                .transpose()
                        };
                        match result {
                            Ok(_) => set_status(if resume {
                                game_text(language, "Game resumed", "游戏已恢复")
                            } else {
                                game_text(language, "Game paused", "游戏已暂停")
                            }),
                            Err(error) => report_process_error(language, error),
                        }
                    }
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui
                        .button(game_text(language, "Select process", "选择进程"))
                        .clicked()
                    {
                        self.refresh_processes();
                        self.process_picker_open = true;
                    }
                    if ui
                        .add_enabled(
                            bound,
                            egui::Button::new(game_text(language, "Unbind", "解除绑定")),
                        )
                        .clicked()
                    {
                        let result = {
                            let mut state = app_state().lock().unwrap();
                            let result =
                                state.bound_process.as_mut().map(|p| p.restore()).transpose();
                            if result.is_ok() {
                                state.bound_process = None;
                                state.auto_load_suppressed = true;
                                state.config.copy_focus_from(&load_config());
                            }
                            result
                        };
                        if let Err(error) = result {
                            report_process_error(language, error);
                        }
                    }
                });
            });
        });
    }

    fn shortcuts_tab(&mut self, ui: &mut egui::Ui) {
        let text = tr(self.language);
        theme::heading(ui, "01", game_text(self.language, "Shortcuts", "快捷键"));
        ui.label(RichText::new(text.preset).color(theme::MUTED));
        egui::ComboBox::from_id_salt("preset_combo")
            .width(ui.available_width())
            .selected_text(PRESETS[self.selected_preset])
            .show_ui(ui, |ui| {
                for (index, preset) in PRESETS.iter().enumerate() {
                    if ui
                        .selectable_value(&mut self.selected_preset, index, *preset)
                        .clicked()
                    {
                        self.mapping_input = (*preset).to_string();
                    }
                }
            });
        self.shortcut_input_row(ui, 0);
        ui.label(RichText::new(text.mapping_hint).small().color(theme::MUTED));
        ui.label(game_text(self.language, "Keyboard trigger", "键盘触发键"));
        self.shortcut_input_row(ui, 1);
        ui.label(
            RichText::new(game_text(
                self.language,
                "Blank disables. Apply with Save in Settings. Triggers on release.",
                "留空关闭；在设置页保存后生效，松开按键时触发。",
            ))
            .small()
            .color(theme::MUTED),
        );
        ui.separator();
        ui.label(game_text(self.language, "Game speed shortcuts", "游戏加速快捷键"));
        ui.label(game_text(self.language, "Speed up", "加速"));
        self.shortcut_input_row(ui, 2);
        ui.label(game_text(self.language, "Speed down", "减速"));
        self.shortcut_input_row(ui, 3);
        ui.label(RichText::new(game_text(self.language,
            "NumAdd / NumSubtract = numpad + / −. Blank disables; save in Settings.",
            "NumAdd / NumSubtract 表示小键盘 + / −；支持组合键，留空关闭，在设置页保存。")).small());
    }

    fn settings_controls(&mut self, ui: &mut egui::Ui) {
        let text = tr(self.language);
        let language = self.language;
        ui.horizontal(|ui| {
            if ui
                .add(
                    egui::Button::new(RichText::new(text.save).strong().color(theme::BG))
                        .fill(theme::MINT),
                )
                .clicked()
            {
                self.apply_changes();
            }
            if ui
                .add_enabled(
                    !app_state().lock().unwrap().features_locked(),
                    egui::Button::new(text.reset),
                )
                .clicked()
            {
                self.selected_preset = 0;
                self.mapping_input = PRESETS[0].to_string();
                self.keyboard_trigger_input.clear();
                self.speed_up_input = "NumAdd".into();
                self.speed_down_input = "NumSubtract".into();
                self.capture_enabled = true;
                self.debug_logging = false;
                self.pause_game_on_trigger = false;
                self.language = Language::English;
                {
                    let mut state = app_state().lock().unwrap();
                    let last_process = std::mem::take(&mut state.config.last_process);
                    let previous = state.config.clone();
                    state.config = AppConfig {
                        last_process,
                        ..AppConfig::default()
                    };
                    state.config.copy_focus_from(&previous);
                }
                self.apply_changes();
                startup::set_enabled(false);
            }
        });

        let features_locked = app_state().lock().unwrap().features_locked();
        ui.add_enabled_ui(!features_locked, |ui| {
            match startup::enabled() {
                Ok(mut enabled) => {
                    if ui
                        .checkbox(
                            &mut enabled,
                            game_text(
                                language,
                                "Start with Windows (applies immediately)",
                                "开机自启动（即时生效）",
                            ),
                        )
                        .changed()
                    {
                        startup::set_enabled(enabled);
                    }
                }
                Err(error) => {
                    ui.colored_label(
                        theme::MUTED,
                        format!(
                            "{}: {error}",
                            game_text(
                                language,
                                "Cannot read startup setting",
                                "无法读取开机自启动设置"
                            )
                        ),
                    );
                }
            }
            ui.checkbox(&mut self.capture_enabled, text.capture);
            ui.checkbox(&mut self.debug_logging, text.debug);
            let mut auto_load = app_state().lock().unwrap().config.auto_load_process;
            if ui
                .checkbox(
                    &mut auto_load,
                    game_text(
                        language,
                        "Automatically load the last process",
                        "自动加载上次的进程",
                    ),
                )
                .changed()
            {
                app_state().lock().unwrap().config.auto_load_process = auto_load;
                let mut saved = load_config();
                saved.auto_load_process = auto_load;
                if let Err(error) = save_config(&saved) {
                    set_status(&format!("{} - {error}", tr(language).save_failed));
                }
                if auto_load {
                    self.refresh_processes();
                }
            }
            if ui
                .checkbox(
                    &mut self.pause_game_on_trigger,
                    game_text(
                        language,
                        "Pause game on Xbox / keyboard trigger",
                        "Xbox / 键盘触发时暂停游戏（即时生效）",
                    ),
                )
                .changed()
            {
                app_state().lock().unwrap().config.pause_game_on_trigger =
                    self.pause_game_on_trigger;
            }
            ui.separator();
            self.focus_controls(ui);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(text.logs).clicked() {
                    self.debug_window_open = true;
                }
                if ui
                    .button(game_text(language, "Open data folder", "打开数据目录"))
                    .clicked()
                {
                    open_data_dir(language);
                }
            });
        });
    }

    fn process_picker(&mut self, ctx: &egui::Context) {
        if !self.process_picker_open {
            return;
        }
        let language = self.language;
        let remembered = remembered_process();
        let mut open = self.process_picker_open;
        let mut selected = None;
        egui::Window::new(game_text(language, "Select game process", "选择游戏进程"))
            .id(egui::Id::new(PROCESS_PICKER_WINDOW_ID))
            .open(&mut open)
            .default_size(vec2(390.0, 320.0))
            .show(ctx, |ui| {
                ui.label(game_text(
                    language,
                    "Select the game's executable. Only accessible processes are listed.",
                    "请选择游戏的可执行进程，仅列出当前有权限控制的进程。",
                ));
                ui.horizontal(|ui| {
                    ui.add(
                        TextEdit::singleline(&mut self.process_filter).hint_text(game_text(
                            language,
                            "Filter by name or PID",
                            "按名称或 PID 筛选",
                        )),
                    );
                    if ui.button(game_text(language, "Refresh", "刷新")).clicked() {
                        self.refresh_processes();
                    }
                });
                if !remembered.is_empty() {
                    ui.label(
                        RichText::new(tr(language).picker_order_hint)
                            .small()
                            .color(theme::MUTED),
                    );
                }
                let filter = self.process_filter.trim().to_lowercase();
                // Rebinding the same game is the common case, and the list is long,
                // so the executable used last time is pulled to the top.
                let mut order: Vec<usize> = (0..self.processes.len())
                    .filter(|&index| {
                        let process = &self.processes[index];
                        !process.exited()
                            && format!("{} (PID {})", process.name, process.pid)
                                .to_lowercase()
                                .contains(&filter)
                    })
                    .collect();
                order.sort_by_key(|&index| {
                    !is_remembered_process(&self.processes[index], &remembered)
                });
                ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                    for &index in &order {
                        let process = &self.processes[index];
                        let label = format!("{} (PID {})", process.name, process.pid);
                        if ui
                            .selectable_label(is_remembered_process(process, &remembered), label)
                            .clicked()
                        {
                            selected = Some(index);
                        }
                    }
                });
            });
        if let Some(index) = selected {
            let name = self.processes[index].name.clone();
            let result = {
                let mut state = app_state().lock().expect("app state mutex poisoned");
                let result = state.bound_process.as_mut().map(|p| p.restore()).transpose();
                if result.is_ok() {
                    state.bind_process(self.processes.remove(index));
                    state.status =
                        game_text(language, "Game process bound", "游戏进程已绑定").into();
                }
                result
            };
            match result {
                Ok(_) => {
                    open = false;
                    remember_process(&name);
                }
                Err(error) => report_process_error(language, error),
            }
        }
        self.process_picker_open = open;
        if !open {
            self.processes.clear();
        }
    }
}

impl eframe::App for MapperApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        {
            let mut state = app_state().lock().unwrap();
            if state.bound_process.as_ref().is_some_and(|p| p.exited()) {
                state.bound_process = None;
                state.config.copy_focus_from(&load_config());
                state.status = game_text(
                    self.language,
                    "Bound game exited; select a process again",
                    "绑定的游戏已退出，请重新选择进程",
                )
                .into();
            }
        }
        self.macros.tick(ctx, self.settings_tab == 2);
        self.update_shortcut_capture(ctx);
        let trainer_target = {
            let state = app_state().lock().unwrap();
            state
                .bound_process
                .as_ref()
                .and_then(|p| p.trainer_target(state.binding_generation).ok())
        };
        self.trainer.sync_target(trainer_target);
        if self.trainer.exit_ready() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        #[cfg(debug_assertions)]
        if let Ok(path) = env::var("SLACKINPUT_UI_SNAPSHOT") {
            use eframe::icon_data::IconDataExt;
            self.snapshot_frames += 1;
            if self.snapshot_frames == 8 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            }
            for event in ctx.input(|input| input.events.clone()) {
                if let egui::Event::Screenshot { image, .. } = event {
                    let icon = egui::IconData {
                        width: image.width() as u32,
                        height: image.height() as u32,
                        rgba: image.pixels.iter().flat_map(|c| c.to_array()).collect(),
                    };
                    fs::write(&path, icon.to_png_bytes().expect("encode UI snapshot"))
                        .expect("save UI snapshot");
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
        if ctx.input(|input| input.viewport().close_requested()) {
            let trainer_ready = self.trainer.prepare_exit();
            if !trainer_ready {
                self.settings_tab = 1;
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            }
            let result = {
                let mut state = app_state().lock().expect("app state mutex poisoned");
                let result = state.bound_process.as_mut().map(|p| p.restore()).transpose();
                if result.is_ok() && trainer_ready {
                    state.config.capture_enabled = false;
                }
                result
            };
            if let Err(error) = result {
                self.trainer.cancel_exit();
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                report_process_error(self.language, error);
            }
        }
        let (status, logs, connected_devices) = {
            let state = app_state().lock().expect("app state mutex poisoned");
            (
                state.status.clone(),
                state.logs.clone(),
                state.connected_devices.clone(),
            )
        };
        let text = tr(self.language);
        let was_debug_window_open = self.debug_window_open;

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(theme::BG)
                    .inner_margin(egui::Margin::same(14))
                    .stroke(egui::Stroke::new(2.0, theme::EDGE)),
            )
            .show(ctx, |ui| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
                let mut controls_left = ui.max_rect().right();
                let header = ui.horizontal(|ui| {
                    ui.image((self.icon.id(), vec2(34.0, 34.0)))
                        .on_hover_text(text.title);
                    ui.vertical(|ui| {
                        ui.add(egui::Label::new(
                            RichText::new("SLACK INPUT")
                                .monospace()
                                .size(20.0)
                                .strong()
                                .color(theme::MINT),
                        ));
                        ui.label(
                            RichText::new(format!(
                                "{} · v{}",
                                game_text(
                                    self.language,
                                    "GAME COMPANION",
                                    "游戏助手 / 随时切换",
                                ),
                                env!("CARGO_PKG_VERSION"),
                            ))
                            .small()
                            .color(theme::MUTED),
                        );
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .button("X")
                            .on_hover_text(game_text(self.language, "Exit", "退出"))
                            .clicked()
                        {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        let minimize = ui.button("_").on_hover_text(game_text(
                            self.language,
                            "Minimize to tray",
                            "最小化到托盘",
                        ));
                        controls_left = minimize.rect.left();
                        if minimize.clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                        }
                    });
                });
                if let Some(titlebar) = &self.titlebar {
                    // Floating egui windows drawn over the caption strip keep
                    // client input there, or the native hit-test eats their
                    // title bar and close-button clicks.
                    let mut floating_windows = Vec::new();
                    for (open, id) in [
                        (self.process_picker_open, PROCESS_PICKER_WINDOW_ID),
                        (self.debug_window_open, DEBUG_LOG_WINDOW_ID),
                    ] {
                        if let Some(rect) = open
                            .then(|| ctx.memory(|mem| mem.area_rect(egui::Id::new(id))))
                            .flatten()
                        {
                            floating_windows.push(rect);
                        }
                    }
                    titlebar.update(
                        egui::Rect::from_min_max(
                            egui::Pos2::ZERO,
                            egui::pos2(controls_left - 8.0, header.response.rect.bottom() + 8.0),
                        ),
                        &floating_windows,
                        ctx.pixels_per_point(),
                    );
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    let active = !connected_devices.is_empty();
                    ui.label(
                        RichText::new(if active { "■" } else { "□" }).color(if active {
                            theme::MINT
                        } else {
                            theme::GOLD
                        }),
                    );
                    ui.label(format!(
                        "{} / {}",
                        text.controller,
                        if active { text.active } else { text.inactive }
                    ));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        egui::ComboBox::from_id_salt("language_combo")
                            .width(100.0)
                            .selected_text(self.language.display_name())
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.language,
                                    Language::English,
                                    Language::English.display_name(),
                                );
                                ui.selectable_value(
                                    &mut self.language,
                                    Language::Chinese,
                                    Language::Chinese.display_name(),
                                );
                            })
                            .response
                            .on_hover_text(text.language);
                    });
                });
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    for (index, label) in [
                        game_text(self.language, "Game", "游戏"),
                        game_text(self.language, "Trainer", "修改器"),
                        game_text(self.language, "Key macros", "按键宏"),
                        game_text(self.language, "Shortcuts", "快捷键"),
                        game_text(self.language, "Settings", "设置"),
                    ]
                    .iter()
                    .enumerate()
                    {
                        ui.selectable_value(&mut self.settings_tab, index, *label);
                    }
                });
                ui.add_space(6.0);
                let content_height = (ui.available_height() - 42.0).max(80.0);
                if self.settings_tab == 0 {
                    ui.allocate_ui_with_layout(
                        vec2(ui.available_width(), content_height),
                        Layout::top_down(Align::Min),
                        |ui| {
                            theme::card().show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                self.game_controls(ui);
                            });
                        },
                    );
                } else {
                    let scroll = ScrollArea::vertical().id_salt(("tab_scroll", self.settings_tab));
                    #[cfg(debug_assertions)]
                    let scroll = if env::var_os("SLACKINPUT_UI_SNAPSHOT").is_some()
                        && env::var_os("SLACKINPUT_UI_SCROLL_BOTTOM").is_some()
                        && self.snapshot_frames == 2
                    {
                        scroll.vertical_scroll_offset(10000.0)
                    } else {
                        scroll
                    };
                    scroll
                        .auto_shrink([false, false])
                        .max_height(content_height)
                        .scroll_bar_visibility(
                            egui::scroll_area::ScrollBarVisibility::AlwaysVisible,
                        )
                        .show(ui, |ui| {
                            theme::card().show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                if self.settings_tab == 1 {
                                    self.trainer.show(ui, self.language);
                                } else if self.settings_tab == 2 {
                                    self.macros.show(ui, self.language);
                                } else if self.settings_tab == 3 {
                                    self.shortcuts_tab(ui);
                                } else {
                                    self.settings_controls(ui);
                                }
                            });
                        });
                }
                ui.separator();
                ui.add_space(2.0);
                ui.label(RichText::new(status).small().color(theme::MUTED));
            });

        if app_state().lock().unwrap().features_locked() {
            self.process_picker_open = false;
        }
        self.process_picker(ctx);

        if self.debug_window_open {
            egui::Window::new(text.logs)
                .id(egui::Id::new(DEBUG_LOG_WINDOW_ID))
                .open(&mut self.debug_window_open)
                .resizable(true)
                .vscroll(true)
                .default_size(vec2(390.0, 340.0))
                .show(ctx, |ui| {
                    ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new(logs.as_str()).monospace()).wrap());
                    });
                });
        }

        if was_debug_window_open && !self.debug_window_open {
            self.debug_logging = false;
            app_state()
                .lock()
                .expect("app state mutex poisoned")
                .config
                .debug_logging = false;
        }

        ctx.request_repaint_after(Duration::from_millis(100));
    }
}

const PRESETS: [&str; 6] = [
    "Ctrl+Win+Left",
    "Ctrl+Win+Right",
    "Ctrl+Win+Up",
    "Ctrl+Win+Down",
    "Alt+Tab",
    "Ctrl+Shift+Esc",
];

fn main() -> Result<()> {
    let mut config = load_config();
    #[cfg(debug_assertions)]
    if env::var_os("SLACKINPUT_UI_SNAPSHOT").is_some() {
        config.capture_enabled = false;
        config.keyboard_trigger.clear();
        config.speed_up.clear();
        config.speed_down.clear();
        config.auto_load_process = false;
        config.language = env::var("SLACKINPUT_UI_LANGUAGE")
            .ok()
            .as_deref()
            .and_then(Language::from_config_value)
            .unwrap_or(Language::Chinese);
    }
    if env::args().any(|arg| arg == "--debug") {
        config.debug_logging = true;
    }

    let mapping_keys = parse_mapping(&config.mapping_text)
        .unwrap_or_else(|| parse_mapping(PRESETS[0]).expect("default mapping must parse"));
    let _ = APP_STATE.set(Mutex::new(AppState::new(config, mapping_keys)));

    spawn_raw_input_thread();
    spawn_process_auto_loader();
    spawn_anti_cheat_monitor();
    push_log_force(tr(current_language()).startup_log);

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([480.0, 560.0])
            .with_min_inner_size([440.0, 480.0])
            .with_resizable(true)
            .with_decorations(false)
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png"))
                    .expect("embedded icon"),
            )
            .with_maximize_button(false)
            .with_title(concat!("SlackInput v", env!("CARGO_PKG_VERSION"))),
        ..Default::default()
    };

    let result = eframe::run_native(
        "SlackInput",
        native_options,
        Box::new(|cc| {
            configure_egui_fonts(&cc.egui_ctx);
            theme::install(&cc.egui_ctx);
            Ok(Box::new(MapperApp::new(cc)))
        }),
    );

    // The global state is not dropped on exit. Explicitly release its binding.
    {
        let mut state = app_state().lock().expect("app state mutex poisoned");
        state.config.capture_enabled = false;
        if let Some(mut process) = state.bound_process.take() {
            if let Err(error) = process.restore() {
                drop(state);
                push_log_force(&format!("Resume on exit failed: {error}"));
            }
        }
    }

    if let Err(error) = result {
        return Err(Error::new(
            windows::core::HRESULT(0x80004005u32 as i32),
            format!("GUI 启动失败: {error}"),
        ));
    }

    Ok(())
}

fn app_state() -> &'static Mutex<AppState> {
    APP_STATE.get().expect("app state not initialized")
}

fn configure_egui_fonts(ctx: &egui::Context) {
    let Some((font_name, font_bytes)) = load_cjk_font() else {
        push_log_force(tr(current_language()).font_missing_log);
        return;
    };

    let mut fonts = FontDefinitions::default();
    fonts
        .font_data
        .insert(font_name.clone(), FontData::from_owned(font_bytes).into());
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, font_name.clone());
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .insert(0, font_name);
    ctx.set_fonts(fonts);
}

fn load_cjk_font() -> Option<(String, Vec<u8>)> {
    for path in CANDIDATE_CJK_FONTS {
        if let Ok(bytes) = fs::read(path) {
            let name = PathBuf::from(path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("cjk-font")
                .to_string();
            return Some((name, bytes));
        }
    }
    None
}

// UI-only fixture: never bind, start, suspend or modify a real game for screenshots.
#[cfg(debug_assertions)]
fn snapshot_anti_cheat() -> Option<anti_cheat::Report> {
    env::var_os("SLACKINPUT_UI_SNAPSHOT")?;
    let outcome = match env::var("SLACKINPUT_UI_ANTI_CHEAT").ok()?.as_str() {
        "clear" => anti_cheat::Outcome::NoKnownProcess,
        "detected" => anti_cheat::Outcome::Detected(vec!["BattlEye: beservice.exe".into()]),
        "unknown" => anti_cheat::Outcome::Unavailable("Example: access denied".into()),
        _ => return None,
    };
    Some(anti_cheat::Report {
        outcome,
        checked_at: Instant::now(),
    })
}

fn spawn_anti_cheat_monitor() {
    thread::spawn(|| {
        loop {
            let generation = {
                let state = app_state().lock().unwrap();
                state
                    .bound_process
                    .as_ref()
                    .filter(|p| !p.exited())
                    .map(|_| state.binding_generation)
            };
            if let Some(generation) = generation {
                let report = anti_cheat::scan();
                app_state()
                    .lock()
                    .unwrap()
                    .accept_anti_cheat_report(generation, report);
            }
            thread::sleep(anti_cheat::REFRESH_INTERVAL);
        }
    });
}

fn spawn_process_auto_loader() {
    thread::spawn(|| {
        loop {
            thread::sleep(Duration::from_secs(1));
            let target = app_state().lock().unwrap().auto_load_target();
            let Some(target) = target else {
                continue;
            };
            // Enumerate outside the state lock so input and UI remain responsive.
            let Ok(processes) = process::enumerate() else {
                continue;
            };
            let Some(process) = processes
                .into_iter()
                .find(|p| is_remembered_process(p, &target) && !p.exited())
            else {
                continue;
            };
            let mut state = app_state().lock().unwrap();
            // The user may have disabled loading, unbound, or selected another game.
            if state.auto_load_target().as_deref() == Some(target.as_str()) && !process.exited() {
                state.bind_process(process);
                state.status = game_text(
                    state.config.language,
                    "Last game process loaded automatically",
                    "已自动加载上次的游戏进程",
                )
                .into();
            }
        }
    });
}

fn spawn_raw_input_thread() {
    thread::spawn(|| {
        if let Err(error) = raw_input_thread_main() {
            let text = tr(current_language());
            set_status(&format!("{} - {error}", text.raw_input_failed));
            push_log_force(&format!("{}: {error}", text.raw_input_failed));
        }
    });
}

fn raw_input_thread_main() -> Result<()> {
    let hwnd = create_message_window()?;
    register_raw_input(hwnd)?;
    refresh_connected_devices()?;
    push_log_if_debug(tr(current_language()).raw_input_started);

    unsafe {
        let mut msg = MSG::default();
        loop {
            let status = GetMessageW(&mut msg, None, 0, 0).0;
            if status == -1 {
                return Err(Error::from_thread());
            }
            if status == 0 {
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    Ok(())
}

fn create_message_window() -> Result<HWND> {
    unsafe {
        let instance = HINSTANCE(GetModuleHandleW(None)?.0);
        let class_name = w!("XboxGuideMapperRawInputWindow");
        let wc = WNDCLASSW {
            hInstance: instance,
            lpszClassName: class_name,
            lpfnWndProc: Some(raw_input_wndproc),
            ..Default::default()
        };

        if RegisterClassW(&wc) == 0 {
            return Err(Error::from_thread());
        }

        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class_name,
            w!(""),
            WINDOW_STYLE::default(),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance),
            None,
        )
    }
}

fn register_raw_input(hwnd: HWND) -> Result<()> {
    let devices = [
        RAWINPUTDEVICE {
            usUsagePage: HID_USAGE_PAGE_GENERIC_DESKTOP,
            usUsage: HID_USAGE_GAMEPAD,
            dwFlags: RIDEV_INPUTSINK | RIDEV_DEVNOTIFY,
            hwndTarget: hwnd,
        },
        RAWINPUTDEVICE {
            usUsagePage: HID_USAGE_PAGE_GENERIC_DESKTOP,
            usUsage: HID_USAGE_JOYSTICK,
            dwFlags: RIDEV_INPUTSINK | RIDEV_DEVNOTIFY,
            hwndTarget: hwnd,
        },
    ];

    unsafe { RegisterRawInputDevices(&devices, std::mem::size_of::<RAWINPUTDEVICE>() as u32) }
}

unsafe extern "system" fn raw_input_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_INPUT => {
            if let Err(error) = log_raw_input(lparam) {
                push_log_if_debug(&format!("Raw Input 处理失败: {error}"));
                set_status(tr(current_language()).raw_input_process_failed);
            }
            LRESULT(0)
        }
        WM_INPUT_DEVICE_CHANGE => {
            let event = if wparam.0 as u32 == GIDC_ARRIVAL {
                "arrival"
            } else if wparam.0 as u32 == GIDC_REMOVAL {
                "removal"
            } else {
                "change"
            };
            let _ = refresh_connected_devices();
            push_log_if_debug(&format!(
                "Raw Input device {}: wParam={} lParam={:#x}",
                event, wparam.0, lparam.0
            ));
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn log_raw_input(lparam: LPARAM) -> Result<()> {
    let mut size = 0u32;
    let hrawinput = HRAWINPUT(lparam.0 as *mut c_void);
    let header_size = std::mem::size_of::<RAWINPUTHEADER>() as u32;

    unsafe {
        let size_result = GetRawInputData(hrawinput, RID_INPUT, None, &mut size, header_size);
        if size_result == u32::MAX {
            return Err(Error::from_thread());
        }
    }

    let mut buffer = vec![0u8; size as usize];
    unsafe {
        let read = GetRawInputData(
            hrawinput,
            RID_INPUT,
            Some(buffer.as_mut_ptr() as *mut c_void),
            &mut size,
            header_size,
        );
        if read == u32::MAX {
            return Err(Error::from_thread());
        }
    }

    let raw = unsafe { &*(buffer.as_ptr() as *const RAWINPUT) };
    if raw.header.dwType != RIM_TYPEHID.0 {
        return Ok(());
    }

    let hid = unsafe { raw.data.hid };
    let report_size = hid.dwSizeHid as usize;
    let report_count = hid.dwCount as usize;
    if report_size == 0 || report_count == 0 {
        return Ok(());
    }

    let device = raw.header.hDevice;
    let device_name =
        raw_device_name(device).unwrap_or_else(|_| format!("HANDLE({:#x})", device.0 as usize));
    let hid_info = raw_hid_info(device).ok();
    let bytes_ptr = unsafe { addr_of!(raw.data.hid.bRawData) as *const u8 };
    let reports = unsafe { std::slice::from_raw_parts(bytes_ptr, report_size * report_count) };

    for (index, report) in reports.chunks(report_size).enumerate() {
        let event = next_hid_event(device.0 as isize);
        if debug_enabled() {
            let hex = report
                .iter()
                .map(|byte| format!("{byte:02X}"))
                .collect::<Vec<_>>()
                .join(" ");
            if let Some(info) = hid_info {
                push_log_if_debug(&format!(
                    "Raw HID 事件: {} report={} bytes={} event={} vid={:04X} pid={:04X} usage={:04X}:{:04X} data={}",
                    device_name,
                    index,
                    report.len(),
                    event,
                    info.dwVendorId,
                    info.dwProductId,
                    info.usUsagePage,
                    info.usUsage,
                    hex
                ));
            } else {
                push_log_if_debug(&format!(
                    "Raw HID 事件: {} report={} bytes={} event={} data={}",
                    device_name,
                    index,
                    report.len(),
                    event,
                    hex
                ));
            }
        }

        if should_trigger_from_raw_hid(hid_info, report) {
            remember_connected_device(&device_name);
            trigger_mapping(&device_name)?;
        }
    }

    Ok(())
}

fn should_trigger_from_raw_hid(hid_info: Option<RID_DEVICE_INFO_HID>, report: &[u8]) -> bool {
    let Some(info) = hid_info else {
        return false;
    };

    info.dwVendorId == 0x045E
        && info.dwProductId == 0x02E0
        && info.usUsagePage == HID_USAGE_PAGE_GENERIC_DESKTOP
        && info.usUsage == HID_USAGE_GAMEPAD
        && report == RAW_GUIDE_TRIGGER_REPORT
}

fn trigger_mapping(device_name: &str) -> Result<()> {
    if macros::is_recording() {
        return Ok(());
    }
    if !capture_enabled() {
        push_log_if_debug(tr(current_language()).capture_disabled_log);
        return Ok(());
    }
    if !trigger_ready() {
        return Ok(());
    }

    let mapping = current_mapping_keys();
    let label = mapping_label();
    let text = tr(current_language());
    set_status(&format!("{} {}", text.triggered_prefix, label));
    push_log_if_debug(&format!(
        "{}: {} -> {}",
        text.trigger_log_prefix, device_name, label
    ));
    let hotkey_result = send_hotkey(&mapping);
    // Suspension failure must not suppress the configured shortcut.
    let pause_result = {
        let mut state = app_state().lock().expect("app state mutex poisoned");
        if state.config.capture_enabled
            && state.config.pause_game_on_trigger
            && !state.features_locked()
        {
            state
                .bound_process
                .as_mut()
                .map(|process| process.suspend())
                .transpose()
        } else {
            Ok(None)
        }
    };
    if let Err(error) = pause_result {
        report_process_error(current_language(), error);
    }
    hotkey_result
}

fn send_hotkey(keys: &[VIRTUAL_KEY]) -> Result<()> {
    let mut inputs = Vec::with_capacity(keys.len() * 2);
    for &key in keys {
        inputs.push(key_input(key, false));
    }
    for &key in keys.iter().rev() {
        inputs.push(key_input(key, true));
    }

    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        return Err(Error::from_thread());
    }
    Ok(())
}

fn key_input(vk: VIRTUAL_KEY, key_up: bool) -> INPUT {
    let flags = if key_up {
        KEYEVENTF_KEYUP
    } else {
        Default::default()
    };

    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn trigger_ready() -> bool {
    let now = Instant::now();
    let store = LAST_TRIGGER_AT.get_or_init(|| Mutex::new(now - TRIGGER_COOLDOWN));
    let mut guard = store.lock().expect("trigger mutex poisoned");
    if now.duration_since(*guard) < TRIGGER_COOLDOWN {
        return false;
    }
    *guard = now;
    true
}

fn captured_key(key: egui::Key, modifiers: egui::Modifiers, win: bool) -> Option<String> {
    let name = match key {
        egui::Key::ArrowLeft => "Left",
        egui::Key::ArrowRight => "Right",
        egui::Key::ArrowUp => "Up",
        egui::Key::ArrowDown => "Down",
        egui::Key::Escape => "Esc",
        _ => key.name(),
    };
    parse_token(name)?;
    let mut parts = Vec::new();
    if modifiers.ctrl {
        parts.push("Ctrl");
    }
    if modifiers.alt {
        parts.push("Alt");
    }
    if modifiers.shift {
        parts.push("Shift");
    }
    if win {
        parts.push("Win");
    }
    parts.push(name);
    Some(parts.join("+"))
}

fn parse_keyboard_trigger(text: &str) -> Option<Option<(u32, u32)>> {
    if text.trim().is_empty() {
        return Some(None);
    }
    let mut modifiers = 0;
    let mut key = None;
    for token in text.split('+') {
        let parsed = parse_token(token)?;
        let modifier = match parsed {
            VK_CONTROL => 2,
            VK_MENU => 1,
            VK_SHIFT => 4,
            VK_LWIN => 8,
            _ => 0,
        };
        if modifier != 0 {
            if modifiers & modifier != 0 {
                return None;
            }
            modifiers |= modifier;
        } else if key.replace(parsed.0 as u32).is_some() {
            return None;
        }
    }
    Some(Some((modifiers, key?)))
}

fn configure_keyboard(text: &str) -> std::result::Result<(), String> {
    let config = app_state().lock().unwrap().config.clone();
    configure_shortcuts(text, &config.speed_up, &config.speed_down)
}

fn configure_shortcuts(trigger: &str, up: &str, down: &str) -> std::result::Result<(), String> {
    let keys = parse_shortcuts(trigger, up, down)?;
    keyboard::configure_all(keys).map_err(|e| format!("快捷键注册失败 / Shortcut registration failed: {e}"))
}

fn parse_shortcuts(trigger: &str, up: &str, down: &str) -> std::result::Result<[Option<(u32, u32)>; 3], String> {
    let mut keys = [None; 3];
    for (i, text) in [trigger, up, down].iter().enumerate() {
        keys[i] = parse_keyboard_trigger(text)
            .ok_or_else(|| format!("快捷键格式无效 / Invalid shortcut: {text}"))?;
        if keys[i].is_some() && keys[..i].contains(&keys[i]) {
            return Err("快捷键不能重复 / Shortcuts must be distinct".into());
        }
    }
    Ok(keys)
}

fn speed_shortcut_target() -> Option<(u64, u32)> {
    let state = app_state().lock().unwrap();
    if !state.config.capture_enabled || state.features_locked() || macros::is_recording() { return None; }
    state.bound_process.as_ref().filter(|p|
        !p.exited() && !p.paused && process::foreground_pid() == p.pid)
        .map(|p| (state.binding_generation, p.pid))
}

fn change_game_speed(requested: Option<u32>, up: bool, expected: Option<(u64, u32)>) {
    let mut state = app_state().lock().unwrap();
    let generation = state.binding_generation;
    if requested.is_none() && (!state.config.capture_enabled || macros::is_recording()
        || expected != state.bound_process.as_ref().map(|p| (generation, p.pid))
        || state.bound_process.as_ref().is_none_or(|p| process::foreground_pid() != p.pid)) {
        return;
    }
    if state.features_locked() && requested != Some(1) { return; }
    let Some(process) = state.bound_process.as_mut().filter(|p| !p.exited()) else { return; };
    if process.paused && requested != Some(1) { return; }
    let current = process.speed.as_ref().map_or(1, |s| s.multiplier);
    let next = requested.unwrap_or_else(|| if up { (current * 2).min(4) } else { (current / 2).max(1) });
    if next == current && requested != Some(1) { return; }
    if next == 1 && process.speed.is_none() { return; }
    let result = (|| -> std::result::Result<(), String> {
        if process.speed.is_none() {
            process.speed = Some(trainer::SpeedSession::open(&process.trainer_target(generation)?)?);
        }
        process.speed.as_mut().unwrap().set(next)
    })();
    state.status = match result {
        Ok(()) => format!("游戏计时倍率 / Game clock: x{next}（实际效果取决于游戏 / effect depends on game）"),
        Err(e) => format!("游戏加速失败 / Game speed failed: {e}"),
    };
}

fn keyboard_trigger_released(modifiers: u32, key: u32) -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    let released = |vk: i32| unsafe { GetAsyncKeyState(vk) >= 0 };
    released(key as i32)
        && (modifiers & 1 == 0 || released(VK_MENU.0 as i32))
        && (modifiers & 2 == 0 || released(VK_CONTROL.0 as i32))
        && (modifiers & 4 == 0 || released(VK_SHIFT.0 as i32))
        && (modifiers & 8 == 0 || (released(0x5B) && released(0x5C)))
}

fn parse_mapping(text: &str) -> Option<Vec<VIRTUAL_KEY>> {
    let tokens: Vec<&str> = text
        .split('+')
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .collect();
    if tokens.is_empty() {
        return None;
    }

    let mut keys = Vec::with_capacity(tokens.len());
    for token in tokens {
        keys.push(parse_token(token)?);
    }
    Some(keys)
}

fn parse_token(token: &str) -> Option<VIRTUAL_KEY> {
    let upper = token.trim().to_ascii_uppercase();
    let key = match upper.as_str() {
        "CTRL" | "CONTROL" => VK_CONTROL,
        "WIN" | "WINDOWS" => VK_LWIN,
        "ALT" => VK_MENU,
        "SHIFT" => VK_SHIFT,
        "LEFT" => VK_LEFT,
        "RIGHT" => VK_RIGHT,
        "UP" => VK_UP,
        "DOWN" => VK_DOWN,
        "TAB" => VK_TAB,
        "ESC" | "ESCAPE" => VK_ESCAPE,
        "ENTER" | "RETURN" => VK_RETURN,
        "SPACE" => VK_SPACE,
        "NUMADD" => VIRTUAL_KEY(0x6B),
        "NUMSUBTRACT" => VIRTUAL_KEY(0x6D),
        "BACKSPACE" => VIRTUAL_KEY(0x08),
        "INSERT" => VIRTUAL_KEY(0x2D),
        "DELETE" | "DEL" => VIRTUAL_KEY(0x2E),
        "HOME" => VK_HOME,
        "END" => VK_END,
        "PAGEUP" | "PRIOR" => VK_PRIOR,
        "PAGEDOWN" | "NEXT" => VK_NEXT,
        _ if upper.len() == 1 => {
            let ch = upper.as_bytes()[0];
            if ch.is_ascii_uppercase() || ch.is_ascii_digit() {
                VIRTUAL_KEY(ch as u16)
            } else {
                return None;
            }
        }
        _ if upper.starts_with('F') => {
            let number = upper[1..].parse::<u16>().ok()?;
            if (1..=24).contains(&number) {
                VIRTUAL_KEY(0x70 + number - 1)
            } else {
                return None;
            }
        }
        _ => return None,
    };
    Some(key)
}

fn preset_index(mapping: &str) -> Option<usize> {
    PRESETS.iter().position(|preset| *preset == mapping)
}

fn mapping_label() -> String {
    app_state()
        .lock()
        .expect("app state mutex poisoned")
        .config
        .mapping_text
        .clone()
}

fn current_mapping_keys() -> Vec<VIRTUAL_KEY> {
    app_state()
        .lock()
        .expect("app state mutex poisoned")
        .mapping_keys
        .clone()
}

fn capture_enabled() -> bool {
    app_state()
        .lock()
        .expect("app state mutex poisoned")
        .config
        .capture_enabled
}

fn debug_enabled() -> bool {
    app_state()
        .lock()
        .expect("app state mutex poisoned")
        .config
        .debug_logging
}

fn current_language() -> Language {
    app_state()
        .lock()
        .expect("app state mutex poisoned")
        .config
        .language
}

fn set_status(text: &str) {
    app_state().lock().expect("app state mutex poisoned").status = text.to_string();
}

fn refresh_connected_devices() -> Result<()> {
    let names = enumerate_matching_devices()?;
    app_state()
        .lock()
        .expect("app state mutex poisoned")
        .connected_devices = names;
    Ok(())
}

fn remember_connected_device(device_name: &str) {
    let mut state = app_state().lock().expect("app state mutex poisoned");
    if !state
        .connected_devices
        .iter()
        .any(|name| name == device_name)
    {
        state.connected_devices.push(device_name.to_string());
    }
}

fn push_log_if_debug(text: &str) {
    if debug_enabled() {
        push_log_force(text);
    }
}

fn push_log_force(text: &str) {
    let mut state = app_state().lock().expect("app state mutex poisoned");
    if !state.logs.is_empty() {
        state.logs.push_str("\n");
    }
    state.logs.push_str(text);
    if state.logs.len() > MAX_LOG_CHARS {
        let split_at = state.logs.len().saturating_sub(MAX_LOG_CHARS / 2);
        state.logs = state.logs.split_off(split_at);
    }
}

fn config_dir() -> PathBuf {
    env::var("APPDATA")
        .map(|appdata| PathBuf::from(appdata).join("SlackInput"))
        .unwrap_or_else(|_| {
            env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.to_path_buf()))
                .unwrap_or_else(|| PathBuf::from("."))
        })
}

fn open_data_dir(language: Language) {
    let dir = config_dir();
    // The folder is only created on the first save, so make sure it exists
    // before handing it to Explorer.
    if let Err(error) = fs::create_dir_all(&dir) {
        set_status(&format!(
            "{}: {error}",
            game_text(language, "Failed to create data folder", "无法创建数据目录")
        ));
        return;
    }
    if let Err(error) = std::process::Command::new("explorer").arg(&dir).spawn() {
        set_status(&format!(
            "{}: {error}",
            game_text(language, "Failed to open data folder", "无法打开数据目录")
        ));
    }
}

fn config_path() -> PathBuf {
    config_dir().join("SlackInput.ini")
}

fn legacy_config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(exe) = env::current_exe() {
        paths.push(exe.with_file_name("SlackInput.ini"));
        paths.push(exe.with_file_name("xbox-guide-mapper.ini"));
    }
    paths
}

fn load_config() -> AppConfig {
    let primary = config_path();
    let contents = if primary.exists() {
        fs::read_to_string(primary).ok()
    } else {
        legacy_config_paths()
            .into_iter()
            .find_map(|p| fs::read_to_string(p).ok())
    };
    let contents = match contents {
        Some(contents) => contents,
        None => return AppConfig::default(),
    };

    parse_config(&contents)
}

fn parse_config(contents: &str) -> AppConfig {
    let mut config = AppConfig::default();
    for line in contents.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "keyboard_trigger" => config.keyboard_trigger = value.trim().to_string(),
            "speed_up" => config.speed_up = value.trim().to_string(),
            "speed_down" => config.speed_down = value.trim().to_string(),
            "mapping" => config.mapping_text = value.trim().to_string(),
            "capture_enabled" => config.capture_enabled = value.trim().eq_ignore_ascii_case("true"),
            "debug_logging" => config.debug_logging = value.trim().eq_ignore_ascii_case("true"),
            "pause_game_on_trigger" => {
                config.pause_game_on_trigger = value.trim().eq_ignore_ascii_case("true")
            }
            "last_process" => config.last_process = value.trim().to_string(),
            "auto_load_process" => {
                config.auto_load_process = value.trim().eq_ignore_ascii_case("true")
            }
            "focus_style" => config.focus_style = marker::Style::parse(value),
            "focus_enabled" => config.focus_enabled = value.trim().eq_ignore_ascii_case("true"),
            "focus_locked" => config.focus_locked = value.trim().eq_ignore_ascii_case("true"),
            "focus_diameter" => {
                config.focus_diameter = value.trim().parse::<u8>().unwrap_or(16).clamp(8, 36)
            }
            "focus_opacity" => {
                config.focus_opacity = value.trim().parse::<u8>().unwrap_or(190).clamp(50, 255)
            }
            "focus_x" => config.focus_x = parse_focus_position(value),
            "focus_y" => config.focus_y = parse_focus_position(value),
            "language" => {
                if let Some(language) = Language::from_config_value(value) {
                    config.language = language;
                }
            }
            _ => {}
        }
    }

    if parse_mapping(&config.mapping_text).is_none() {
        config.mapping_text = AppConfig::default().mapping_text;
    }

    config
}

fn save_config(config: &AppConfig) -> Result<()> {
    let dir = config_dir();
    if !dir.exists() {
        fs::create_dir_all(&dir).map_err(|error| {
            Error::new(
                windows::core::HRESULT(0x80070020u32 as i32),
                format!("创建配置目录失败: {error}"),
            )
        })?;
    }
    fs::write(config_path(), config_body(config)).map_err(|error| {
        Error::new(
            windows::core::HRESULT(0x80070020u32 as i32),
            format!("保存配置失败: {error}"),
        )
    })
}

fn config_body(config: &AppConfig) -> String {
    let mut body = format!(
        "mapping={}\ncapture_enabled={}\ndebug_logging={}\nlanguage={}\npause_game_on_trigger={}\nlast_process={}\nauto_load_process={}\nfocus_enabled={}\nfocus_locked={}\nfocus_diameter={}\nfocus_opacity={}\nfocus_x={}\nfocus_y={}\nfocus_style={}\n",
        config.mapping_text,
        config.capture_enabled,
        config.debug_logging,
        config.language.config_value(),
        config.pause_game_on_trigger,
        config.last_process,
        config.auto_load_process,
        config.focus_enabled,
        config.focus_locked,
        config.focus_diameter,
        config.focus_opacity,
        config.focus_x,
        config.focus_y,
        config.focus_style.key()
    );
    body.push_str(&format!("keyboard_trigger={}\n", config.keyboard_trigger));
    body.push_str(&format!("speed_up={}\nspeed_down={}\n", config.speed_up, config.speed_down));
    body
}

fn parse_focus_position(value: &str) -> f32 {
    value
        .trim()
        .parse::<f32>()
        .ok()
        .filter(|v| v.is_finite())
        .unwrap_or(0.5)
        .clamp(0.0, 1.0)
}

#[cfg(test)]
mod config_tests {
    use super::*;

    #[test]
    fn anti_cheat_results_cannot_cross_binding_sessions() {
        let mut state = AppState::new(AppConfig::default(), vec![]);
        let report = anti_cheat::Report {
            outcome: anti_cheat::Outcome::NoKnownProcess,
            checked_at: Instant::now(),
        };
        state.accept_anti_cheat_report(0, report.clone());
        assert!(state.anti_cheat_report.is_none());
        state.bound_process = Some(process::BoundProcess::current_for_window_test());
        state.binding_generation = 2;
        state.accept_anti_cheat_report(1, report.clone());
        assert!(state.anti_cheat_report.is_none());
        state.accept_anti_cheat_report(2, report);
        assert!(state.anti_cheat_report.is_some());
    }

    #[test]
    fn anti_cheat_lock_survives_failed_scans_and_clears_on_confirmed_clear() {
        let mut state = AppState::new(AppConfig::default(), Vec::new());
        state.bound_process = Some(process::BoundProcess::current_for_window_test());
        let report = |outcome| anti_cheat::Report {
            outcome,
            checked_at: Instant::now(),
        };
        state.accept_anti_cheat_report(
            0,
            report(anti_cheat::Outcome::Detected(vec!["test".into()])),
        );
        assert!(state.features_locked());
        state.accept_anti_cheat_report(0, report(anti_cheat::Outcome::Unavailable("test".into())));
        assert!(state.features_locked());
        state.accept_anti_cheat_report(1, report(anti_cheat::Outcome::NoKnownProcess));
        assert!(state.features_locked());
        state.accept_anti_cheat_report(0, report(anti_cheat::Outcome::NoKnownProcess));
        assert!(!state.features_locked());
        state.anti_cheat_locked = true;
        state.bound_process = None;
        assert!(!state.features_locked());
    }

    #[test]
    fn saving_one_game_preserves_another_games_profile() {
        let dir = env::temp_dir().join(format!(
            "slackinput-profiles-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let a = dir.join("a.ini");
        let b = dir.join("b.ini");
        let mut config_a = AppConfig {
            focus_enabled: true,
            focus_x: 0.2,
            ..Default::default()
        };
        let config_b = AppConfig {
            focus_style: marker::Style::X,
            focus_y: 0.8,
            ..Default::default()
        };
        write_focus_profile(&a, &config_a).unwrap();
        write_focus_profile(&b, &config_b).unwrap();
        config_a.focus_locked = true;
        config_a.focus_x = 0.6;
        write_focus_profile(&a, &config_a).unwrap();
        for (path, expected) in [(&a, &config_a), (&b, &config_b)] {
            let loaded = parse_config(&fs::read_to_string(path).unwrap());
            assert_eq!(focus_body(&loaded), focus_body(expected));
            fs::remove_file(path).unwrap();
        }
        fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn game_profiles_are_case_insensitive_and_path_safe() {
        assert_eq!(
            focus_profile_path("Game.EXE"),
            focus_profile_path("game.exe")
        );
        assert_ne!(
            focus_profile_path("game-a.exe"),
            focus_profile_path("game-b.exe")
        );
        assert_eq!(
            focus_profile_path("../game.exe").parent().unwrap(),
            config_dir().join("games")
        );
    }

    #[test]
    fn focus_profiles_round_trip_without_capturing_general_settings() {
        let game_a = AppConfig {
            focus_enabled: true,
            focus_locked: true,
            focus_style: marker::Style::Crosshair,
            focus_diameter: 28,
            focus_opacity: 220,
            focus_x: 0.25,
            focus_y: 0.75,
            mapping_text: "Alt+Tab".into(),
            pause_game_on_trigger: true,
            ..Default::default()
        };
        let body = focus_body(&game_a);
        assert!(body.lines().all(|line| line.starts_with("focus_")));
        let loaded = parse_config(&body);
        assert_eq!(focus_body(&loaded), body);
        let mut active = AppConfig::default();
        active.copy_focus_from(&loaded);
        assert_eq!(focus_body(&active), body);
        assert_eq!(active.mapping_text, "Ctrl+Win+Left");
        assert!(!active.pause_game_on_trigger);
        let game_b = AppConfig::default();
        active.copy_focus_from(&game_b);
        assert!(!active.focus_enabled);
        active.copy_focus_from(&loaded);
        assert_eq!((active.focus_x, active.focus_y), (0.25, 0.75));
    }

    #[test]
    fn auto_load_waits_for_game_without_overriding_user_choices() {
        let mut state = AppState::new(
            AppConfig {
                last_process: "game.exe".into(),
                ..Default::default()
            },
            vec![],
        );
        assert_eq!(state.auto_load_target().as_deref(), Some("game.exe"));
        state.config.auto_load_process = false;
        assert!(state.auto_load_target().is_none());
        state.config.auto_load_process = true;
        state.auto_load_suppressed = true;
        assert!(state.auto_load_target().is_none());
        state.auto_load_suppressed = false;
        state.bound_process = Some(process::BoundProcess::current_for_window_test());
        assert!(state.auto_load_target().is_none());
        state.bound_process = None;
        assert!(state.auto_load_target().is_some());
        state.config.last_process.clear();
        assert!(state.auto_load_target().is_none());
    }

    #[test]
    fn captured_shortcuts_can_be_parsed() {
        for (key, modifiers, win, expected) in [
            (egui::Key::F8, egui::Modifiers::NONE, false, "F8"),
            (
                egui::Key::ArrowLeft,
                egui::Modifiers::CTRL,
                true,
                "Ctrl+Win+Left",
            ),
            (
                egui::Key::Q,
                egui::Modifiers::CTRL | egui::Modifiers::ALT,
                false,
                "Ctrl+Alt+Q",
            ),
            (egui::Key::Delete, egui::Modifiers::NONE, false, "Delete"),
        ] {
            let text = captured_key(key, modifiers, win).unwrap();
            assert_eq!(text, expected);
            assert!(parse_mapping(&text).is_some());
            assert!(parse_keyboard_trigger(&text).is_some());
        }
        assert!(captured_key(egui::Key::F35, egui::Modifiers::NONE, false).is_none());
    }

    #[test]
    fn speed_shortcuts_default_round_trip_and_reject_duplicates() {
        let old = parse_config("keyboard_trigger=F8\n");
        assert_eq!(old.speed_up, "NumAdd");
        assert_eq!(old.speed_down, "NumSubtract");
        assert_eq!(parse_shortcuts("F8", &old.speed_up, &old.speed_down).unwrap(),
            [Some((0, 0x77)), Some((0, 0x6b)), Some((0, 0x6d))]);
        let custom = AppConfig { speed_up: "Ctrl+F9".into(), speed_down: String::new(), ..old };
        let loaded = parse_config(&config_body(&custom));
        assert_eq!(loaded.speed_up, "Ctrl+F9");
        assert!(loaded.speed_down.is_empty());
        assert!(parse_shortcuts("F8", "f8", "NumSubtract").is_err());
        assert!(parse_shortcuts("", "NumAdd", "numadd").is_err());
        assert!(parse_shortcuts("", "Num+", "NumSubtract").is_err());
        assert_eq!(parse_keyboard_trigger("Ctrl+NumAdd"), Some(Some((2, 0x6b))));
        assert_eq!(parse_shortcuts("", "", "").unwrap(), [None; 3]);
    }

    #[test]
    fn keyboard_trigger_validates_single_keys_and_combinations() {
        assert_eq!(parse_keyboard_trigger(""), Some(None));
        assert_eq!(parse_keyboard_trigger("F8"), Some(Some((0, 0x77))));
        assert_eq!(
            parse_keyboard_trigger(" ctrl + Alt + q "),
            Some(Some((3, 0x51)))
        );
        for invalid in ["Ctrl", "Ctrl+Ctrl+Q", "A+B", "Ctrl++Q", "F25", "wat"] {
            assert_eq!(parse_keyboard_trigger(invalid), None, "{invalid}");
        }
        assert!(parse_config("").keyboard_trigger.is_empty());
        let config = AppConfig {
            keyboard_trigger: "Ctrl+Alt+Q".into(),
            ..Default::default()
        };
        assert_eq!(
            parse_config(&config_body(&config)).keyboard_trigger,
            "Ctrl+Alt+Q"
        );
    }

    #[test]
    fn auto_load_defaults_on_and_setting_round_trips() {
        assert!(parse_config("last_process=game.exe").auto_load_process);
        for enabled in [false, true] {
            let config = AppConfig {
                auto_load_process: enabled,
                last_process: "game.exe".into(),
                ..Default::default()
            };
            let loaded = parse_config(&config_body(&config));
            assert_eq!(loaded.auto_load_process, enabled);
            assert_eq!(loaded.last_process, "game.exe");
        }
    }

    #[test]
    fn old_configs_keep_game_pause_disabled() {
        assert!(!AppConfig::default().pause_game_on_trigger);
        assert!(!parse_config("mapping=Alt+Tab\ncapture_enabled=true\n").pause_game_on_trigger);
    }

    #[test]
    fn pause_setting_round_trips() {
        for enabled in [false, true] {
            let config = AppConfig {
                pause_game_on_trigger: enabled,
                ..Default::default()
            };
            assert_eq!(
                parse_config(&config_body(&config)).pause_game_on_trigger,
                enabled
            );
        }
    }

    #[test]
    fn last_process_round_trips_and_old_configs_start_empty() {
        let config = AppConfig {
            last_process: "ForzaHorizon5.exe".into(),
            ..Default::default()
        };
        assert_eq!(
            parse_config(&config_body(&config)).last_process,
            "ForzaHorizon5.exe"
        );
        assert!(
            parse_config("mapping=Alt+Tab\ncapture_enabled=true\n")
                .last_process
                .is_empty()
        );
        assert!(AppConfig::default().last_process.is_empty());
    }

    #[test]
    fn focus_settings_round_trip_and_invalid_positions_are_safe() {
        for style in marker::Style::ALL {
            let config = AppConfig {
                focus_style: style,
                ..Default::default()
            };
            assert_eq!(parse_config(&config_body(&config)).focus_style, style);
        }
        assert_eq!(
            parse_config("focus_style=unknown").focus_style,
            marker::Style::Ring
        );
        assert_eq!(parse_config("").focus_style, marker::Style::Ring);
        let config = AppConfig {
            focus_enabled: true,
            focus_locked: true,
            focus_diameter: 24,
            focus_opacity: 128,
            focus_x: 0.2,
            focus_y: 0.8,
            ..Default::default()
        };
        let loaded = parse_config(&config_body(&config));
        assert!(loaded.focus_enabled && loaded.focus_locked);
        assert_eq!((loaded.focus_diameter, loaded.focus_opacity), (24, 128));
        assert_eq!((loaded.focus_x, loaded.focus_y), (0.2, 0.8));
        for value in ["NaN", "inf", "-inf", "invalid"] {
            assert_eq!(parse_focus_position(value), 0.5);
        }
        assert_eq!(parse_focus_position("-9"), 0.0);
        assert_eq!(parse_focus_position("2"), 1.0);
        assert!(!parse_config("mapping=Alt+Tab").focus_enabled);
    }
}

fn next_hid_event(device_key: isize) -> u64 {
    let counts = HID_EVENT_COUNTS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = counts.lock().expect("hid event count mutex poisoned");
    let next = guard.get(&device_key).copied().unwrap_or(0) + 1;
    guard.insert(device_key, next);
    next
}

fn enumerate_matching_devices() -> Result<Vec<String>> {
    let mut count = 0u32;
    unsafe {
        let result = GetRawInputDeviceList(
            None,
            &mut count,
            std::mem::size_of::<RAWINPUTDEVICELIST>() as u32,
        );
        if result == u32::MAX {
            return Err(Error::from_thread());
        }
    }

    if count == 0 {
        return Ok(Vec::new());
    }

    let mut devices = vec![RAWINPUTDEVICELIST::default(); count as usize];
    unsafe {
        let result = GetRawInputDeviceList(
            Some(devices.as_mut_ptr()),
            &mut count,
            std::mem::size_of::<RAWINPUTDEVICELIST>() as u32,
        );
        if result == u32::MAX {
            return Err(Error::from_thread());
        }
    }

    let mut names = Vec::new();
    for device in devices.into_iter().take(count as usize) {
        if device.dwType != RIM_TYPEHID {
            continue;
        }
        let Ok(info) = raw_hid_info(device.hDevice) else {
            continue;
        };
        if !is_matching_controller(info) {
            continue;
        }
        let name = raw_device_name(device.hDevice)
            .unwrap_or_else(|_| format!("HANDLE({:#x})", device.hDevice.0 as usize));
        names.push(name);
    }

    names.sort();
    names.dedup();
    Ok(names)
}

fn is_matching_controller(info: RID_DEVICE_INFO_HID) -> bool {
    info.dwVendorId == 0x045E
        && info.dwProductId == 0x02E0
        && info.usUsagePage == HID_USAGE_PAGE_GENERIC_DESKTOP
        && info.usUsage == HID_USAGE_GAMEPAD
}

fn raw_device_name(device: HANDLE) -> Result<String> {
    let key = device.0 as isize;
    let cache = HID_NAMES.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(name) = cache
        .lock()
        .expect("hid name mutex poisoned")
        .get(&key)
        .cloned()
    {
        return Ok(name);
    }

    let mut size = 0u32;
    unsafe {
        let query = GetRawInputDeviceInfoW(Some(device), RIDI_DEVICENAME, None, &mut size);
        if query == u32::MAX {
            return Err(Error::from_thread());
        }
    }

    let mut buffer = vec![0u16; size as usize];
    unsafe {
        let read = GetRawInputDeviceInfoW(
            Some(device),
            RIDI_DEVICENAME,
            Some(buffer.as_mut_ptr() as *mut c_void),
            &mut size,
        );
        if read == u32::MAX {
            return Err(Error::from_thread());
        }
    }

    let nul = buffer
        .iter()
        .position(|&ch| ch == 0)
        .unwrap_or(buffer.len());
    let name = String::from_utf16_lossy(&buffer[..nul]);
    cache
        .lock()
        .expect("hid name mutex poisoned")
        .insert(key, name.clone());
    Ok(name)
}

fn raw_hid_info(device: HANDLE) -> Result<RID_DEVICE_INFO_HID> {
    let mut info = RID_DEVICE_INFO {
        cbSize: std::mem::size_of::<RID_DEVICE_INFO>() as u32,
        ..Default::default()
    };
    let mut size = std::mem::size_of::<RID_DEVICE_INFO>() as u32;
    unsafe {
        let read = GetRawInputDeviceInfoW(
            Some(device),
            RIDI_DEVICEINFO,
            Some(&mut info as *mut _ as *mut c_void),
            &mut size,
        );
        if read == u32::MAX {
            return Err(Error::from_thread());
        }
        Ok(info.Anonymous.hid)
    }
}
