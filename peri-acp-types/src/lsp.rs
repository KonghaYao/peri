//! LSP 服务器配置契约。
//!
//! 自 `peri-lsp/src/config.rs` 迁入（3.0 批 2 波 1：协议类型归契约层；
//! peri-lsp 保留 re-export 保兼容）。加载/展开逻辑留在 peri-lsp。

use std::{collections::HashMap, path::PathBuf};

use serde::{Deserialize, Serialize};

/// LSP 服务器配置来源
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspConfigSource {
    Global(PathBuf),
    Plugin { plugin_name: String },
}

/// 单个 LSP 服务器配置（兼容 Claude Code settings.json 的 lspServers 格式）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspServerConfig {
    /// 服务器显示名称
    #[serde(default)]
    pub name: String,
    /// 可执行命令
    pub command: String,
    /// 命令参数
    #[serde(default)]
    pub args: Vec<String>,
    /// 传递给子进程的环境变量
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<HashMap<String, String>>,
    /// 文件扩展名到语言 ID 的映射
    #[serde(default, rename = "extensionToLanguage")]
    pub extension_to_language: HashMap<String, String>,
    /// 初始化选项
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "initializationOptions"
    )]
    pub initialization_options: Option<serde_json::Value>,
    /// 是否禁用
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
    /// 最大重启次数
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "maxRestarts"
    )]
    pub max_restarts: Option<u32>,
    /// 启动超时（毫秒）
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "startupTimeout"
    )]
    pub startup_timeout: Option<u64>,
    /// 配置来源标记（运行时使用，不序列化）
    #[serde(skip)]
    pub source: Option<LspConfigSource>,
}
