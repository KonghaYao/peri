use super::*;

#[test]
fn test_context_warning_serde_roundtrip() {
    let ev = ExecutorEvent::ContextWarning {
        used_tokens: 150000,
        total_tokens: 200000,
        percentage: 75.0,
    };
    let json = serde_json::to_string(&ev).unwrap();
    let deserialized: ExecutorEvent = serde_json::from_str(&json).unwrap();
    if let ExecutorEvent::ContextWarning {
        used_tokens,
        total_tokens,
        percentage,
    } = deserialized
    {
        assert_eq!(used_tokens, 150000);
        assert_eq!(total_tokens, 200000);
        assert!((percentage - 75.0).abs() < 0.01);
    } else {
        panic!("Deserialized to wrong variant");
    }
}

#[test]
fn test_llm_retrying_serde_roundtrip() {
    let ev = ExecutorEvent::LlmRetrying {
        attempt: 2,
        max_attempts: 5,
        delay_ms: 2000,
        error: "API 错误 503: Service Unavailable".to_string(),
    };
    let json = serde_json::to_string(&ev).unwrap();
    let deserialized: ExecutorEvent = serde_json::from_str(&json).unwrap();
    if let ExecutorEvent::LlmRetrying {
        attempt,
        max_attempts,
        delay_ms,
        error,
    } = deserialized
    {
        assert_eq!(attempt, 2);
        assert_eq!(max_attempts, 5);
        assert_eq!(delay_ms, 2000);
        assert_eq!(error, "API 错误 503: Service Unavailable");
    } else {
        panic!("Deserialized to wrong variant");
    }
}

