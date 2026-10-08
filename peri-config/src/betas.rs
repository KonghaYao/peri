//! Beta flag 有效值投影（`ConfigurationSnapshot::flags()`）。
//!
//! 规则（设计 §配置面接入）：
//! - 合并：global 与 workspace 逐 key 覆盖（workspace 同名键胜出），显式 false 可
//!   关闭 global 的 true；差异提取与 MetaHarness 同款（见 `app::AppConfig`）。
//! - 校验：键必须在注册表内；未知键在解析后剔除并 warn，不进入本投影。
//! - 投影：由**合并后 settings** 派生；未覆盖与未知 id 一律 false。
//!
//! 类型 [`BetaFlags`] 定义在契约 crate（`peri-acp-types`）：它同时是会话冻结载体
//! （`FrozenContext`）的成员，消费方（Bash / Agent 装配面）不重新解析 flag 语义。

use crate::app::PeriConfig;

pub use peri_acp_types::beta_flags::{BetaFlagOrigin, BetaFlagValue, BetaFlags};

/// 由合并后 settings 派生有效值，并标注每个覆盖的**来源层**（诊断用）。
///
/// `global` 是同一 scope 的 global 层：值与 global 相同的键归属 Global，其余
/// （workspace 覆盖、或 workspace 独有键）归属 Workspace。同一 key 在两层取相同值
/// 时报告 Global——它不影响有效值，只影响诊断口径。
pub(crate) fn resolve(global: &PeriConfig, merged: &PeriConfig) -> BetaFlags {
    BetaFlags::from_values(merged.config.betas.overrides.iter().map(|(id, enabled)| {
        let origin = match global.config.betas.get(id) {
            Some(global_value) if global_value == *enabled => BetaFlagOrigin::Global,
            _ => BetaFlagOrigin::Workspace,
        };
        (
            id.clone(),
            BetaFlagValue {
                enabled: *enabled,
                origin,
            },
        )
    }))
}

/// 只有合并视图（没有可对照的 global 层）时的投影。
///
/// 来源层在这里无法判定（合并结果本身不携带它），全部按
/// [`BetaFlagOrigin::Global`] 标注——它只出现在无来源层对照的遗留/测试构造路径，
/// 不影响有效值与消费语义。生产新建会话走 [`crate::settings::ConfigSource::beta_flags`]
/// （由快照投影，来源层可判）。
pub fn from_merged(merged: &PeriConfig) -> BetaFlags {
    resolve(merged, merged)
}

#[cfg(test)]
#[path = "betas_test.rs"]
mod tests;
