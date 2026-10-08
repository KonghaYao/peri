use super::*;
use std::sync::Arc;

/// [回归测试] Failed 只允许当前 Started handle 写入，拒绝旧连接及不存在的来源。
#[test]
fn failed_state_is_fenced_by_current_started_handle() {
    let registry = McpSkillRegistry::new();
    let old: HandleToken = Arc::new(1u32);
    let current: HandleToken = Arc::new(2u32);
    registry.mark_discovery_failed("srv", old.clone());
    assert!(registry.discovery_state("srv").is_none());
    registry.mark_discovery_started("srv", current.clone());
    registry.mark_discovery_failed("srv", old.clone());
    assert!(registry.discovery_in_progress());
    assert!(!registry.discovery_failed());
    registry.mark_discovery_failed("srv", current.clone());
    assert!(
        matches!(registry.discovery_state("srv"), Some(ServerDiscoveryState::Failed { handle }) if Arc::ptr_eq(&handle, &current))
    );
    assert!(!registry.discovery_in_progress());
    registry.mark_discovery_started("srv", old.clone());
    registry.mark_discovery_completed("srv", old.clone(), vec![]);
    registry.mark_discovery_failed("srv", old);
    assert!(!registry.discovery_failed());
    assert!(matches!(
        registry.discovery_state("srv"),
        Some(ServerDiscoveryState::Discovered { .. })
    ));
}
