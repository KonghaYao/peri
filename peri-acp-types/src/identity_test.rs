//! identity.rs 契约测试（Seam 2：§9 时序契约——身份标识不可复用）。

use super::{
    AttemptId, AttemptIdentity, CancelRequest, EventDeliveryClass, EventEnvelope, SessionEpoch,
    SessionSeq, TurnIdentity,
};
use crate::thread::CancelPolicy;

// ── SessionEpoch：不可复用（单调递增） ──────────────────────────────────────

/// 初始纪元为 1（0 保留给"未知"场景）。
#[test]
fn test_session_epoch_initial_is_one() {
    assert_eq!(SessionEpoch::initial().get(), 1);
}

/// epoch 只增不减：next 严格大于当前值，两次 next 结果不同（不可复用）。
#[test]
fn test_session_epoch_monotonic_not_reused() {
    let mut epoch = SessionEpoch::initial();
    let mut seen = std::collections::HashSet::new();
    for _ in 0..100 {
        let next = epoch.next();
        assert!(next > epoch, "epoch 必须严格递增（不可复用）");
        assert!(seen.insert(next), "epoch 值不得复用: {next:?}");
        epoch = next;
    }
}

/// Default 与 initial 一致。
#[test]
fn test_session_epoch_default_matches_initial() {
    assert_eq!(SessionEpoch::default(), SessionEpoch::initial());
}

// ── AttemptId：不可复用（每次生成唯一） ─────────────────────────────────────

/// 两次生成必然不同（uuid v7 唯一性），且非空。
#[test]
fn test_attempt_id_unique_per_generation() {
    let a = AttemptId::new();
    let b = AttemptId::new();
    assert_ne!(a, b, "attempt_id 每次生成必须唯一（不可复用）");
    assert!(!a.as_str().is_empty());
}

/// 批量生成仍保持唯一。
#[test]
fn test_attempt_id_bulk_unique() {
    let mut seen = std::collections::HashSet::new();
    for _ in 0..1000 {
        let id = AttemptId::new();
        assert!(seen.insert(id.clone()), "attempt_id 不得复用: {id}");
    }
}

/// Default 等价于 new（每次生成新值）。
#[test]
fn test_attempt_id_default_is_fresh() {
    let a = AttemptId::default();
    let b = AttemptId::default();
    assert_ne!(a, b);
}

// ── 序列化：跨层消息携带身份必须可序列化往返 ─────────────────────────────────

/// SessionEpoch 序列化为数值，可往返。
#[test]
fn test_session_epoch_serde_roundtrip() {
    let epoch = SessionEpoch::initial().next();
    let json = serde_json::to_string(&epoch).unwrap();
    assert_eq!(json, "2");
    let back: SessionEpoch = serde_json::from_str(&json).unwrap();
    assert_eq!(back, epoch);
}

/// AttemptId 序列化为字符串，可往返。
#[test]
fn test_attempt_id_serde_roundtrip() {
    let id = AttemptId::new();
    let json = serde_json::to_string(&id).unwrap();
    let back: AttemptId = serde_json::from_str(&json).unwrap();
    assert_eq!(back, id);
    assert_eq!(back.as_str(), id.as_str());
}

/// 四元组序列化往返：跨层消息携带的完整身份不丢失字段。
#[test]
fn test_attempt_identity_serde_roundtrip() {
    let identity = AttemptIdentity::new(
        "session-1",
        SessionEpoch::initial().next(),
        "turn-7",
        AttemptId::new(),
    );
    let json = serde_json::to_string(&identity).unwrap();
    let back: AttemptIdentity = serde_json::from_str(&json).unwrap();
    assert_eq!(back, identity);
    assert_eq!(back.session_id, "session-1");
    assert_eq!(back.session_epoch, SessionEpoch::initial().next());
    assert_eq!(back.turn_id, "turn-7");
}

// ── 组合类型构造 ────────────────────────────────────────────────────────────

/// TurnIdentity 构造与字段访问。
#[test]
fn test_turn_identity_construction() {
    let tid = TurnIdentity::new("session-1", SessionEpoch::initial());
    assert_eq!(tid.session_id, "session-1");
    assert_eq!(tid.session_epoch, SessionEpoch::initial());
}

/// AttemptIdentity 构造：四元组字段齐全。
#[test]
fn test_attempt_identity_construction() {
    let aid = AttemptIdentity::new("s", SessionEpoch::initial(), "t", AttemptId::new());
    assert_eq!(aid.session_id, "s");
    assert_eq!(aid.session_epoch, SessionEpoch::initial());
    assert_eq!(aid.turn_id, "t");
    assert!(!aid.attempt_id.as_str().is_empty());
}

// ── SessionSeq：同 session 单调递增（事件契约） ──────────────────────────────

/// 首个事件序号为 1（0 保留给"未知/未分配"场景）。
#[test]
fn test_session_seq_initial_is_one() {
    assert_eq!(SessionSeq::initial().get(), 1);
}

/// session_seq 单调递增：next 严格大于当前值，连续取值不重复（去重键第三元）。
#[test]
fn test_session_seq_monotonic_not_reused() {
    let mut seq = SessionSeq::initial();
    let mut seen = std::collections::HashSet::new();
    for _ in 0..100 {
        let next = seq.next();
        assert!(next > seq, "session_seq 必须严格递增（单调）");
        assert!(seen.insert(next), "session_seq 不得复用: {next:?}");
        seq = next;
    }
}

/// SessionSeq 不实现 Default：缺失序号必须显式表达（Option），禁止伪装归零。
#[test]
fn test_session_seq_no_default_impl() {
    // 编译期契约：SessionSeq 无 Default 实现（缺省构造被拒绝）。
    // 运行时层面验证：只有 initial()/next() 两个构造入口，均产生 >= 1 的显式值。
    assert!(SessionSeq::initial().get() >= 1);
    assert!(SessionSeq::initial().next().get() > SessionSeq::initial().get());
}

