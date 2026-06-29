//! Skill DTOs -- 取代 peri_middlewares::skills::loader::SkillMetadata

use serde::{Deserialize, Serialize};

/// Skill 元数据（对齐 peri_middlewares::skills::loader::SkillMetadata）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkillMetadataDto {
    pub name: String,
    pub description: String,
    pub source: SkillSourceDto,
    pub disabled: bool,
}

/// Skill 来源（对齐 peri_middlewares::skills::loader::SkillSource）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SkillSourceDto {
    Builtin,
    User,
    Project,
    Plugin { plugin_name: String },
    Agm,
}
