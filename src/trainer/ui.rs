use super::{
    Target,
    profile::{self, Input},
    worker::Worker,
};
use crate::{Language, game_text, theme};
use eframe::egui::{self, RichText};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

pub struct TrainerUi {
    worker: Worker,
    directory: PathBuf,
    target: Option<Target>,
    inputs: HashMap<String, String>,
    search: String,
    enabled_only: bool,
    local_error: String,
    closing: bool,
    preview: bool,
    overwrite: HashSet<String>,
    delete: Option<String>,
}
impl TrainerUi {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            worker: Worker::new(directory.clone()),
            directory,
            target: None,
            inputs: HashMap::new(),
            search: String::new(),
            enabled_only: false,
            local_error: String::new(),
            closing: false,
            preview: false,
            overwrite: HashSet::new(),
            delete: None,
        }
    }
    pub fn sync_target(&mut self, target: Option<Target>) {
        if self.preview {
            return;
        }
        self.worker.poll();
        if target != self.target {
            self.inputs.clear();
            self.delete = None;
            self.overwrite.clear();
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
        false
    }
    pub fn exit_ready(&mut self) -> bool {
        if self.preview && !self.closing {
            return false;
        }
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
        self.worker.state.profile = Some(profile::builtin());
        self.worker.state.busy = false;
        self.worker.state.message = "界面预览：未连接游戏，不会执行修改".into();
        if std::env::var_os("SLACKINPUT_UI_TRAINER_IMPORT").is_some() {
            self.worker.state.draft = Some(profile::ImportDraft {
                profile: profile::builtin(),
                warnings: vec!["静态导入预览：不会执行 EXE".into()],
            });
        }
        self.search = std::env::var("SLACKINPUT_UI_TRAINER_FILTER").unwrap_or_default();
        if std::env::var_os("SLACKINPUT_UI_TRAINER_ACTIVE").is_some() {
            self.worker
                .state
                .active
                .insert("money".into(), Some(123456.0));
            self.inputs.insert("money".into(), "123456".into());
        }
    }
    pub fn show(&mut self, ui: &mut egui::Ui, language: Language) {
        self.contents(ui, language);
    }
    pub fn previewing(&self) -> bool {
        self.preview
    }
    fn management(&mut self, ui: &mut egui::Ui, language: Language) {
        let state = self.worker.state.clone();
        let ready = !state.busy
            && !state.cleanup_failed
            && !self.closing
            && !self.preview
            && state.target == self.target;
        if ui
            .add_enabled(
                ready && state.draft.is_none(),
                egui::Button::new(game_text(language, "Import trainer EXE", "导入修改器 EXE")),
            )
            .clicked()
        {
            self.overwrite.clear();
            self.delete = None;
            self.worker.analyze();
        }
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    ready && state.draft.is_none(),
                    egui::Button::new(game_text(language, "Reload config", "重新加载配置")),
                )
                .clicked()
            {
                self.inputs.clear();
                self.delete = None;
                self.worker.bind(self.target.clone());
            }
            ui.label(
                RichText::new(self.directory.display().to_string())
                    .small()
                    .color(theme::MUTED),
            );
        });
        if let Some(draft) = &state.draft {
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                ui.label(format!(
                    "导入预览：{} · {} 项",
                    draft.profile.name,
                    draft.profile.features.len()
                ));
                for warning in &draft.warnings {
                    ui.colored_label(theme::GOLD, warning);
                }
                let conflicts: Vec<_> = draft
                    .profile
                    .features
                    .iter()
                    .filter(|f| {
                        state.profile.as_ref().is_some_and(|p| {
                            p.features
                                .iter()
                                .any(|x| profile::name_key(&x.name) == profile::name_key(&f.name))
                        })
                    })
                    .collect();
                ui.label(format!(
                    "新增 {} 项，同名 {} 项（默认跳过）",
                    draft.profile.features.len() - conflicts.len(),
                    conflicts.len()
                ));
                ui.horizontal(|ui| {
                    if ui.button("全部覆盖").clicked() {
                        self.overwrite = conflicts
                            .iter()
                            .map(|f| profile::name_key(&f.name))
                            .collect();
                    }
                    if ui.button("全部跳过").clicked() {
                        self.overwrite.clear();
                    }
                });
                egui::ScrollArea::vertical()
                    .id_salt("import_review")
                    .max_height((ui.available_height() - 160.0).clamp(80.0, 360.0))
                    .show(ui, |ui| {
                        for f in &draft.profile.features {
                            let key = profile::name_key(&f.name);
                            if conflicts.iter().any(|x| x.id == f.id) {
                                ui.vertical(|ui| {
                                    ui.label(&f.name);
                                    let mut replace = self.overwrite.contains(&key);
                                    ui.radio_value(&mut replace, true, "覆盖");
                                    ui.radio_value(&mut replace, false, "跳过");
                                    if replace {
                                        self.overwrite.insert(key);
                                    } else {
                                        self.overwrite.remove(&key);
                                    }
                                });
                            } else {
                                ui.label(format!("新增：{}", f.name));
                            }
                        }
                    });
                ui.label("保存前将停用当前全部修改；不会自动启用新项目。");
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(ready, egui::Button::new("确认导入"))
                        .clicked()
                    {
                        let merged = if let Some(current) = &state.profile {
                            profile::merge(current, &draft.profile, &self.overwrite)
                        } else {
                            Ok(draft.profile.clone())
                        };
                        match merged {
                            Ok(p) => {
                                self.inputs.clear();
                                self.local_error.clear();
                                self.worker.save(p);
                            }
                            Err(e) => self.local_error = e,
                        }
                    }
                    if ui.add_enabled(ready, egui::Button::new("取消")).clicked() {
                        self.worker.dismiss_draft();
                    }
                });
            });
        }
        if let Some(id) = self.delete.clone()
            && let Some(p) = &state.profile
            && let Some(f) = p.features.iter().find(|f| f.id == id)
        {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("删除“{}”？保存前将停用全部修改。", f.name));
                if ui
                    .add_enabled(ready, egui::Button::new("确认删除"))
                    .clicked()
                {
                    let mut updated = p.clone();
                    updated.features.retain(|f| f.id != id);
                    self.worker.save(updated);
                    self.inputs.remove(&id);
                    self.delete = None;
                }
                if ui.button("取消").clicked() {
                    self.delete = None;
                }
            });
        }
    }
    fn contents(&mut self, ui: &mut egui::Ui, language: Language) {
        if crate::app_state().lock().unwrap().features_locked() {
            ui.disable();
        }
        let state = self.worker.state.clone();
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
        if state.target != self.target && !state.cleanup_failed {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(game_text(language, "Loading trainer…", "正在加载修改器…"));
            });
            return;
        }
        if state.draft.is_some() || self.delete.is_some() {
            self.management(ui, language);
        } else {
            egui::CollapsingHeader::new(game_text(language, "Manage profiles", "配置管理"))
                .id_salt("trainer_management")
                .show(ui, |ui| self.management(ui, language));
        }
        if state.draft.is_some() {
            if self.worker.state.busy {
                ui.spinner();
            }
            if state.error {
                ui.colored_label(theme::GOLD, &state.message);
            }
            if !self.local_error.is_empty() {
                ui.colored_label(theme::GOLD, &self.local_error);
            }
            if state.cleanup_failed
                && ui
                    .add_enabled(!state.busy, egui::Button::new("重试停用全部"))
                    .clicked()
            {
                self.worker.stop_all(false);
            }
            return;
        }
        let catalog = profile::builtin();
        let Some(profile) = state
            .profile
            .as_ref()
            .filter(|profile| !profile.features.is_empty())
            .or_else(|| self.preview.then_some(&catalog))
        else {
            if state.busy || state.target != self.target {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(game_text(language, "Loading trainer…", "正在加载修改器…"));
                });
                return;
            }
            ui.heading(game_text(
                language,
                "No matching trainer",
                "未找到匹配的修改器",
            ));
            if state.profile.as_ref().is_some_and(|p| p.features.is_empty()) {
                ui.label(game_text(
                    language,
                    "The profile for this process has no options yet.",
                    "当前进程的配置尚无修改项。",
                ));
            } else if state.error {
                ui.colored_label(theme::GOLD, &state.message);
            } else {
                ui.label(&state.message);
            }
            if !self.local_error.is_empty() {
                ui.colored_label(theme::GOLD, &self.local_error);
            }
            if state.target.is_some() {
                ui.label(game_text(
                    language,
                    "Import a compatible trainer EXE to add a profile for this process.",
                    "可导入与当前进程匹配的修改器 EXE，添加对应配置。",
                ));
                ui.label(
                    RichText::new(game_text(
                        language,
                        "The current importer recognizes only the verified Digimon Story Time Stranger source EXE.",
                        "当前导入器仅识别已验证的《数码宝贝物语：时空异客》来源 EXE。",
                    ))
                    .small()
                    .color(theme::MUTED),
                );
                if ui
                    .add_enabled(
                        !state.cleanup_failed && !self.closing && !self.preview,
                        egui::Button::new(game_text(
                            language,
                            "Import trainer EXE",
                            "导入修改器 EXE",
                        )),
                    )
                    .clicked()
                {
                    self.overwrite.clear();
                    self.delete = None;
                    self.worker.analyze();
                }
            }
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
            .size(17.0)
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
        ui.label(
            RichText::new(game_text(
                language,
                "Game effects still need validation. Switching tabs keeps active options running.",
                "游戏效果尚待实测；切换页签保留已启用项，退出程序前停用全部。",
            ))
            .small()
            .color(theme::GOLD),
        );
        ui.add_space(6.0);
        ui.add(
            egui::TextEdit::singleline(&mut self.search)
                .desired_width(ui.available_width())
                .hint_text(game_text(language, "Search options", "搜索功能")),
        );
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(
                &mut self.enabled_only,
                game_text(language, "Enabled only", "仅启用"),
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
            && !self.worker.state.busy
            && state.draft.is_none()
            && self.delete.is_none()
            && !state.cleanup_failed
            && !pending_target
            && state.target.is_some();
        if state.busy || state.error || state.cleanup_failed || !self.local_error.is_empty() {
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
        }
        ui.scope(|ui| {
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
                            && (!self.enabled_only || state.active.contains_key(&f.id))
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
                            ui.vertical(|ui| {
                                ui.label(RichText::new(label).strong());
                                ui.label(
                                    RichText::new(if active {
                                        game_text(language, "Enabled", "已启用")
                                    } else if supported && feature.one_shot() {
                                        game_text(
                                            language,
                                            "Apply once to selected Digimon",
                                            "一次性应用到当前数码宝贝",
                                        )
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
                                    .color(if active {
                                        theme::MINT
                                    } else {
                                        theme::MUTED
                                    }),
                                );
                            });
                            ui.horizontal_wrapped(|ui| {
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
                                }
                                if ui
                                    .add_enabled(
                                        !state.busy
                                            && !state.cleanup_failed
                                            && !self.preview
                                            && !self.closing
                                            && state.target == self.target
                                            && state.draft.is_none(),
                                        egui::Button::new(game_text(language, "Delete", "删除")),
                                    )
                                    .clicked()
                                {
                                    self.delete = Some(feature.id.clone());
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
                                if !feature.one_shot()
                                    && !matches!(feature.input, Input::Toggle)
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
                                ui.label(
                                    RichText::new(format!(
                                        "{} {value} · ID {digimon}",
                                        game_text(language, "Last applied:", "上次应用：")
                                    ))
                                    .small()
                                    .color(theme::MINT),
                                );
                            }
                        });
                    ui.add_space(4.0);
                }
            }
            if count == 0 {
                ui.label(if self.enabled_only {
                    if query.is_empty() {
                        game_text(language, "No enabled options", "暂无已启用的修改项")
                    } else {
                        game_text(
                            language,
                            "No matching enabled options",
                            "没有匹配的已启用修改项",
                        )
                    }
                } else {
                    game_text(language, "No matching options", "没有匹配的功能")
                });
            }
        });
        ui.separator();
        if !state.busy && !state.error && !state.message.is_empty() {
            ui.label(RichText::new(&state.message).small().color(theme::MUTED));
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