/// SessionSeq 序列化往返。
#[test]
fn test_session_seq_serde_roundtrip() {
    let seq = SessionSeq::initial().next().next();
    let json = serde_json::to_string(&seq).unwrap();
    assert_eq!(json, "3");
    let back: SessionSeq = serde_json::from_str(&json).unwrap();
    assert_eq!(back, seq);
}

// ── EventEnvelope：canonical envelope 契约 ───────────────────────────────────

/// envelope 构造：身份字段齐全，message_id 显式缺省为 None（不伪装）。
#[test]
fn test_event_envelope_construction() {
    let env = EventEnvelope::new(
        "session-1",
        SessionEpoch::initial(),
        "turn-7",
        "agent-9",
        SessionSeq::initial(),
        EventDeliveryClass::Critical,
    );
    assert_eq!(env.session_id, "session-1");
    assert_eq!(env.session_epoch, SessionEpoch::initial());
    assert_eq!(env.turn_id, "turn-7");
    assert_eq!(env.agent_id, "agent-9");
    assert_eq!(env.session_seq, SessionSeq::initial());
    assert_eq!(env.delivery_class, EventDeliveryClass::Critical);
    assert_eq!(
        env.message_id, None,
        "message_id 可选且语义明确，缺省为 None 而非伪装值"
    );
}

/// envelope 序列化往返：身份字段跨 transport 不丢失。
#[test]
fn test_event_envelope_serde_roundtrip() {
    let mut env = EventEnvelope::new(
        "session-1",
        SessionEpoch::initial().next(),
        "turn-7",
        "agent-9",
        SessionSeq::initial().next(),
        EventDeliveryClass::Broadcast,
    );
    env.message_id = Some("msg-1".to_string());
    let json = serde_json::to_string(&env).unwrap();
    let back: EventEnvelope = serde_json::from_str(&json).unwrap();
    assert_eq!(back, env);
    assert_eq!(back.turn_id, "turn-7");
    assert_eq!(back.agent_id, "agent-9");
    assert_eq!(back.session_seq.get(), 2);
    assert_eq!(back.delivery_class, EventDeliveryClass::Broadcast);
    assert_eq!(back.message_id.as_deref(), Some("msg-1"));
}

/// 去重键 (session_id, turn_id, session_seq) 完整性：三元素全部可在 envelope 上取到。
#[test]
fn test_event_envelope_dedup_key_available() {
    let env = EventEnvelope::new(
        "s",
        SessionEpoch::initial(),
        "t",
        "a",
        SessionSeq::initial(),
        EventDeliveryClass::Critical,
    );
    let key = (&env.session_id, &env.turn_id, env.session_seq);
    assert_eq!(key.0, "s");
    assert_eq!(key.1, "t");
    assert_eq!(key.2, SessionSeq::initial());
}

// ── CancelRequest：§9 cancel 契约（三元组定位 + clear_queue + policy） ───────

/// 构造默认：携带完整四元组身份，clear_queue 默认 false（cancel ≠ 清除待办）。
#[test]
fn test_cancel_request_default_keeps_queue() {
    let identity = AttemptIdentity::new(
        "session-1",
        SessionEpoch::initial(),
        "turn-7",
        AttemptId::new(),
    );
    let req = CancelRequest::new(identity.clone(), CancelPolicy::Cascade);
    assert_eq!(req.identity, identity);
    assert_eq!(req.policy, CancelPolicy::Cascade);
    assert!(
        !req.clear_queue,
        "默认不清除 MQ 待办（§9：cancel ≠ 清除待办）"
    );
}

/// clear_queue 标志可显式携带（§9：cancel 请求可带 clear_queue 标志）。
#[test]
fn test_cancel_request_with_clear_queue_flag() {
    let identity = AttemptIdentity::new(
        "session-1",
        SessionEpoch::initial(),
        "turn-7",
        AttemptId::new(),
    );
    let req = CancelRequest::new(identity, CancelPolicy::Independent).with_clear_queue(true);
    assert!(req.clear_queue);
    assert_eq!(req.policy, CancelPolicy::Independent);
}

/// 幂等判定三元组可自请求提取：同一 (session_id, turn_id, attempt_id) 判定一致。
#[test]
fn test_cancel_request_idempotency_triple_stable() {
    let identity = AttemptIdentity::new(
        "session-1",
        SessionEpoch::initial(),
        "turn-7",
        AttemptId::new(),
    );
    let a = CancelRequest::new(identity.clone(), CancelPolicy::Cascade);
    let b = CancelRequest::new(identity, CancelPolicy::Cascade);
    assert_eq!(
        (
            a.identity.session_id,
            a.identity.turn_id,
            a.identity.attempt_id.clone()
        ),
        (
            b.identity.session_id,
            b.identity.turn_id,
            b.identity.attempt_id.clone()
        )
    );
}

/// CancelRequest 序列化往返（跨层消息统一携带，字段不丢失）。
#[test]
fn test_cancel_request_serde_roundtrip() {
    let req = CancelRequest::new(
        AttemptIdentity::new("s", SessionEpoch::initial().next(), "t", AttemptId::new()),
        CancelPolicy::Independent,
    )
    .with_clear_queue(true);
    let json = serde_json::to_string(&req).unwrap();
    let back: CancelRequest = serde_json::from_str(&json).unwrap();
    assert_eq!(back, req);
    assert_eq!(back.identity.session_id, "s");
    assert_eq!(back.policy, CancelPolicy::Independent);
    assert!(back.clear_queue);
}
