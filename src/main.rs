#![windows_subsystem = "windows"]

mod desktop;
mod process;
mod theme;
mod titlebar;

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
    mapping_text: String,
    capture_enabled: bool,
    debug_logging: bool,
    language: Language,
    pause_game_on_trigger: bool,
    focus_enabled: bool,
    focus_locked: bool,
    focus_diameter: u8,
    focus_opacity: u8,
    focus_x: f32,
    focus_y: f32,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            mapping_text: "Ctrl+Win+Left".to_string(),
            capture_enabled: true,
            debug_logging: false,
            language: Language::English,
            pause_game_on_trigger: false,
            focus_enabled: false,
            focus_locked: false,
            focus_diameter: 16,
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
    english: &'static str,
    chinese: &'static str,
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
            english: "English",
            chinese: "Chinese",
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
            english: "英文",
            chinese: "中文",
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
        },
    }
}

struct AppState {
    config: AppConfig,
    mapping_keys: Vec<VIRTUAL_KEY>,
    status: String,
    logs: String,
    connected_devices: Vec<String>,
    bound_process: Option<process::BoundProcess>,
}

impl AppState {
    fn new(config: AppConfig, mapping_keys: Vec<VIRTUAL_KEY>) -> Self {
        let text = tr(config.language);
        let status = if config.capture_enabled {
            text.status_capture_on.to_string()
        } else {
            text.status_capture_off.to_string()
        };
        Self {
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

        Self {
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
        }
    }

    fn apply_changes(&mut self) {
        let mapping_text = self.mapping_input.trim().to_string();
        let text = tr(self.language);
        let Some(mapping_keys) = parse_mapping(&mapping_text) else {
            set_status(text.invalid_hotkey);
            push_log_force(text.invalid_hotkey);
            return;
        };

        let config = AppConfig {
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

        match save_config(&config) {
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

impl MapperApp {
    fn focus_controls(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        theme::heading(ui, "03", game_text(language, "Focus ring", "防晕眩圆点"));
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
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 94.0), Sense::hover());
        ui.painter().rect_filled(rect, 0.0, theme::BG);
        let center = rect.center();
        for x in (0..(rect.width() as i32)).step_by(16) {
            ui.painter().line_segment(
                [
                    egui::pos2(rect.left() + x as f32, rect.top()),
                    egui::pos2(rect.left() + x as f32, rect.bottom()),
                ],
                egui::Stroke::new(0.5, theme::EDGE),
            );
        }
        for y in (0..94).step_by(16) {
            ui.painter().line_segment(
                [
                    egui::pos2(rect.left(), rect.top() + y as f32),
                    egui::pos2(rect.right(), rect.top() + y as f32),
                ],
                egui::Stroke::new(0.5, theme::EDGE),
            );
        }
        ui.painter().circle_stroke(
            center,
            config.focus_diameter as f32 / 2.0,
            egui::Stroke::new(3.0, Color32::from_white_alpha(config.focus_opacity)),
        );
        ui.label(
            RichText::new(game_text(
                language,
                "TRANSPARENT CENTER / PREVIEW",
                "中心透明 / 效果预览",
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
                    "UNLOCKED / Drag the white rim to move it.",
                    "已解锁 / 按住白色圆环边缘拖动位置。",
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
                "Changes apply now. Save to keep them.",
                "调整即时生效，点击保存配置以保留。",
            ))
            .small()
            .color(theme::MUTED),
        );
        if changed {
            let mut state = app_state().lock().unwrap();
            state.config.focus_enabled = config.focus_enabled;
            state.config.focus_locked = config.focus_locked;
            state.config.focus_diameter = config.focus_diameter;
            state.config.focus_opacity = config.focus_opacity;
            state.config.focus_x = config.focus_x;
            state.config.focus_y = config.focus_y;
        }
    }

    fn refresh_processes(&mut self) {
        match process::enumerate() {
            Ok(processes) => self.processes = processes,
            Err(error) => report_process_error(self.language, error),
        }
    }

    fn game_controls(&mut self, ui: &mut egui::Ui) {
        let language = self.language;
        ui.vertical(|ui| {
            theme::heading(ui, "02", game_text(language, "Game process", "游戏进程"));
            let (label, paused) = {
                let mut state = app_state().lock().expect("app state mutex poisoned");
                if state.bound_process.as_ref().is_some_and(|p| p.exited()) {
                    state.bound_process = None;
                    state.status = game_text(
                        language,
                        "Bound game exited; select a process again",
                        "绑定的游戏已退出，请重新选择进程",
                    )
                    .into();
                }
                match &state.bound_process {
                    Some(p) => (
                        format!(
                            "{} (PID {}) — {}",
                            p.name,
                            p.pid,
                            if p.paused {
                                game_text(language, "Paused", "已暂停")
                            } else {
                                game_text(language, "Running", "运行中")
                            }
                        ),
                        p.paused,
                    ),
                    None => (
                        game_text(language, "No process bound", "未绑定进程").into(),
                        false,
                    ),
                }
            };
            ui.label(label);
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
                        paused,
                        egui::Button::new(game_text(language, "Resume game", "恢复游戏")),
                    )
                    .clicked()
                {
                    let result = {
                        let mut state = app_state().lock().expect("app state mutex poisoned");
                        state.bound_process.as_mut().map(|p| p.resume()).transpose()
                    };
                    match result {
                        Ok(_) => set_status(game_text(language, "Game resumed", "游戏已恢复")),
                        Err(error) => report_process_error(language, error),
                    }
                }
                if ui
                    .button(game_text(language, "Unbind", "解除绑定"))
                    .clicked()
                {
                    let result = {
                        let mut state = app_state().lock().expect("app state mutex poisoned");
                        let result = state.bound_process.as_mut().map(|p| p.resume()).transpose();
                        if result.is_ok() {
                            state.bound_process = None;
                        }
                        result
                    };
                    if let Err(error) = result {
                        report_process_error(language, error);
                    }
                }
            });
            ui.checkbox(
                &mut self.pause_game_on_trigger,
                game_text(
                    language,
                    "Pause bound game on Xbox button (Save to apply)",
                    "按 Xbox 键时暂停绑定游戏（保存后生效）",
                ),
            );
        });
    }

