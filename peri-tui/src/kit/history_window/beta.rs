//! Startup projection of the history-window beta setting.

/// 从配置文档读取 TUI 本地 beta（kebab-case `history-window-v1`）。
///
/// 未配置视为**开启**：发布启用绑定 S7，本地投影默认开启；只有用户显式配置
/// `false` 才关闭。`BetasConfig::get` 保留「未配置」与「显式 false」的区别。
#[must_use]
pub fn history_window_beta_enabled(config: &crate::config::PeriConfig) -> bool {
    config
        .config
        .betas
        .get(peri_config::betas::HISTORY_WINDOW_V1)
        .unwrap_or(true)
}

/// 从合并快照的 beta 投影读取与 [`history_window_beta_enabled`] 相同的语义。
///
/// 未覆盖（`BetaFlags::value` 返回 `None`）= 开启；
/// 显式 `false` = 关闭。启动路径的快照投影与回退配置读取必须共用这一默认值
/// （发布启用绑定 S7）。
pub fn history_window_beta_enabled_from_flags(flags: &peri_config::betas::BetaFlags) -> bool {
    flags
        .value(peri_config::betas::HISTORY_WINDOW_V1)
        .is_none_or(|value| value.enabled)
}

/// 启动时把 beta 配置写入运行开关（只写一次；无热重载）。
pub fn apply_history_window_beta(enabled: bool) {
    crate::kit::atoms::HISTORY_WINDOW_V1.set(enabled);
}
