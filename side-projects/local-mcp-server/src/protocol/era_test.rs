//! `protocol::era` 的单元测试（WP-005）。

use super::{era_of, is_modern, Era};
use crate::wire::{MCP_LEGACY_PROTOCOL_VERSION, MCP_MODERN_PROTOCOL_VERSION};

#[test]
fn test_known_versions_map_to_their_era() {
    assert_eq!(Era::of(MCP_MODERN_PROTOCOL_VERSION), Era::Modern);
    assert_eq!(Era::of(MCP_LEGACY_PROTOCOL_VERSION), Era::Legacy);
    assert_eq!(era_of(Some(MCP_MODERN_PROTOCOL_VERSION)), Era::Modern);
    assert_eq!(era_of(Some(MCP_LEGACY_PROTOCOL_VERSION)), Era::Legacy);
}

#[test]
fn test_undetermined_version_is_legacy_shape() {
    // 未协商前不得给出 modern 专属字段（ttlMs/cacheScope/resultType）。
    assert!(!is_modern(None));
    assert_eq!(era_of(None), Era::Legacy);
}

#[test]
fn test_version_comparison_is_monotonic_for_iso_dates() {
    // 未来的修订版继承"每请求自描述"语义，因此按日期单调比较而不是白名单匹配。
    assert!(is_modern(Some("2027-01-01")));
    // 早于 modern 基线（含本服务不支持的旧版本）按 legacy 形状处理。
    assert!(!is_modern(Some("2025-06-18")));
    assert!(!is_modern(Some("1900-01-01")));
}

#[test]
fn test_era_names_are_stable_for_evidence() {
    assert_eq!(Era::Legacy.as_str(), "legacy");
    assert_eq!(Era::Modern.as_str(), "modern");
    assert!(!Era::Legacy.is_modern());
    assert!(Era::Modern.is_modern());
}
