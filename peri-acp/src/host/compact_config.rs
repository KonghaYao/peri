//! Compact 配置装配辅助（L5 l5-shell：自 `host/exec/compact_pipeline.rs` 归位）。
//!
//! [`load_compact_config`] 依赖 ACP `PeriConfig`（provider 配置），按
//! 「每轮重新应用 env overrides」语义预填 `CommandContext.compact_config`——
//! 装配是 ACP 协议面职责（§0），执行体（`peri-agent::session::exec::compact_pipeline`）
//! 不触碰 ACP 配置类型。

/// 加载 compact 配置：`unwrap_or_default()` 后立即应用 env overrides。
///
/// [TRAP] env 优先级 DISABLE_COMPACT / DISABLE_AUTO_COMPACT / COMPACT_THRESHOLD 每轮
/// 重新读取（非 frozen），apply_env_overrides() 必须在 unwrap_or_default() 之后调用。
pub fn load_compact_config(
    peri_config: &crate::provider::PeriConfig,
) -> peri_acp_types::compact::CompactConfig {
    let mut compact_config = peri_config.config.compact.clone().unwrap_or_default();
    compact_config.apply_env_overrides();
    compact_config
}
