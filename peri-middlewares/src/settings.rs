//! 宿主配置读取适配器（F12；不读任何技能内容）。
//!
//! 语义与迁移前 `skills::load_global_skills_dir` / `skills::load_disable_bundled_skills`
//! **逐字保留**（只换归属模块）：它们是 workspace 实例资源 provider 的**输入位**
//! （技能根列表 + builtin 关闭位），不是技能来源——W4b（J5）后宿主侧技能文件系统
//! 读取点为零，本模块因此独立于 `skills` 模块存在，便于静态断言
//! （`peri-middlewares/src/skills/` 不含任何 `std::fs` 调用）。
//!
//! 读取位置是**全局** `~/.peri/settings.json`（`HOME` 优先，与迁移前一致）；
//! `skillsDir` 支持嵌套 `{"config":{"skillsDir":…}}` 与扁平两种形态，
//! `disableBundledSkills` 同理。

use std::path::PathBuf;

/// 全局配置文件路径：~/.peri/settings.json
pub fn global_config_path() -> PathBuf {
    dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".peri")
        .join("settings.json")
}

/// 从全局配置中加载 skills_dir 路径。
///
/// F12（保留为宿主适配器）：只读**配置**（`skillsDir`），不读任何技能内容；
/// 产出的路径作为 workspace 实例的资源根输入（J5：技能内容的读取归 MCP 侧）。
pub fn load_global_skills_dir() -> Option<PathBuf> {
    let path = global_config_path();
    if !path.exists() {
        return None;
    }

    let content = std::fs::read_to_string(&path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;

    // 支持嵌套 { "config": { "skillsDir": "..." } } 或扁平 { "skillsDir": "..." }
    let skills_dir = json
        .get("config")
        .and_then(|c| c.get("skillsDir"))
        .or_else(|| json.get("skillsDir"))
        .and_then(|v| v.as_str())
        .map(PathBuf::from);

    skills_dir.filter(|p| !p.as_os_str().is_empty())
}

/// 从 `~/.peri/settings.json` 读取 `disableBundledSkills` 配置（默认 false）。
///
/// F12（保留为宿主适配器）：session/new 时一次性读取并冻结，会话内不再重新读取
/// （保持系统提示词稳定性）。W4b 起它是 workspace 资源 provider 的**输入位**
/// （`disable_bundled`），不再是宿主扫描的开关。
pub fn load_disable_bundled_skills() -> bool {
    load_disable_bundled_skills_from_path(&global_config_path())
}

/// 测试注入入口：从指定 settings 文件读取 disableBundledSkills。
pub fn load_disable_bundled_skills_from_path(path: &std::path::Path) -> bool {
    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(json): Result<serde_json::Value, _> = serde_json::from_str(&content) else {
        return false;
    };
    // 支持嵌套 { "config": { "disableBundledSkills": ... } } 或扁平
    json.get("config")
        .and_then(|c| c.get("disableBundledSkills"))
        .or_else(|| json.get("disableBundledSkills"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

#[cfg(test)]
#[path = "settings_test.rs"]
mod tests;
