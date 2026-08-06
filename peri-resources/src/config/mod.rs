//! peri-config — 直操配置文件（settings.json 等）。
//!
//! 本迁移点仅落子模块骨架与配置路径入口；
//! 配置读取语义之外的配置逻辑不迁移（伞形 PRD 非目标）。

use std::path::PathBuf;

/// 全局配置目录 `~/.peri`
pub fn peri_dir() -> Option<PathBuf> {
    dirs_next::home_dir().map(|home| home.join(".peri"))
}

/// 全局配置文件 `~/.peri/settings.json`
pub fn settings_path() -> Option<PathBuf> {
    peri_dir().map(|dir| dir.join("settings.json"))
}
