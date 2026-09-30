//! 配置数据面读取适配器（F12；不读任何技能内容）。
//!
//! 只读取 workspace 实例资源 provider 的 builtin 关闭位，不提供技能根。
//! 技能内容由 MCP 侧读取，`skills/` 不含任何 `std::fs` 调用。
//!
//! 读取位置由配置数据面统一维护，尊重 ACP `--config-file` 路径；
//! `disableBundledSkills` 支持嵌套 `{"config":{"disableBundledSkills":…}}`
//! 与扁平两种形态。

use std::path::PathBuf;

/// 配置数据面的全局配置文件路径，默认 `~/.peri/settings.json`。
pub fn global_config_path() -> PathBuf {
    peri_mcp_config::global_config_path()
}

/// 从全局配置权威路径读取 `disableBundledSkills` 配置（默认 false）。
///
/// F12（保留为宿主适配器）：session/new 时一次性读取并冻结，会话内不再重新读取
/// （保持系统提示词稳定性）。W4b 起它是 workspace 资源 provider 的**输入位**
/// （`disable_bundled`），不再是宿主扫描的开关。
pub fn load_disable_bundled_skills() -> bool {
    load_disable_bundled_skills_from_path(&global_config_path())
}

/// 测试注入入口：从指定 settings 文件读取 disableBundledSkills。
pub fn load_disable_bundled_skills_from_path(path: &std::path::Path) -> bool {
    let Ok(content) = peri_mcp_config::read_text(path) else {
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
