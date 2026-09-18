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
            || self.features.is_empty()
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
        // Metadata may describe new games, but it cannot opt into native execution by
        // claiming this adapter's identity with altered fields or process bindings.
        if self.id == builtin().id {
            let canonical = builtin();
            if serde_json::to_value(self).map_err(|e| e.to_string())?
                != serde_json::to_value(canonical).map_err(|e| e.to_string())?
            {
                return Err("内置适配器元数据不匹配，请勿修改其执行配置".into());
            }
        }
        Ok(())
    }
}

/// Future import button: analyze -> review draft -> install -> resolve by process.
/// No process or executable is started by this interface.
#[allow(dead_code)] // Public extension seam; the import button is a later milestone.
pub trait StaticImporter {
    fn analyze(&self, bytes: &[u8]) -> Result<ImportDraft, String>;
}

#[allow(dead_code)]
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
            return Ok(Some(embedded));
        }
        if !self.directory.exists() {
            return Ok(None);
        }
        let mut found = None;
        for entry in fs::read_dir(&self.directory)
            .map_err(|e| e.to_string())?
            .take(257)
        {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let p = read_profile(&path)?;
            if p.matches_process(process) {
                if found.is_some() {
                    return Err("存在多个匹配修改器，请先解决配置冲突".into());
                }
                found = Some(p);
            }
        }
        Ok(found)
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
