use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Result;

use super::config::PeriConfig;

/// 进程级全局配置路径重定向（None 表示未设置，使用默认路径）。
///
/// 由部署装配点（CLI 入口）在启动早期调用一次；相对路径按启动时 cwd 解析为绝对路径。
static CONFIG_PATH_OVERRIDE: Mutex<Option<PathBuf>> = Mutex::new(None);

/// 全局配置文件路径。
///
/// 已通过 [`set_global_config_path`] 设置重定向时返回重定向路径，否则返回默认
/// `~/.peri/settings.json`。
pub fn config_path() -> PathBuf {
    CONFIG_PATH_OVERRIDE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_else(default_config_path)
}

/// 默认配置文件路径：~/.peri/settings.json
fn default_config_path() -> PathBuf {
    dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".peri")
        .join("settings.json")
}

/// 进程级重定向全局配置文件路径；`None` 复位为默认路径。
///
/// 由部署装配点（CLI 入口）在启动早期调用一次，之后 [`config_path()`]、
/// [`load()`]、[`save()`] 均跟随该路径。相对路径按启动时 cwd 解析为绝对路径。
pub fn set_global_config_path(path: Option<PathBuf>) {
    let resolved = path.map(|p| {
        if p.is_relative() {
            std::env::current_dir()
                .ok()
                .map(|c| c.join(&p))
                .unwrap_or(p)
        } else {
            p
        }
    });
    *CONFIG_PATH_OVERRIDE
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = resolved;
}

/// 工作区配置文件路径：{cwd}/.peri/settings.json
/// 文件不存在时返回 None
pub fn workspace_config_path() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let path = cwd.join(".peri").join("settings.json");
    if path.exists() {
        Some(path)
    } else {
        None
    }
}

/// 加载配置（全局 + 工作区合并），文件不存在时返回默认空配置
///
/// 先加载 ~/.peri/settings.json 获取全局配置，
/// 再检测当��工作目录的 .peri/settings.json 是否存在，
/// 若存在则加载并以工作区字段覆盖全局对应字段。
pub fn load() -> Result<PeriConfig> {
    let mut merged = load_from(&config_path())?;
    if let Some(ws_path) = workspace_config_path() {
        let workspace = load_from(&ws_path)?;
        merged.config.merge_overrides(workspace.config);
    }
    Ok(merged)
}

/// 从指定路径加载配置
pub fn load_from(path: &Path) -> Result<PeriConfig> {
    if !path.exists() {
        return Ok(PeriConfig::default());
    }
    let content = std::fs::read_to_string(path)?;
    let cfg: PeriConfig = serde_json::from_str(&content)?;
    Ok(cfg)
}

/// 原子写回配置文件（先写临时文件，再 rename，避免写入中断导致文件损坏）
pub fn save(cfg: &PeriConfig) -> Result<()> {
    save_to(cfg, &config_path())
}

/// 将配置写入指定路径
pub fn save_to(cfg: &PeriConfig, path: &Path) -> Result<()> {
    // 确保目录存在
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let content = serde_json::to_string_pretty(cfg)?;

    // atomic write
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, content)?;
    std::fs::rename(&tmp_path, path)?;

    Ok(())
}

#[cfg(test)]
#[path = "store_test.rs"]
mod store_tests;
