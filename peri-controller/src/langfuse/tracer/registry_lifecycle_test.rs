//! 异常诊断不得抹去已经打开的 AGENT observation 的关闭义务。

use super::*;

fn make_active_registry() -> (SubagentRegistry, String) {
    let mut registry = SubagentRegistry::new();
    registry.set_main_agent_id("main".to_string());
    registry.register_invocation("main", "call", &serde_json::json!({}), "parent-stage");
    let SubagentStartOutcome::Joined { obs, .. } =
        registry.on_subagent_start("main", "child", "worker", false)
    else {
        panic!("子 agent 必须已 join 并打开 observation");
    };
    (registry, obs.observation_id)
}

/// [回归测试] 重复 Start 将状态改为 Incomplete，旧 cleanup 只看 Active/StopReceived，
/// 因此已经打开的 observation 永远不会关闭。
#[test]
fn test_duplicate_start_turn_end_closes_open_observation_once() {
    let (mut registry, observation_id) = make_active_registry();
    assert!(matches!(
        registry.on_subagent_start("main", "child", "worker", false),
        SubagentStartOutcome::Duplicate
    ));
    let closed = registry.cleanup_turn_end();
    assert_eq!(closed.len(), 1, "异常诊断不能丢失已打开观测的收尾");
    assert_eq!(closed[0].observation_id, observation_id);
    assert_eq!(
        closed[0].incomplete_reason,
        Some(IncompleteReason::DuplicateStart)
    );
    assert_eq!(
        registry.status_of("child"),
        Some(&SubagentStatus::Incomplete(
            IncompleteReason::DuplicateStart
        ))
    );
    assert!(
        registry.cleanup_turn_end().is_empty(),
        "重复收尾不得重复关闭"
    );
}

/// [回归测试] StopReceived 后重复 Stop 不能让 turn-end 丢掉原结果及关闭事件。
#[test]
fn test_duplicate_stop_turn_end_closes_open_observation_once() {
    let (mut registry, observation_id) = make_active_registry();
    assert!(registry
        .on_subagent_stop("main", "child", "first result", false)
        .is_none());
    assert!(registry
        .on_subagent_stop("main", "child", "duplicate result", true)
        .is_none());
    let closed = registry.cleanup_turn_end();
    assert_eq!(closed.len(), 1);
    assert_eq!(closed[0].observation_id, observation_id);
    assert_eq!(closed[0].output, "first result");
    assert!(!closed[0].is_error);
    assert_eq!(
        closed[0].incomplete_reason,
        Some(IncompleteReason::DuplicateStop)
    );
    assert!(registry.cleanup_turn_end().is_empty());
}

#[test]
fn test_duplicate_stop_after_closed_does_not_reopen_observation() {
    let (mut registry, observation_id) = make_active_registry();
    assert!(registry
        .on_invocation_tool_end("main", "call", "tool result", false)
        .is_none());
    let closed = registry
        .on_subagent_stop("main", "child", "done", false)
        .unwrap();
    assert_eq!(closed.observation_id, observation_id);
    assert_eq!(closed.incomplete_reason, None);
    assert!(registry
        .on_subagent_stop("main", "child", "duplicate", true)
        .is_none());
    assert!(
        registry.cleanup_turn_end().is_empty(),
        "已关闭观测遇重复 Stop 仍保持幂等"
    );
}
