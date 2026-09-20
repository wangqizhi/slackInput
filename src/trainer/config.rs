//! Serializable location data; native adapters own instruction semantics.
use super::hooks::Variant;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchRecipe {
    pub pattern: String,
    pub offset: usize,
    pub original: Vec<u8>,
    pub replacement: Vec<u8>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stat {
    pub offsets: Vec<usize>,
    pub scale: i32,
    pub float: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Execution {
    pub address_resolution: String,
    pub groups: BTreeMap<String, Vec<Variant>>,
    pub layouts: BTreeMap<String, String>,
    pub patches: BTreeMap<String, Vec<PatchRecipe>>,
    pub stats: BTreeMap<String, Stat>,
    pub fields: BTreeMap<String, i32>,
    pub limits: BTreeMap<String, i32>,
    pub evolution_offsets: Vec<i32>,
}
impl Default for Execution {
    fn default() -> Self {
        serde_json::from_str(include_str!("../../assets/trainers/digimon-execution.json"))
            .expect("valid execution defaults")
    }
}
impl Execution {
    pub fn validate(&self) -> Result<(), String> {
        let defaults = Self::default();
        if self.address_resolution != "main_module_unique_aob"
            || self.groups.keys().ne(defaults.groups.keys())
            || self.patches.keys().ne(defaults.patches.keys())
            || self.layouts.keys().ne(defaults.layouts.keys())
            || self.stats.keys().ne(defaults.stats.keys())
        {
            return Err("执行配置缺少适配组或定位方式不支持".into());
        }
        if self.fields.keys().ne(defaults.fields.keys())
            || self.limits.keys().ne(defaults.limits.keys())
            || self.fields.values().any(|v| !(-4096..=4096).contains(v))
            || self.limits.values().any(|v| *v < 1)
            || self.fields["money"] > 127
            || self.fields["money"] < 0
            || self.fields["agent_points"] > 127
            || self.fields["agent_points"] < 0
            || self.fields["object_id"] < 0
            || self.evolution_offsets.is_empty()
            || self.evolution_offsets.len() > 64
            || self
                .evolution_offsets
                .iter()
                .any(|v| !(0..=4092).contains(v) || v % 4 != 0)
        {
            return Err("字段偏移或固定数值无效".into());
        }
        for (group, variants) in &self.groups {
            if variants.is_empty() || variants.len() > 16 {
                return Err("特征变体数量无效".into());
            }
            for v in variants {
                pattern(&v.pattern)?;
                let length = pattern(&v.original)?;
                // The native instruction builder assumes these instruction shapes.
                if v.offset > 4096
                    || !(5..=16).contains(&length)
                    || !defaults.groups[group]
                        .iter()
                        .any(|d| d.original == v.original)
                {
                    return Err("原生钩子指令契约不匹配".into());
                }
            }
        }
        for value in self.layouts.values() {
            pattern(value)?;
        }
        for (id, variants) in &self.patches {
            if variants.is_empty() || variants.len() > 16 {
                return Err("补丁数量无效".into());
            }
            for v in variants {
                pattern(&v.pattern)?;
                if v.offset > 4096
                    || v.original.is_empty()
                    || v.original.len() > 32
                    || (id != "money" && v.original.len() != v.replacement.len())
                    || (id == "money"
                        && (v.original != defaults.patches[id][0].original
                            || !v.replacement.is_empty()))
                {
                    return Err("补丁原字节或替换长度无效".into());
                }
            }
        }
        for stat in self.stats.values() {
            if stat.offsets.is_empty()
                || stat.offsets.len() > 4
                || stat.scale <= 0
                || stat.offsets.iter().any(|o| *o > 4092 || o % 4 != 0)
            {
                return Err("属性偏移或缩放无效".into());
            }
        }
        Ok(())
    }
    pub fn stat_writes(&self, id: &str, value: i32) -> Result<Vec<(usize, [u8; 4])>, String> {
        let stat = self.stats.get(id).ok_or("未知属性")?;
        let scaled = value.checked_mul(stat.scale).ok_or("属性缩放溢出")?;
        let bytes = if stat.float {
            (scaled as f32).to_le_bytes()
        } else {
            scaled.to_le_bytes()
        };
        Ok(stat.offsets.iter().map(|offset| (*offset, bytes)).collect())
    }
}
fn pattern(text: &str) -> Result<usize, String> {
    let parts: Vec<_> = text.split_whitespace().collect();
    if parts.is_empty()
        || parts.len() > 256
        || parts.iter().all(|p| *p == "*")
        || parts
            .iter()
            .any(|p| *p != "*" && (p.len() != 2 || u8::from_str_radix(p, 16).is_err()))
    {
        return Err("AOB 特征格式无效".into());
    }
    Ok(parts.len())
}
