//! Versioned metadata contract. Imported files are data, never executable plugins.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

pub const SAMPLE_HASH: &str = "655f340b386aa82d4e41c84fd6c9f434cdad2a3b6d761dbf81ba09359d0e7e84";
pub const GAME_PROCESS: &str = "Digimon Story Time Stranger.exe";
const MAX_PROFILE: u64 = 2 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub process_names: Vec<String>,
    pub source_sha256: String,
    pub source_version: String,
    pub features: Vec<Feature>,
    #[serde(default)]
    pub execution: super::config::Execution,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Feature {
    pub id: String,
    pub name: String,
    pub name_en: String,
    pub group: String,
    pub description: String,
    pub input: Input,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Input {
    Toggle,
    Integer {
        default: i32,
        min: i32,
        max: i32,
        one_shot: bool,
    },
    Float {
        default: f64,
        min: f64,
        max: f64,
    },
}

impl Feature {
    pub fn default_text(&self) -> String {
        match self.input {
            Input::Toggle => String::new(),
            Input::Integer { default, .. } => default.to_string(),
            Input::Float { default, .. } => default.to_string(),
        }
    }

    pub fn one_shot(&self) -> bool {
        matches!(self.input, Input::Integer { one_shot: true, .. })
    }
    pub fn parse_value(&self, text: &str) -> Result<Option<f64>, String> {
        match self.input {
            Input::Toggle => Ok(None),
            Input::Integer { min, max, .. } => {
                let value: i32 = text
                    .trim()
                    .parse()
                    .map_err(|_| "请输入有效整数".to_string())?;
                if !(min..=max).contains(&value) {
                    return Err(format!("数值范围：{min}–{max}"));
                }
                if self.id == "e_digimon_talent" && value > i32::MAX / 1000 {
                    return Err("才能数值过大（最大 2147483）".into());
                }
                if self.id == "e_digimon_bond" && value > i32::MAX / 100 {
                    return Err("友情数值过大（最大 21474836）".into());
                }
                Ok(Some(f64::from(value)))
            }
            Input::Float { min, max, .. } => {
                let value: f64 = text
                    .trim()
                    .parse()
                    .map_err(|_| "请输入有效倍率".to_string())?;
                if !value.is_finite()
                    || !(min..=max).contains(&value)
                    || !(value as f32).is_finite()
                {
                    return Err(format!("倍率范围：{min}–{max}"));
                }
                Ok(Some(value))
            }
        }
    }
}

pub fn supported(profile: &Profile, id: &str) -> bool {
    profile.id == "digimon-time-stranger-20260709"
        && profile.source_sha256 == SAMPLE_HASH
        && (matches!(
            id,
            "stealth_mode"
                | "scan_rate_wont_decrease"
                | "battle_items_wont_decrease"
                | "money"
                | "agent_points"
        ) || super::hooks::group(id).is_some())
}

pub fn builtin() -> Profile {
    serde_json::from_str(include_str!(
        "../../assets/trainers/digimon-time-stranger.json"
    ))
    .expect("validated embedded trainer profile")
}

impl Profile {
    pub fn matches_process(&self, name: &str) -> bool {
        self.process_names
            .iter()
            .any(|p| p.eq_ignore_ascii_case(name))
    }

    pub fn validate(&self) -> Result<(), String> {
        self.execution.validate()?;
        if self.schema_version != 1
            || self.id.is_empty()
            || self.id.len() > 100
            || !self
                .id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        {
            return Err("不支持的修改器配置版本或 ID".into());
        }
        if self.name.is_empty()
            || self.name.len() > 256
            || self.features.len() > 256
            || self.process_names.is_empty()
            || self.process_names.len() > 16
            || self.source_sha256.len() != 64
            || !self.source_sha256.bytes().all(|c| c.is_ascii_hexdigit())
        {
            return Err("修改器配置字段无效".into());
        }
        for name in &self.process_names {
            if name.len() > 260
                || name.contains(['/', '\\', ':'])
                || !name.to_ascii_lowercase().ends_with(".exe")
            {
                return Err("进程名必须是 EXE 文件名".into());
            }
        }
        let mut ids = HashSet::new();
        for f in &self.features {
            if !ids.insert(&f.id)
                || f.id.is_empty()
                || f.id.len() > 100
                || f.name.len() > 256
                || f.name_en.len() > 256
                || f.description.len() > 8192
                || f.group.is_empty()
                || f.group.len() > 128
                || matches!(f.id.as_str(), "set_game_speed" | "movespeed_f")
            {
                return Err("修改项重复、无效或属于已排除的加速功能".into());
            }
            match f.input {
                Input::Integer {
                    default, min, max, ..
                } if min > max || !(min..=max).contains(&default) => {
                    return Err("整数范围无效".into());
                }
                Input::Float { default, min, max }
                    if !default.is_finite()
                        || !min.is_finite()
                        || !max.is_finite()
                        || min > max
                        || !(min..=max).contains(&default) =>
                {
                    return Err("倍率范围无效".into());
                }
                _ => {}
            }
        }
        if self.id == builtin().id {
            let canonical = builtin();
            if self.process_names != canonical.process_names || self.source_sha256 != SAMPLE_HASH {
                return Err("内置适配器进程或来源不匹配".into());
            }
            for f in &self.features {
                let original = canonical
                    .features
                    .iter()
                    .find(|x| x.id == f.id)
                    .ok_or("不支持的功能 ID")?;
                if serde_json::to_value(&f.input).unwrap()
                    != serde_json::to_value(&original.input).unwrap()
                {
                    return Err("适配器输入契约不匹配".into());
                }
            }
        }
        let mut names = HashSet::new();
        if self
            .features
            .iter()
            .any(|f| name_key(&f.name).is_empty() || !names.insert(name_key(&f.name)))
        {
            return Err("功能名称为空或重复".into());
        }
        Ok(())
    }
}

/// Static analysis -> review draft -> save -> resolve by process.
/// No process or executable is started by this interface.
pub trait StaticImporter {
    fn analyze(&self, bytes: &[u8]) -> Result<ImportDraft, String>;
}

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct ImportDraft {
    pub profile: Profile,
    pub warnings: Vec<String>,
}

#[allow(dead_code)]
pub struct SampleImporter;
impl StaticImporter for SampleImporter {
    fn analyze(&self, bytes: &[u8]) -> Result<ImportDraft, String> {
        if bytes.len() > 32 * 1024 * 1024
            || !bytes.starts_with(b"MZ")
            || format!("{:x}", Sha256::digest(bytes)) != SAMPLE_HASH
        {
            return Err("当前导入器仅识别已研究的时空异客修改器样本；未执行文件".into());
        }
        Ok(ImportDraft {
            profile: builtin(),
            warnings: vec![
                "40 项均有原生执行适配；效果仍需在实际游戏版本确认".into(),
                "游戏速度和移动速度已排除；不会自动启用任何修改".into(),
            ],
        })
    }
}

pub struct Repository {
    pub directory: PathBuf,
}
impl Repository {
    pub fn resolve(&self, process: &str) -> Result<Option<Profile>, String> {
        let embedded = builtin();
        if embedded.matches_process(process) {
            let path = self.directory.join(format!("{}.json", embedded.id));
            if !path.exists() {
                self.save(&embedded)?;
            }
        }
        if !self.directory.exists() {
            return Ok(None);
        }
        let mut found = None;
        for entry in fs::read_dir(&self.directory).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let p = read_profile(&path)?;
            if path.file_stem().and_then(|s| s.to_str()) != Some(p.id.as_str()) {
                return Err("配置文件名必须与 ID 一致".into());
            }
            if p.matches_process(process) {
                if found.is_some() {
                    return Err("存在多个匹配修改器，请先解决配置冲突".into());
                }
                found = Some(p);
            }
        }
        Ok(found)
    }

    pub fn save(&self, profile: &Profile) -> Result<PathBuf, String> {
        profile.validate()?;
        fs::create_dir_all(&self.directory).map_err(|e| e.to_string())?;
        let target = self.directory.join(format!("{}.json", profile.id));
        let temp = target.with_extension("pending");
        let bytes = serde_json::to_vec_pretty(profile).map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_PROFILE {
            return Err("配置过大".into());
        }
        use std::io::Write;
        let result = (|| {
            let mut file = fs::File::create(&temp).map_err(|e| e.to_string())?;
            file.write_all(&bytes).map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            drop(file);
            use std::os::windows::ffi::OsStrExt;
            use windows::{
                Win32::Storage::FileSystem::{
                    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
                },
                core::PCWSTR,
            };
            let from: Vec<u16> = temp.as_os_str().encode_wide().chain(Some(0)).collect();
            let to: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
            unsafe {
                MoveFileExW(
                    PCWSTR(from.as_ptr()),
                    PCWSTR(to.as_ptr()),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            }
            .map_err(|e| e.to_string())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result?;
        Ok(target)
    }

    /// Never overwrite an existing profile implicitly. Caller owns review/update UX.
    #[allow(dead_code)]
    pub fn install(&self, draft: &ImportDraft) -> Result<PathBuf, String> {
        draft.profile.validate()?;
        fs::create_dir_all(&self.directory).map_err(|e| e.to_string())?;
        let target = self.directory.join(format!("{}.json", draft.profile.id));
        if target.exists() {
            return Err("修改器配置已存在".into());
        }
        let temporary =
            self.directory
                .join(format!("{}.{}.tmp", draft.profile.id, std::process::id()));
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        let result = (|| {
            file.write_all(&serde_json::to_vec_pretty(&draft.profile).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            drop(file);
            fs::rename(&temporary, &target).map_err(|e| e.to_string())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result?;
        Ok(target)
    }
}

fn read_profile(path: &Path) -> Result<Profile, String> {
    use std::io::Read;
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(MAX_PROFILE + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_PROFILE {
        return Err("修改器配置文件过大".into());
    }
    let profile: Profile = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    profile.validate()?;
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn builtin_catalog_and_execution_boundary() {
        let p = builtin();
        p.validate().unwrap();
        assert_eq!(p.features.len(), 40);
        assert_eq!(
            p.features.iter().filter(|f| supported(&p, &f.id)).count(),
            40
        );
        assert!(p.matches_process("DIGIMON STORY TIME STRANGER.EXE"));
        assert!(!p.matches_process("Digimon Story Time Stranger.exe.bak"));
        let money = p.features.iter().find(|f| f.id == "money").unwrap();
        assert_eq!(money.parse_value("123"), Ok(Some(123.0)));
        for value in ["0", "-1", "NaN", "3.5", "2147483648"] {
            assert!(money.parse_value(value).is_err());
        }
        let mut forged = p.clone();
        forged.process_names = vec!["other.exe".into()];
        assert!(forged.validate().is_err());
        assert!(SampleImporter.analyze(b"MZunknown").is_err());
    }
    #[test]
    fn saved_profile_resolves_after_reopen_without_enabling() {
        let directory =
            std::env::temp_dir().join(format!("slackinput-profile-test-{}", std::process::id()));
        let repo = Repository {
            directory: directory.clone(),
        };
        let mut p = builtin();
        p.id = "test-import".into();
        p.process_names = vec!["sample.exe".into()];
        let draft = ImportDraft {
            profile: p,
            warnings: vec![],
        };
        let path = repo.install(&draft).unwrap();
        assert!(repo.install(&draft).is_err());
        let loaded = Repository {
            directory: directory.clone(),
        }
        .resolve("SAMPLE.EXE")
        .unwrap()
        .unwrap();
        assert!(!supported(&loaded, "money"));
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    #[ignore = "requires the user-supplied local EXE; static reads only"]
    fn sample_static_import() {
        let bytes = fs::read("dist/Digimon Story Time Stranger v1.0-v20260709 Plus 42 Trainer.exe")
            .unwrap();
        let draft = SampleImporter.analyze(&bytes).unwrap();
        draft.profile.validate().unwrap();
        assert_eq!(draft.profile.features.len(), 40);
        assert_eq!(draft.warnings.len(), 2);
        let mut altered = bytes;
        altered[200] ^= 1;
        assert!(SampleImporter.analyze(&altered).is_err());
    }
}

pub fn name_key(name: &str) -> String {
    name.trim().to_lowercase()
}

pub fn merge(
    current: &Profile,
    incoming: &Profile,
    overwrite: &HashSet<String>,
) -> Result<Profile, String> {
    current.validate()?;
    incoming.validate()?;
    if current.id != incoming.id || current.process_names != incoming.process_names {
        return Err("不同游戏配置不能合并".into());
    }
    let mut result = current.clone();
    let mut selected = HashSet::new();
    for feature in &incoming.features {
        let key = name_key(&feature.name);
        if let Some(index) = result
            .features
            .iter()
            .position(|f| name_key(&f.name) == key)
        {
            if overwrite.contains(&key) {
                result.features[index] = feature.clone();
                selected.insert(feature.id.clone());
            }
        } else {
            result.features.push(feature.clone());
            selected.insert(feature.id.clone());
        }
    }
    let retained: Vec<_> = current
        .features
        .iter()
        .filter(|f| !selected.contains(&f.id))
        .collect();
    if !selected.is_empty() {
        let old = &current.execution;
        let new = &incoming.execution;
        if old.fields != new.fields
            || old.limits != new.limits
            || old.evolution_offsets != new.evolution_offsets
        {
            if !retained.is_empty() {
                return Err("共享字段因子不同，请全部覆盖或保留现有配置".into());
            }
            result.execution.fields = new.fields.clone();
            result.execution.limits = new.limits.clone();
            result.execution.evolution_offsets = new.evolution_offsets.clone();
        }
        for id in &selected {
            if let Some(stat) = new.stats.get(id) {
                result.execution.stats.insert(id.clone(), stat.clone());
            }
            let patch_id = if id == "agent_points" {
                "money"
            } else {
                id.as_str()
            };
            if let Some(patches) = new.patches.get(patch_id) {
                if patch_id == "money"
                    && retained
                        .iter()
                        .any(|f| matches!(f.id.as_str(), "money" | "agent_points"))
                    && serde_json::to_value(&old.patches[patch_id]).unwrap()
                        != serde_json::to_value(patches).unwrap()
                {
                    return Err("金钱和点数共享定位已变化，请一并覆盖".into());
                }
                result
                    .execution
                    .patches
                    .insert(patch_id.into(), patches.clone());
            }
            if let Some(group) = super::hooks::group(id) {
                let key = format!("{group:?}");
                let changed = serde_json::to_value(&old.groups[&key]).unwrap()
                    != serde_json::to_value(&new.groups[&key]).unwrap()
                    || old.layouts.get(&key) != new.layouts.get(&key);
                if changed
                    && retained
                        .iter()
                        .any(|f| super::hooks::group(&f.id) == Some(group))
                {
                    return Err(format!("{key} 共享定位已变化，请同时覆盖同组功能"));
                }
                result
                    .execution
                    .groups
                    .insert(key.clone(), new.groups[&key].clone());
                if let Some(layout) = new.layouts.get(&key) {
                    result.execution.layouts.insert(key, layout.clone());
                }
            }
        }
    }
    result.validate()?;
    Ok(result)
}

/// AI drafts must pass validation and the same human review flow as EXE imports.
#[allow(dead_code)]
pub struct AiRequest {
    pub instruction: String,
    pub current: Profile,
}
#[allow(dead_code)]
pub trait AiDraftProvider {
    fn propose(&self, request: &AiRequest) -> Result<ImportDraft, String>;
}

#[cfg(test)]
mod management_tests {
    use super::*;
    #[test]
    fn names_merge_mixed_all_skip_and_all_overwrite() {
        let mut current = builtin();
        current.features.truncate(2);
        let mut incoming = builtin();
        incoming.features.truncate(3);
        incoming.features[0].name = format!("  {}  ", current.features[0].name);
        for f in &mut incoming.features {
            f.description = "updated".into();
        }
        let skipped = merge(&current, &incoming, &HashSet::new()).unwrap();
        assert_eq!(skipped.features.len(), 3);
        assert_eq!(
            skipped.features[0].description,
            current.features[0].description
        );
        let mixed = merge(
            &current,
            &incoming,
            &HashSet::from([name_key(&incoming.features[0].name)]),
        )
        .unwrap();
        assert_eq!(mixed.features[0].description, "updated");
        assert_eq!(
            mixed.features[1].description,
            current.features[1].description
        );
        let all = merge(
            &current,
            &incoming,
            &incoming
                .features
                .iter()
                .map(|f| name_key(&f.name))
                .collect(),
        )
        .unwrap();
        assert!(all.features.iter().all(|f| f.description == "updated"));
        assert_eq!(name_key("  MONEY "), name_key("money"));
        incoming.features[0].name = "different name same id".into();
        assert!(merge(&current, &incoming, &HashSet::new()).is_err());
    }
    #[test]
    fn deletion_and_changed_factors_survive_atomic_replace_and_empty_reload() {
        let directory = std::env::temp_dir().join(format!("trainer-delete-{}", std::process::id()));
        let repo = Repository {
            directory: directory.clone(),
        };
        let mut p = repo.resolve(GAME_PROCESS).unwrap().unwrap();
        p.features.remove(0);
        p.execution
            .stats
            .get_mut("e_digimon_level")
            .unwrap()
            .offsets = vec![128];
        repo.save(&p).unwrap();
        let loaded = repo.resolve(GAME_PROCESS).unwrap().unwrap();
        assert_eq!(loaded.features.len(), 39);
        assert_eq!(
            loaded.execution.stat_writes("e_digimon_level", 7).unwrap(),
            vec![(128, 7i32.to_le_bytes())]
        );
        p.features.clear();
        let path = repo.save(&p).unwrap();
        assert!(
            repo.resolve(GAME_PROCESS)
                .unwrap()
                .unwrap()
                .features
                .is_empty()
        );
        let mut invalid = p.clone();
        invalid.execution.groups.clear();
        assert!(repo.save(&invalid).is_err());
        assert!(
            repo.resolve(GAME_PROCESS)
                .unwrap()
                .unwrap()
                .features
                .is_empty()
        );
        fs::write(&path, b"broken JSON").unwrap();
        assert!(repo.resolve(GAME_PROCESS).is_err());
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }
    #[test]
    fn shared_factors_cannot_silently_change_skipped_options() {
        let current = builtin();
        let mut incoming = current.clone();
        incoming.execution.fields.insert("cp_current".into(), 424);
        let mixed = HashSet::from([name_key(&incoming.features[0].name)]);
        assert!(merge(&current, &incoming, &mixed).is_err());
        let all = incoming
            .features
            .iter()
            .map(|f| name_key(&f.name))
            .collect();
        assert_eq!(
            merge(&current, &incoming, &all).unwrap().execution.fields["cp_current"],
            424
        );
        assert_eq!(
            merge(&current, &incoming, &HashSet::new())
                .unwrap()
                .execution
                .fields["cp_current"],
            420
        );
    }
    #[test]
    fn invalid_factors_and_duplicate_names_rejected() {
        let mut p = builtin();
        p.features[1].name = p.features[0].name.clone();
        assert!(p.validate().is_err());
        let mut c = super::super::config::Execution::default();
        c.groups.get_mut("Battle").unwrap()[0].pattern = "* * *".into();
        assert!(c.validate().is_err());
        let mut c = super::super::config::Execution::default();
        c.stats.get_mut("e_digimon_level").unwrap().scale = i32::MAX;
        assert!(c.stat_writes("e_digimon_level", 2).is_err());
    }
}
