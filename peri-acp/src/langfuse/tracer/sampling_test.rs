use super::*;

#[test]
fn test_rate_1_0_always_emits() {
    let mut d = SamplingDecider::new(1.0);
    for i in 0..10 {
        let turn_id = format!("turn_{}", i);
        assert!(d.should_emit(&turn_id, "sess"), "turn {} 应采样", i);
    }
}

#[test]
fn test_rate_0_never_emits() {
    let mut d = SamplingDecider::new(0.0);
    for i in 0..10 {
        let turn_id = format!("turn_{}", i);
        assert!(!d.should_emit(&turn_id, "sess"), "turn {} 不应采样", i);
    }
}

#[test]
fn test_consistent_within_same_turn() {
    let mut d = SamplingDecider::new(0.5);
    let decision1 = d.should_emit("turn_1", "sess");
    let decision2 = d.should_emit("turn_1", "sess");
    let decision3 = d.should_emit("turn_1", "sess");
    assert_eq!(decision1, decision2);
    assert_eq!(decision2, decision3);
}

#[test]
fn test_cleanup_turn_removes_decision() {
    let mut d = SamplingDecider::new(1.0);
    d.should_emit("turn_1", "sess");
    assert_eq!(d.decided_len(), 1);
    d.cleanup_turn("turn_1");
    assert_eq!(d.decided_len(), 0);
}

#[test]
fn test_cleanup_prevents_unbounded_growth() {
    let mut d = SamplingDecider::new(1.0);
    for i in 0..2000 {
        let turn_id = format!("turn_{}", i);
        d.should_emit(&turn_id, "sess");
        d.cleanup_turn(&turn_id);
    }
    assert_eq!(d.decided_len(), 0, "cleanup 后应清空");
}

#[test]
fn test_high_turn_count_triggers_emergency_cleanup() {
    let mut d = SamplingDecider::new(1.0);
    // 不调 cleanup_turn，模拟异常情况
    for i in 0..1500 {
        let turn_id = format!("turn_{}", i);
        d.should_emit(&turn_id, "sess");
    }
    // decided_len 不应无限增长，应在 1000 时触发清理
    assert!(d.decided_len() <= 1100, "实际: {}", d.decided_len());
}
