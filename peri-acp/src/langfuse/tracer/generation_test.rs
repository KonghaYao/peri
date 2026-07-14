use super::*;

#[test]
fn test_on_llm_start_sets_active_step() {
    let mut t = GenerationTracker::new();
    let start = t.on_llm_start(0, vec![], vec![]);
    assert!(start.gen_id.starts_with("gen_"));
}

#[test]
fn test_on_llm_end_returns_generation_end_and_clears_state() {
    let mut t = GenerationTracker::new();
    t.on_llm_start(0, vec![], vec![]);
    let end = t.on_llm_end(0).expect("should return Some");
    assert!(end.gen_id.starts_with("gen_"));
    // 再次 on_llm_end 应返回 None
    assert!(t.on_llm_end(0).is_none());
}

#[test]
fn test_on_llm_retrying_accumulates_attempts() {
    let mut t = GenerationTracker::new();
    t.on_llm_start(0, vec![], vec![]);
    t.on_llm_retrying(1, 3, 1000, "timeout");
    t.on_llm_retrying(2, 3, 2000, "timeout");
    let end = t.on_llm_end(0).expect("should return Some");
    assert!(end.retry_metadata.is_some());
    let meta = end.retry_metadata.unwrap();
    assert!(meta.to_string().contains("timeout"));
}

#[test]
fn test_on_llm_start_clears_previous_retry_attempts() {
    // 第二次 on_llm_start 应清空 retry_attempts
    let mut t = GenerationTracker::new();
    t.on_llm_start(0, vec![], vec![]);
    t.on_llm_retrying(1, 3, 1000, "err");
    t.on_llm_start(1, vec![], vec![]); // 新 step
    let end = t.on_llm_end(1).expect("should return Some");
    assert!(end.retry_metadata.is_none(), "新 step 不应携带旧 retry");
}

#[test]
fn test_on_llm_end_unknown_step_returns_none() {
    let mut t = GenerationTracker::new();
    assert!(t.on_llm_end(99).is_none());
}

#[test]
fn test_on_llm_request_payload_supplements_body() {
    let mut t = GenerationTracker::new();
    t.on_llm_start(0, vec![], vec![]);
    t.on_llm_request_payload(
        0,
        std::sync::Arc::new(serde_json::json!({"model": "claude-4.7"})),
    );
    let end = t.on_llm_end(0).expect("should return Some");
    assert_eq!(end.input_json["model"], "claude-4.7");
}
