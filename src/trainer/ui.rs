use super::{
    Target,
    profile::{self, Input},
    worker::Worker,
};
use crate::{Language, game_text, theme};
use eframe::egui::{self, RichText};
use std::{collections::HashMap, path::PathBuf};

pub const WINDOW_ID: &str = "trainer_window";
pub struct TrainerUi {
    pub open: bool,
    worker: Worker,
    target: Option<Target>,
    inputs: HashMap<String, String>,
    search: String,
    available_only: bool,
    local_error: String,
    closing: bool,
    preview: bool,
    preview_frames: u32,
}
impl TrainerUi {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            open: false,
            worker: Worker::new(directory),
            target: None,
            inputs: HashMap::new(),
            search: String::new(),
            available_only: false,
            local_error: String::new(),
            closing: false,
            preview: false,
            preview_frames: 0,
        }
    }
    pub fn sync_target(&mut self, target: Option<Target>) {
        if self.preview {
            return;
        }
        self.worker.poll();
        if target != self.target {
            self.inputs.clear();
            self.local_error.clear();
            self.target = target.clone();
            self.worker.bind(target);
        } else if self.worker.state.target != self.target
            && !self.worker.state.busy
            && !self.worker.state.cleanup_failed
        {
            // A previous retarget may have been blocked by cleanup. Resume it only
            // after the user successfully retries cleanup on the retained session.
            self.worker.bind(self.target.clone());
        }
    }
    pub fn prepare_exit(&mut self) -> bool {
        self.worker.poll();
        if self.worker.state.exit_ready {
            return true;
        }
        if !self.closing || (!self.worker.state.busy && self.worker.state.error) {
            self.closing = true;
            self.worker.stop_all(true);
        }
        self.open = true;
        false
    }
    pub fn exit_ready(&mut self) -> bool {
        self.worker.poll();
        if self.closing && !self.worker.state.busy && self.worker.state.error {
            self.closing = false;
        }
        self.closing && self.worker.state.exit_ready
    }
    pub fn cancel_exit(&mut self) {
        self.closing = false;
        self.worker.state.exit_ready = false;
    }
    #[cfg(debug_assertions)]
    pub fn preview(&mut self) {
        self.preview = true;
        self.open = true;
        self.worker.state.profile = Some(profile::builtin());
        self.worker.state.message = "界面预览：未连接游戏，不会执行修改".into();
        self.search = std::env::var("SLACKINPUT_UI_TRAINER_FILTER").unwrap_or_default();
        if std::env::var_os("SLACKINPUT_UI_TRAINER_ACTIVE").is_some() {
            self.worker
                .state
                .active
                .insert("money".into(), Some(123456.0));
            self.inputs.insert("money".into(), "123456".into());
        }
    }
    pub fn show(&mut self, ctx: &egui::Context, language: Language) {
        if !self.open {
            return;
        }
        let title = game_text(language, "Trainer", "修改器");
        if self.preview {
            let mut open = self.open;
            egui::Window::new(title)
                .id(egui::Id::new(WINDOW_ID))
                .open(&mut open)
                .default_pos([20.0, 20.0])
                .default_size([680.0, 730.0])
                .show(ctx, |ui| self.contents(ui, language));
            self.open = open;
            return;
        }
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of(WINDOW_ID),
            egui::ViewportBuilder::default()
                .with_title(title)
                .with_inner_size([680.0, 760.0])
                .with_min_inner_size([540.0, 440.0]),
            |ctx, class| {
                if class == egui::ViewportClass::Embedded {
                    let mut open = self.open;
                    egui::Window::new(title)
                        .id(egui::Id::new(WINDOW_ID))
                        .open(&mut open)
                        .default_size([640.0, 650.0])
                        .show(ctx, |ui| self.contents(ui, language));
                    self.open = open;
                } else {
                    if ctx.input(|i| i.viewport().close_requested()) {
                        self.open = false;
                    }
                    egui::CentralPanel::default()
                        .frame(egui::Frame::new().fill(theme::BG).inner_margin(18))
                        .show(ctx, |ui| self.contents(ui, language));
                }
            },
        );
    }
    fn contents(&mut self, ui: &mut egui::Ui, language: Language) {
        self.preview_frames = self.preview_frames.saturating_add(1);
        let state = self.worker.state.clone();
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
        let catalog = profile::builtin();
        let Some(profile) = state
            .profile
            .as_ref()
            .or_else(|| state.target.is_none().then_some(&catalog))
        else {
            ui.heading(game_text(
                language,
                "No matching trainer",
                "未找到匹配的修改器",
            ));
            ui.label(&state.message);
            ui.label(game_text(
                language,
                "Bind Digimon Story Time Stranger.exe to load the built-in profile.",
                "绑定 Digimon Story Time Stranger.exe 后自动加载内置配置。",
            ));
            return;
        };
        ui.label(
            RichText::new(
                if language == Language::English && profile.id == profile::builtin().id {
                    "Digimon Story Time Stranger"
                } else {
                    &profile.name
                },
            )
            .size(21.0)
            .strong(),
        );
        let total = profile.features.len();
        let supported_count = profile
            .features
            .iter()
            .filter(|f| profile::supported(profile, &f.id))
            .count();
        let summary = if language == Language::Chinese {
            format!(
                "{total} 项功能 · {supported_count} 项原生适配 · {} 项待适配",
                total - supported_count
            )
        } else {
            format!(
                "{total} options · {supported_count} native adapters · {} pending",
                total - supported_count
            )
        };
        ui.label(
            RichText::new(format!("{}  ·  {}", profile.source_version, summary))
                .small()
                .color(theme::MUTED),
        );
        if let Some(target) = &state.target {
            ui.label(RichText::new(format!("{} · PID {}", target.name, target.pid)).small());
        } else if !self.preview {
            ui.colored_label(
                theme::GOLD,
                game_text(
                    language,
                    "Preview only — bind the game to enable options.",
                    "当前仅预览；绑定游戏进程后才能启用修改。",
                ),
            );
        }
        ui.label(RichText::new(game_text(language, "Game effects still need validation. Closing this window keeps active options running.", "游戏效果尚待实测；关闭面板保留已启用项，退出程序前停用全部。" )).small().color(theme::GOLD));
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .desired_width(200.0)
                    .hint_text(game_text(language, "Search options", "搜索功能")),
            );
            ui.checkbox(
                &mut self.available_only,
                game_text(language, "Adapted only", "仅已适配"),
            );
            if ui
                .add_enabled(
                    !state.busy && !self.preview,
                    egui::Button::new(game_text(language, "Disable all", "停用全部")),
                )
                .clicked()
            {
                self.local_error.clear();
                self.worker.stop_all(false);
            }
        });
        ui.separator();
        let query = self.search.trim().to_lowercase();
        let pending_target = state.target != self.target;
        let can_execute = !self.preview
            && !self.closing
            && !state.busy
            && !state.cleanup_failed
            && !pending_target
            && state.target.is_some();
        let scroll = egui::ScrollArea::vertical()
            .id_salt("trainer_options")
            .auto_shrink([false, false])
            .max_height((ui.available_height() - 90.0).max(120.0));
        #[cfg(debug_assertions)]
        let scroll = if self.preview
            && self.preview_frames == 3
            && std::env::var_os("SLACKINPUT_UI_SCROLL_BOTTOM").is_some()
        {
            scroll.vertical_scroll_offset(10000.0)
        } else {
            scroll
        };
        scroll.show(ui, |ui| {
            let mut count = 0;
            let mut groups: Vec<&str> = Vec::new();
            for f in &profile.features {
                if !groups.contains(&f.group.as_str()) {
                    groups.push(&f.group);
                }
            }
            for group in groups {
                let english = match group {
                    "战斗与探索" => "Combat & exploration",
                    "资源与物品" => "Resources & items",
                    "数码宝贝属性" => "Digimon stats",
                    other => other,
                };
                let features: Vec<_> = profile
                    .features
                    .iter()
                    .filter(|f| {
                        f.group == group
                            && (!self.available_only || profile::supported(profile, &f.id))
                            && (query.is_empty()
                                || format!("{} {} {}", f.name, f.name_en, f.id)
                                    .to_lowercase()
                                    .contains(&query))
                    })
                    .collect();
                if features.is_empty() {
                    continue;
                }
                ui.add_space(8.0);
                ui.label(
                    RichText::new(if language == Language::Chinese {
                        group
                    } else {
                        english
                    })
                    .strong()
                    .color(theme::MINT),
                );
                for feature in features {
                    count += 1;
                    let supported = profile::supported(profile, &feature.id);
                    let active = state.active.contains_key(&feature.id);
                    let label = if language == Language::Chinese {
                        &feature.name
                    } else {
                        &feature.name_en
                    };
                    egui::Frame::new()
                        .fill(theme::PANEL)
                        .inner_margin(10)
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.vertical(|ui| {
                                    let controls_width = if !matches!(feature.input, Input::Toggle)
                                    {
                                        285.0
                                    } else {
                                        235.0
                                    };
                                    ui.set_width(
                                        (ui.available_width() - controls_width).max(160.0),
                                    );
                                    ui.label(RichText::new(label).strong());
                                    ui.label(
                                        RichText::new(if active {
                                            game_text(language, "Enabled", "已启用")
                                        } else if supported {
                                            game_text(
                                                language,
                                                "Off · needs game validation",
                                                "未启用 · 待游戏实测",
                                            )
                                        } else {
                                            game_text(language, "Not adapted", "待适配")
                                        })
                                        .small()
                                        .color(if active { theme::MINT } else { theme::MUTED }),
                                    );
                                });
                                if !matches!(feature.input, Input::Toggle) {
                                    let input = self
                                        .inputs
                                        .entry(feature.id.clone())
                                        .or_insert_with(|| feature.default_text());
                                    ui.add(egui::TextEdit::singleline(input).desired_width(96.0))
                                        .on_hover_text(game_text(
                                            language,
                                            "Enter a value, then enable or apply",
                                            "输入数值后点击启用或应用",
                                        ));
                                } else {
                                    ui.add_space(104.0);
                                }
                                let button = if feature.one_shot() {
                                    game_text(language, "Apply", "应用")
                                } else if active {
                                    game_text(language, "Disable", "停用")
                                } else {
                                    game_text(language, "Enable", "启用")
                                };
                                if ui
                                    .add_enabled(
                                        can_execute && supported,
                                        egui::Button::new(button),
                                    )
                                    .clicked()
                                {
                                    self.local_error.clear();
                                    let value = if active {
                                        Ok(None)
                                    } else {
                                        feature.parse_value(
                                            self.inputs
                                                .get(&feature.id)
                                                .map(String::as_str)
                                                .unwrap_or(""),
                                        )
                                    };
                                    match value {
                                        Ok(value) => {
                                            self.worker.set(feature.id.clone(), value, !active)
                                        }
                                        Err(error) => {
                                            self.local_error = format!("{label}：{error}")
                                        }
                                    }
                                }
                                if !feature.one_shot() && !matches!(feature.input, Input::Toggle)
                                    && ui
                                        .add_enabled(
                                            can_execute && active && supported,
                                            egui::Button::new(game_text(language, "Apply", "应用")),
                                        )
                                        .clicked()
                                {
                                    match feature.parse_value(&self.inputs[&feature.id]) {
                                        Ok(value) => {
                                            self.local_error.clear();
                                            self.worker.set(feature.id.clone(), value, true);
                                        }
                                        Err(e) => self.local_error = e,
                                    }
                                }
                            });
                            if !feature.description.is_empty() {
                                ui.label(
                                    RichText::new(&feature.description)
                                        .small()
                                        .color(theme::MUTED),
                                );
                            }
                            if let Some(Some(value)) = state.active.get(&feature.id) {
                                ui.label(
                                    RichText::new(format!(
                                        "{} {value}",
                                        game_text(language, "Applied:", "已应用：")
                                    ))
                                    .small()
                                    .color(theme::MINT),
                                );
                            }
                            if let Some((value, digimon)) = state.applied.get(&feature.id) {
                                ui.label(RichText::new(format!("{} {value} · ID {digimon}",
                                    game_text(language, "Last applied:", "上次应用：")))
                                    .small().color(theme::MINT));
                            }
                        });
                    ui.add_space(4.0);
                }
            }
            if count == 0 {
                ui.label(game_text(language, "No matching options", "没有匹配的功能"));
            }
        });
        ui.separator();
        if state.busy || (pending_target && !state.error) {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(game_text(language, "Processing…", "正在处理…"));
            });
        } else {
            ui.label(RichText::new(&state.message).color(if state.error {
                theme::GOLD
            } else {
                theme::MUTED
            }));
        }
        if !self.local_error.is_empty() {
            ui.colored_label(theme::GOLD, &self.local_error);
        }
        if state.cleanup_failed {
            ui.colored_label(
                theme::GOLD,
                game_text(
                    language,
                    "Use Disable all to retry cleanup before continuing.",
                    "请点击“停用全部”重试恢复，再继续操作。",
                ),
            );
        }
        ui.label(
            RichText::new(game_text(
                language,
                "Speed options excluded. Profiles load automatically; options never auto-enable.",
                "已排除加速功能；配置随进程自动加载，修改项不会自动启用。",
            ))
            .small()
            .color(theme::MUTED),
        );
    }
}
