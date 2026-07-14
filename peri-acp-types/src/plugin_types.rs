//! Plugin DTOs -- 取代 peri_middlewares::plugin::{InstallScope, MarketplaceSource, ...}

use serde::{Deserialize, Serialize};

/// 插件安装范围（对齐 peri_middlewares::plugin::InstallScope）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum InstallScopeDto {
    User,
    Project,
    Local,
}

/// Marketplace 来源（对齐 peri_middlewares::plugin::MarketplaceSource）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum MarketplaceSourceDto {
    Git { url: String },
    Local { path: String },
    Registry { name: String },
}

/// 命令条目（对齐 peri_middlewares::plugin::CommandEntry）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CommandEntryDto {
    pub name: String,
    pub description: String,
    pub source: CommandSourceDto,
}

/// 命令来源（对齐 peri_middlewares::plugin::CommandSource）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum CommandSourceDto {
    Builtin,
    Plugin { plugin_id: String },
    User,
}

/// Marketplace 条目（用于面板展示）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MarketplaceEntryDto {
    pub id: String,
    pub name: String,
    pub description: String,
    pub source: MarketplaceSourceDto,
    pub installed: bool,
}

/// 插件加载结果 DTO
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginLoadResultDto {
    pub plugins: Vec<PluginInfoDto>,
    pub errors: Vec<String>,
}

/// 插件信息 DTO
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginInfoDto {
    pub name: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub enabled: bool,
}