#[test]
fn test_subagent_started_serde_roundtrip() {
    let ev = ExecutorEvent::SubagentStarted {
        agent_name: "test-agent".to_string(),
        instance_id: "sub_test123".to_string(),
        is_background: false,
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains(r#""type":"subagent_started""#));
    assert!(json.contains(r#""agent_name":"test-agent""#));
    assert!(json.contains(r#""instance_id":"sub_test123""#));
    let deserialized: ExecutorEvent = serde_json::from_str(&json).unwrap();
    if let ExecutorEvent::SubagentStarted {
        agent_name,
        instance_id,
        is_background,
    } = deserialized
    {
        assert_eq!(agent_name, "test-agent");
        assert_eq!(instance_id, "sub_test123");
        assert!(!is_background);
    } else {
        panic!("Deserialized to wrong variant");
    }
}

#[test]
fn test_subagent_stopped_serde_roundtrip() {
    let ev = ExecutorEvent::SubagentStopped {
        agent_name: "test-agent".to_string(),
        result: "done".to_string(),
        is_error: false,
        instance_id: "sub_test456".to_string(),
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains(r#""type":"subagent_stopped""#));
    let deserialized: ExecutorEvent = serde_json::from_str(&json).unwrap();
    if let ExecutorEvent::SubagentStopped {
        agent_name,
        result,
        is_error,
        instance_id,
    } = deserialized
    {
        assert_eq!(agent_name, "test-agent");
        assert_eq!(result, "done");
        assert!(!is_error);
        assert_eq!(instance_id, "sub_test456");
    } else {
        panic!("Deserialized to wrong variant");
    }
}

#[test]
fn test_compact_started_serde() {
    let ev = ExecutorEvent::CompactStarted {
        turn_id: String::new(),
        agent_id: String::new(),
        step: 0,
        strategy: CompactStrategy::Smart,
        trigger: CompactTrigger::Auto,
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains(r#""type":"compact_started""#));
    let deserialized: ExecutorEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(deserialized, ExecutorEvent::CompactStarted { .. }));
}

#[test]
fn test_compact_completed_serde_roundtrip() {
    // full compact 场景：summary 非空，micro_cleared 为 0
    let ev = ExecutorEvent::CompactCompleted {
        summary: "对话摘要内容".to_string(),
        files: vec![
            CompactFileInfo {
                path: "src/main.rs".to_string(),
                lines: 42,
            },
            CompactFileInfo {
                path: "src/lib.rs".to_string(),
                lines: 15,
            },
        ],
        skills: vec!["code-review".to_string(), "refactor".to_string()],
        micro_cleared: 0,
        messages: vec![],
        token_before: 0,
        token_after: 0,
        strategy: CompactStrategy::Smart,
        affected_count: 0,
        estimated_tokens_saved: 0,
        estimated_tokens_before: 0,
        estimated_tokens_after: 0,
        changed_messages: 0,
        changed_fields: 0,
        no_op_candidates: 0,
        full_escalation_reason: None,
        cache_hit_rate_before: 0.0,
        outcome: crate::agent::compact_v2::CompactOutcome::SmartApplied,
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains(r#""type":"compact_completed""#));
    assert!(json.contains(r#""summary":"对话摘要内容""#));
    assert!(json.contains(r#""path":"src/main.rs""#));
    assert!(json.contains(r#""skills":["code-review","refactor"]"#));
    let deserialized: ExecutorEvent = serde_json::from_str(&json).unwrap();
    if let ExecutorEvent::CompactCompleted {
        summary,
        files,
        skills,
        micro_cleared,
        ..
    } = deserialized
    {
        assert_eq!(summary, "对话摘要内容");
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "src/main.rs");
        assert_eq!(files[0].lines, 42);
        assert_eq!(files[1].path, "src/lib.rs");
        assert_eq!(files[1].lines, 15);
        assert_eq!(skills, vec!["code-review", "refactor"]);
        assert_eq!(micro_cleared, 0);
    } else {
        panic!("Deserialized to wrong variant");
    }
}

#[test]
fn test_compact_completed_micro_serde() {
    // micro-compact 场景：summary 为空，micro_cleared > 0
    let ev = ExecutorEvent::CompactCompleted {
        summary: String::new(),
        files: vec![],
        skills: vec![],
        micro_cleared: 8,
        messages: vec![],
        token_before: 0,
        token_after: 0,
        strategy: CompactStrategy::Smart,
        affected_count: 0,
        estimated_tokens_saved: 0,
        estimated_tokens_before: 0,
        estimated_tokens_after: 0,
        changed_messages: 0,
        changed_fields: 0,
        no_op_candidates: 0,
        full_escalation_reason: None,
        cache_hit_rate_before: 0.0,
        outcome: crate::agent::compact_v2::CompactOutcome::MicroApplied,
    };
    let json = serde_json::to_string(&ev).unwrap();
    let deserialized: ExecutorEvent = serde_json::from_str(&json).unwrap();
    if let ExecutorEvent::CompactCompleted {
        summary,
        files,
        skills,
        micro_cleared,
        ..
    } = deserialized
    {
        assert!(summary.is_empty());
        assert!(files.is_empty());
        assert!(skills.is_empty());
        assert_eq!(micro_cleared, 8);
    } else {
        panic!("Deserialized to wrong variant");
    }
}

#[test]
fn test_compact_error_serde_roundtrip() {
    let ev = ExecutorEvent::CompactError {
        message: "LLM 调用超时".to_string(),
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains(r#""type":"compact_error""#));
    assert!(json.contains(r#""message":"LLM 调用超时""#));
    let deserialized: ExecutorEvent = serde_json::from_str(&json).unwrap();
    if let ExecutorEvent::CompactError { message } = deserialized {
        assert_eq!(message, "LLM 调用超时");
    } else {
        panic!("Deserialized to wrong variant");
    }
}

#[test]
fn test_rewind_error_serde_roundtrip() {
    let ev = ExecutorEvent::RewindError {
        message: "rewind: 未找到目标消息 abc".to_string(),
    };
    let json = serde_json::to_string(&ev).unwrap();
    assert!(json.contains(r#""type":"rewind_error""#));
    assert!(json.contains(r#""message":"rewind: 未找到目标消息 abc""#));
    let deserialized: ExecutorEvent = serde_json::from_str(&json).unwrap();
    if let ExecutorEvent::RewindError { message } = deserialized {
        assert_eq!(message, "rewind: 未找到目标消息 abc");
    } else {
        panic!("Deserialized to wrong variant");
    }
}
