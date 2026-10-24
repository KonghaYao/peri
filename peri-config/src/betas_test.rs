//! `betas` 投影测试：来源层标注与默认 false。

use super::*;
use crate::app::PeriConfig;
use peri_acp_types::beta_flags::FULL_ASYNC_TOOLS;

fn config_with(betas: &[(&str, bool)]) -> PeriConfig {
    let mut config = PeriConfig::default();
    for (id, enabled) in betas {
        config.config.betas.set(*id, *enabled);
    }
    config
}

/// global 独有键（或与 global 同值）标注 Global。
#[test]
fn test_resolve_marks_global_origin() {
    let global = config_with(&[(FULL_ASYNC_TOOLS, true)]);
    let flags = resolve(&global, &global.clone());
    assert!(flags.is_enabled(FULL_ASYNC_TOOLS));
    assert_eq!(
        flags.value(FULL_ASYNC_TOOLS).map(|value| value.origin),
        Some(BetaFlagOrigin::Global)
    );
}

/// workspace 覆盖（含显式 false 关闭 global true）标注 Workspace。
#[test]
fn test_resolve_marks_workspace_origin_and_false_override() {
    let global = config_with(&[(FULL_ASYNC_TOOLS, true)]);
    let mut merged = global.clone();
    merged.config.betas.set(FULL_ASYNC_TOOLS, false);
    let flags = resolve(&global, &merged);
    assert!(
        !flags.is_enabled(FULL_ASYNC_TOOLS),
        "workspace 显式 false 关闭 global true"
    );
    assert_eq!(
        flags.value(FULL_ASYNC_TOOLS).map(|value| value.origin),
        Some(BetaFlagOrigin::Workspace)
    );
}

/// 未配置来源派生空投影：一切按 false。
#[test]
fn test_resolve_without_overrides_is_all_false() {
    let global = PeriConfig::default();
    let flags = resolve(&global, &global.clone());
    assert!(flags.is_empty());
    assert!(!flags.is_enabled(FULL_ASYNC_TOOLS));
    assert!(flags.value(FULL_ASYNC_TOOLS).is_none());
}

/// 迁移键 `history-v2`：仍被配置系统识别，但不再有协议选择行为。
#[test]
fn test_history_v2_is_a_known_noop_migration_key() {
    let default = PeriConfig::default();
    assert!(!from_merged(&default).is_enabled(HISTORY_V2));
    assert!(is_known(HISTORY_V2));

    let mut configured = PeriConfig::default();
    configured.config.betas.set(HISTORY_V2, true);
    configured.config.betas.validate();
    // 投影只记录配置值；本地路径不再读取它做分支。
    assert!(from_merged(&configured).is_enabled(HISTORY_V2));
}