    fn process_picker(&mut self, ctx: &egui::Context) {
        if !self.process_picker_open {
            return;
        }
        let language = self.language;
        let mut open = self.process_picker_open;
        let mut selected = None;
        egui::Window::new(game_text(language, "Select game process", "选择游戏进程"))
            .id(egui::Id::new("process_picker"))
            .open(&mut open)
            .default_size(vec2(560.0, 320.0))
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
                let filter = self.process_filter.trim().to_lowercase();
                ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                    for (index, process) in self.processes.iter().enumerate() {
                        let label = format!("{} (PID {})", process.name, process.pid);
                        if !process.exited()
                            && label.to_lowercase().contains(&filter)
                            && ui.selectable_label(false, label).clicked()
                        {
                            selected = Some(index);
                        }
                    }
                });
            });
        if let Some(index) = selected {
            let result = {
                let mut state = app_state().lock().expect("app state mutex poisoned");
                let result = state.bound_process.as_mut().map(|p| p.resume()).transpose();
                if result.is_ok() {
                    state.bound_process = Some(self.processes.remove(index));
                    state.status =
                        game_text(language, "Game process bound", "游戏进程已绑定").into();
                }
                result
            };
            match result {
                Ok(_) => open = false,
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
            let result = {
                let mut state = app_state().lock().expect("app state mutex poisoned");
                let result = state.bound_process.as_mut().map(|p| p.resume()).transpose();
                if result.is_ok() {
                    state.config.capture_enabled = false;
                }
                result
            };
            if let Err(error) = result {
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
                    .inner_margin(egui::Margin::same(22))
                    .stroke(egui::Stroke::new(2.0, theme::EDGE)),
            )
            .show(ctx, |ui| {
                let mut controls_left = ui.max_rect().right();
                let header = ui.horizontal(|ui| {
                    ui.image((self.icon.id(), vec2(58.0, 58.0)))
                        .on_hover_text(text.title);
                    ui.vertical(|ui| {
                        ui.add(egui::Label::new(
                            RichText::new("SLACK INPUT")
                                .monospace()
                                .size(27.0)
                                .strong()
                                .color(theme::MINT),
                        ));
                        ui.label(
                            RichText::new(game_text(
                                self.language,
                                "GAME COMPANION  /  READY WHEN YOU ARE",
                                "游戏助手  /  随时切换，安心继续",
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
                    titlebar.update(
                        egui::Rect::from_min_max(
                            egui::Pos2::ZERO,
                            egui::pos2(controls_left - 8.0, header.response.rect.bottom() + 8.0),
                        ),
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
                            .selected_text(if self.language == Language::English {
                                text.english
                            } else {
                                text.chinese
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.language,
                                    Language::English,
                                    text.english,
                                );
                                ui.selectable_value(
                                    &mut self.language,
                                    Language::Chinese,
                                    text.chinese,
                                );
                            })
                            .response
                            .on_hover_text(text.language);
                    });
                });
                ui.add_space(10.0);
                ScrollArea::vertical()
                    .max_height((ui.available_height() - 102.0).max(260.0))
                    .show(ui, |ui| {
                        ui.columns(2, |columns| {
                            theme::card().show(&mut columns[0], |ui| {
                                ui.set_width(ui.available_width());
                                theme::heading(ui, "01", text.mapping);
                                ui.label(RichText::new(text.preset).color(theme::MUTED));
                                egui::ComboBox::from_id_salt("preset_combo")
                                    .width(ui.available_width())
                                    .selected_text(PRESETS[self.selected_preset])
                                    .show_ui(ui, |ui| {
                                        for (index, preset) in PRESETS.iter().enumerate() {
                                            if ui
                                                .selectable_value(
                                                    &mut self.selected_preset,
                                                    index,
                                                    *preset,
                                                )
                                                .clicked()
                                            {
                                                self.mapping_input = (*preset).to_string();
                                            }
                                        }
                                    });
                                ui.add(
                                    TextEdit::singleline(&mut self.mapping_input)
                                        .desired_width(f32::INFINITY)
                                        .font(egui::TextStyle::Monospace),
                                );
                                ui.label(
                                    RichText::new(text.mapping_hint).small().color(theme::MUTED),
                                );
                                ui.checkbox(&mut self.capture_enabled, text.capture);
                            });
                            columns[0].add_space(12.0);
                            theme::card().show(&mut columns[0], |ui| {
                                ui.set_width(ui.available_width());
                                self.game_controls(ui);
                            });
                            theme::card().show(&mut columns[1], |ui| {
                                ui.set_width(ui.available_width());
                                self.focus_controls(ui);
                            });
                        });
                    });
                ui.add_space(12.0);
                ui.separator();
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
                    if ui.button(text.reset).clicked() {
                        self.selected_preset = 0;
                        self.mapping_input = PRESETS[0].to_string();
                        self.capture_enabled = true;
                        self.debug_logging = false;
                        self.pause_game_on_trigger = false;
                        self.language = Language::English;
                        {
                            let mut state = app_state().lock().unwrap();
                            state.config = AppConfig::default();
                        }
                        self.apply_changes();
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button(text.debug).clicked() {
                            self.debug_window_open = true;
                            self.debug_logging = true;
                            app_state().lock().unwrap().config.debug_logging = true;
                        }
                        ui.label(
                            RichText::new(game_text(
                                self.language,
                                "MINIMIZE → TRAY",
                                "最小化 → 系统托盘",
                            ))
                            .monospace()
                            .small()
                            .color(theme::MUTED),
                        );
                    });
                });
                ui.add_space(2.0);
                ui.label(RichText::new(status).small().color(theme::MUTED));
            });

        self.process_picker(ctx);

        if self.debug_window_open {
            egui::Window::new(text.logs)
                .id(egui::Id::new("debug_log_window"))
                .open(&mut self.debug_window_open)
                .resizable(true)
                .vscroll(true)
                .default_size(vec2(680.0, 360.0))
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
    push_log_force(tr(current_language()).startup_log);

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([960.0, 760.0])
            .with_min_inner_size([900.0, 720.0])
            .with_resizable(true)
            .with_decorations(false)
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png"))
                    .expect("embedded icon"),
            )
            .with_maximize_button(false)
            .with_title("SlackInput"),
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
            if let Err(error) = process.resume() {
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
        if state.config.capture_enabled && state.config.pause_game_on_trigger {
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
            "mapping" => config.mapping_text = value.trim().to_string(),
            "capture_enabled" => config.capture_enabled = value.trim().eq_ignore_ascii_case("true"),
            "debug_logging" => config.debug_logging = value.trim().eq_ignore_ascii_case("true"),
            "pause_game_on_trigger" => {
                config.pause_game_on_trigger = value.trim().eq_ignore_ascii_case("true")
            }
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
    format!(
        "mapping={}\ncapture_enabled={}\ndebug_logging={}\nlanguage={}\npause_game_on_trigger={}\nfocus_enabled={}\nfocus_locked={}\nfocus_diameter={}\nfocus_opacity={}\nfocus_x={}\nfocus_y={}\n",
        config.mapping_text,
        config.capture_enabled,
        config.debug_logging,
        config.language.config_value(),
        config.pause_game_on_trigger,
        config.focus_enabled,
        config.focus_locked,
        config.focus_diameter,
        config.focus_opacity,
        config.focus_x,
        config.focus_y
    )
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
    fn focus_settings_round_trip_and_invalid_positions_are_safe() {
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
