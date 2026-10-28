// Re-export config types from peri-acp (single source of truth)
// Re-export store functions from peri-acp
use thiserror::Error;

pub use peri_acp::provider::{
    AppConfig, PeriConfig, ProfileConfig, Profiles, ProviderConfig, ProviderModels,
};
pub use peri_acp::provider::{
    ConfigSource, config_path, load, load_from, save_to, set_global_config_path,
    workspace_config_path,
};
pub use peri_config::{ConfigurationRevision, ConfigurationSnapshot};

pub mod tui_config;
pub use tui_config::TuiConfig;

/// 显式 revision 保存的失败类别。
///
/// 面板据此区分「配置已变化，重新载入」与普通 I/O/权限失败：前者不是一次可重试的
/// 写入，必须由用户决定是否放弃草稿。
#[derive(Debug, Error)]
pub enum SaveEffectiveError {
    #[error("config source is unavailable")]
    SourceUnavailable,
    #[error("configuration authority unavailable")]
    AuthorityUnavailable,
    #[error(transparent)]
    Settings(#[from] peri_config::settings::SettingsError),
}

impl SaveEffectiveError {
    /// 是否为可由“重新载入”恢复的 revision 冲突（而非磁盘、权限或需重启的基线变化）。
    ///
    /// `GlobalBaselineChanged` 明确要求重启（工作区保存不能静默替换全局基线），
    /// 重新载入不会让它变得可保存，因此不归入本类，避免误导用户。
    pub fn is_conflict(&self) -> bool {
        matches!(
            self,
            Self::Settings(
                peri_config::settings::SettingsError::Conflict
                    | peri_config::settings::SettingsError::Resolution(
                        peri_config::ConfigurationError::Conflict
                    )
            )
        )
    }
}

/// 当前已发布配置的 revision（进入长草稿时捕获一次）。
///
/// 在 Ctrl+S 时重新取 revision 会让旧草稿覆盖外部写入；长草稿必须在进入编辑/内联
/// editor 时捕获本值，并交给 [`save_effective_at`]。
pub fn configuration_revision() -> Result<ConfigurationRevision, SaveEffectiveError> {
    let source = crate::kit::atoms::CONFIG_SOURCE_HANDLE
        .get()
        .ok_or(SaveEffectiveError::SourceUnavailable)?;
    source
        .snapshot()
        .map(|snapshot| snapshot.revision())
        .ok_or(SaveEffectiveError::AuthorityUnavailable)
}

/// 从配置 authority 重新解析当前生效配置（用户显式重载，不写文件）。
///
/// 用于 revision 冲突后的恢复：调用方随后必须用新的 [`configuration_revision`]
/// 重新基线化草稿。只返回 authority 接纳的快照；发布到内存句柄/投影由调用方决定。
pub fn reload_effective() -> Result<PeriConfig, SaveEffectiveError> {
    let source = crate::kit::atoms::CONFIG_SOURCE_HANDLE
        .get()
        .ok_or(SaveEffectiveError::SourceUnavailable)?;
    Ok(source.reload_merged()?)
}

/// 以显式 expected revision 保存到当前生效层，返回 authority 接纳后的配置快照。
///
/// 成功返回值是发布到内存句柄 / 投影 / ACP 的唯一依据：调用方不得发布原始 candidate。
pub fn save_effective_at(
    expected_revision: ConfigurationRevision,
    cfg: &PeriConfig,
) -> Result<PeriConfig, SaveEffectiveError> {
    let source = crate::kit::atoms::CONFIG_SOURCE_HANDLE
        .get()
        .ok_or(SaveEffectiveError::SourceUnavailable)?;
    let snapshot = source.save(expected_revision, cfg)?;
    Ok(snapshot.settings().clone())
}

/// 保存到当前生效层（写回路径决策在 [`ConfigSource`] 加载时一次性确定）。
///
/// 立即编辑入口：保存时读取最新 revision 后委托 [`save_effective_at`]；长草稿
/// （Login / Model 内联 editor）必须改用显式 revision 入口，避免旧草稿覆盖外部写入。
///
/// `handle` 未初始化时返回 Err（正常 TUI 生命周期内不会发生）。
pub fn save_effective(cfg: &PeriConfig) -> anyhow::Result<()> {
    let revision = configuration_revision()?;
    save_effective_at(revision, cfg)?;
    Ok(())
}

#[cfg(test)]
#[path = "types_test.rs"]
mod tests;
