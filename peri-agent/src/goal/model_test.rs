use super::*;

#[test]
fn test_thread_goal_new_生成有效_goal_id() {
    let at = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_234);
    let goal = ThreadGoal::new_at("完成 PR review".to_string(), None, at);
    assert_eq!(goal.objective, "完成 PR review");
    assert_eq!(goal.status, GoalStatus::Active);
    assert_eq!(goal.token_budget, None);
    assert!(!goal.goal_id.is_empty());
    assert_eq!(goal.created_at, chrono::DateTime::<chrono::Utc>::from(at));
    assert_eq!(goal.updated_at, goal.created_at);
}

#[test]
fn test_thread_goal_serde_roundtrip() {
    let goal = ThreadGoal::new_at(
        "测试序列化".to_string(),
        Some(100_000),
        peri_time::now_wall(),
    );
    let json = serde_json::to_string(&goal).unwrap();
    let deserialized: ThreadGoal = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.objective, goal.objective);
    assert_eq!(deserialized.token_budget, goal.token_budget);
}

#[test]
fn test_status_transitions() {
    use GoalStatus::*;
    // Active → Complete / Blocked
    assert!(Active.can_transition_to(&Complete));
    assert!(Active.can_transition_to(&Blocked));
    assert!(!Active.can_transition_to(&Active));
    // 终态不可转
    assert!(!Complete.can_transition_to(&Active));
    assert!(!Complete.can_transition_to(&Blocked));
    assert!(!Blocked.can_transition_to(&Active));
    assert!(!Blocked.can_transition_to(&Complete));
}

#[test]
fn test_is_terminal() {
    use GoalStatus::*;
    assert!(!Active.is_terminal());
    assert!(Complete.is_terminal());
    assert!(Blocked.is_terminal());
}
