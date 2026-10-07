use super::*;
use crate::session_resources::work::{WorkBudget, WorkRecord, WorkStage};

fn legacy_state() -> WorkState {
    WorkState::new(WorkLimits {
        reason_requests: 64,
        dispatches: 256,
        ..WorkLimits::default()
    })
}

#[test]
fn defaults_match_bounded_semantic_iteration_policy() {
    let limits = WorkLimits::default();
    assert_eq!(limits.reason_requests, DEFAULT_AGENT_MAX_ITERATIONS as u64);
    assert_eq!(limits.dispatches, limits.reason_requests * 4);
    assert_eq!(limits.recoveries, 8);
    assert!(limits.reason_requests > 64);
    assert!(limits.dispatches > 256);
    assert!(limits.dispatches < u64::MAX);
}

#[test]
fn loading_preserves_legacy_policy_and_custom_limits_are_not_upgraded() {
    let legacy = legacy_state();
    let serialized = serde_json::to_string(&legacy).unwrap();
    let loaded: WorkState = serde_json::from_str(&serialized).unwrap();
    assert_eq!(loaded, legacy);
    let custom_limits = [
        WorkLimits {
            reason_requests: 32,
            ..legacy.limits.clone()
        },
        WorkLimits {
            dispatches: 128,
            ..legacy.limits.clone()
        },
        WorkLimits {
            recoveries: 4,
            ..legacy.limits.clone()
        },
        WorkLimits {
            max_batch_size: 16,
            ..legacy.limits.clone()
        },
        WorkLimits {
            reason_requests: 1000,
            ..legacy.limits.clone()
        },
    ];
    for limits in custom_limits {
        let mut state = WorkState::new(limits);
        let before = state.clone();
        assert!(!state.upgrade_default_budget_policy_for_user_selection());
        assert_eq!(state, before);
    }
}

#[test]
fn explicit_selection_upgrades_once_without_resetting_old_work_or_budget() {
    let mut state = legacy_state();
    state.revision = 490;
    state.budgets.insert(
        "old-budget".into(),
        WorkBudget {
            reason_requests: 64,
            dispatches: 76,
            recoveries: 2,
        },
    );
    state.works.insert(
        "old-work".into(),
        WorkRecord {
            work_id: "old-work".into(),
            revision: 12,
            budget_id: "old-budget".into(),
            batch_id: "old-batch".into(),
            stage: WorkStage::Blocked,
            resume_stage: Some(WorkStage::ReasonReady),
            request_id: None,
            reason_request: None,
            response: None,
            invocation_ids: Vec::new(),
            reason: Some("reason budget exhausted".into()),
            recovery_condition: Some("explicit budget reset authorization".into()),
        },
    );
    let mut expected = state.clone();
    expected.limits = WorkLimits::default();
    assert!(state.upgrade_default_budget_policy_for_user_selection());
    assert_eq!(state, expected);
    assert!(!state.upgrade_default_budget_policy_for_user_selection());
    assert_eq!(state, expected);
    let restored: WorkState =
        serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
    assert_eq!(restored, expected);
}
