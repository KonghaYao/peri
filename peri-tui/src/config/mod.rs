// Re-export config types from peri-acp (single source of truth)
// Re-export store functions from peri-acp
pub use peri_acp::provider::{
    AppConfig, PeriConfig, ProfileConfig, Profiles, ProviderConfig, ProviderModels,
};
pub use peri_acp::provider::{
    ConfigSource, config_path, load, load_from, save_to, set_global_config_path,
    workspace_config_path,
};

pub mod tui_config;
pub use tui_config::TuiConfig;

/// 保存到当前生效层（写回路径决策在 [`ConfigSource`] 加载时一次性确定）。
///
/// TUI 保存点统一入口：从 [`crate::kit::atoms::CONFIG_SOURCE_HANDLE`] 取
/// 配置源（启动时 set 一次），内部不做任何路径探测/决策——加载与保存共享
/// 同一决策，杜绝"用工作区配置却写全局文件"的漂移。
///
/// `handle` 未初始化时返回 Err（正常 TUI 生命周期内不会发生）。
pub fn save_effective(cfg: &PeriConfig) -> anyhow::Result<()> {
    let source = crate::kit::atoms::CONFIG_SOURCE_HANDLE
        .get()
        .ok_or_else(|| anyhow::anyhow!("CONFIG_SOURCE_HANDLE 未初始化（配置源缺失）"))?;
    source.save(cfg)
}

#[cfg(test)]
#[path = "types_test.rs"]
mod tests;
