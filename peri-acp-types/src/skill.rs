//! Skill DTOs -- 取代 peri_middlewares::skills::loader::SkillMetadata

use serde::{Deserialize, Serialize};

/// Skill 元数据（对齐 peri_middlewares::skills::loader::SkillMetadata）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkillMetadataDto {
    pub name: String,
    pub description: String,
    pub path: String,
    pub source: SkillSourceDto,
    pub plugin_name: Option<String>,
    /// TUI 侧额外字段：插件是否禁用（运行时无此字段，默认 false）
    #[serde(default)]
    pub disabled: bool,
}

/// Skill 来源（对齐 peri_middlewares::skills::loader::SkillSource）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SkillSourceDto {
    Builtin,
    User,
    Global,
    Project,
    Plugin,
    /// AGM 包管理器来源（仅 DTO 侧，运行时暂无对应）
    Agm,
}
