use super::*;

#[test]
fn test_on_llm_start_sets_active_step() {
    let mut t = GenerationTracker::new();
    let start = t.on_llm_start("main", 0, vec![], vec![]);
    assert!(start.gen_id.starts_with("gen_"));
}

#[test]
fn test_on_llm_end_returns_generation_end_and_clears_state() {
    let mut t = GenerationTracker::new();
    t.on_llm_start("main", 0, vec![], vec![]);
    let end = t.on_llm_end("main", 0).expect("should return Some");
    assert!(end.gen_id.starts_with("gen_"));
    // 再次 on_llm_end 应返回 None
    assert!(t.on_llm_end("main", 0).is_none());
}

#[test]
fn test_on_llm_retrying_accumulates_attempts() {
    let mut t = GenerationTracker::new();
    t.on_llm_start("main", 0, vec![], vec![]);
    t.on_llm_retrying("main", 0, 1, 3, 1000, "timeout");
    t.on_llm_retrying("main", 0, 2, 3, 2000, "timeout");
    let end = t.on_llm_end("main", 0).expect("should return Some");
    assert!(end.retry_metadata.is_some());
    let meta = end.retry_metadata.unwrap();
    assert_eq!(meta["retry_count"], 2);
    assert_eq!(meta["retries"][0]["delay_ms"], 1000);
    assert!(meta.get("error").is_none());
}

#[test]
fn test_on_llm_start_clears_previous_retry_attempts() {
    // 第二次 on_llm_start 应清空 retry_attempts（按 generation key 隔离）
    let mut t = GenerationTracker::new();
    t.on_llm_start("main", 0, vec![], vec![]);
    t.on_llm_retrying("main", 0, 1, 3, 1000, "err");
    t.on_llm_start("main", 1, vec![], vec![]); // 新 step
    let end = t.on_llm_end("main", 1).expect("should return Some");
    assert!(end.retry_metadata.is_none(), "新 step 不应携带旧 retry");
}

#[test]
fn test_interleaved_agents_keep_their_own_retries() {
    // 并行 agent 交错：A start → B start → A retry → A end → B retry → B end
    // 每个 end 只能消费自己 generation 的 retry 历史
    let mut t = GenerationTracker::new();
    t.on_llm_start("agent_a", 5, vec![], vec![]);
    t.on_llm_start("agent_b", 1, vec![], vec![]);
    t.on_llm_retrying("agent_a", 5, 1, 3, 500, "a-timeout");
    t.on_llm_retrying("agent_a", 5, 2, 3, 1000, "a-timeout");
    // A 先 end：只应携带 A 自己的 retry，不能消费到 B 的
    let end_a = t
        .on_llm_end("agent_a", 5)
        .expect("A end should return Some");
    let meta_a = end_a.retry_metadata.expect("A should carry its retries");
    assert_eq!(meta_a["retry_count"], 2);
    assert_eq!(meta_a["retries"][0]["delay_ms"], 500);
    assert!(meta_a.get("error").is_none());
    // B 后 end：只应携带 B 自己的 retry
    t.on_llm_retrying("agent_b", 1, 1, 2, 800, "b-timeout");
    let end_b = t
        .on_llm_end("agent_b", 1)
        .expect("B end should return Some");
    let meta_b = end_b.retry_metadata.expect("B should carry its retries");
    assert_eq!(meta_b["retry_count"], 1);
    assert_eq!(meta_b["retries"][0]["delay_ms"], 800);
    assert!(meta_b.get("error").is_none());
}

#[test]
fn test_on_llm_end_unknown_step_returns_none() {
    let mut t = GenerationTracker::new();
    assert!(t.on_llm_end("main", 99).is_none());
}

#[test]
fn test_on_llm_request_payload_supplements_body() {
    let mut t = GenerationTracker::new();
    t.on_llm_start("main", 0, vec![], vec![]);
    t.on_llm_request_payload(
        "main",
        0,
        std::sync::Arc::new(serde_json::json!({"model": "claude-4.7"})),
    );
    let end = t.on_llm_end("main", 0).expect("should return Some");
    assert_eq!(end.input_json["model"], "claude-4.7");
}
