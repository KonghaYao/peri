use super::*;

#[test]
fn test_decode_turn_done() {
    let decoded = AcpEventData::decode("turn-done", serde_json::json!({}));
    match decoded {
        AcpEventData::TurnDone => {}
        _ => panic!("expected TurnDone"),
    }
}

#[test]
fn test_decode_turn_interrupted() {
    let data = serde_json::json!({"reason": "user cancelled"});
    let decoded = AcpEventData::decode("turn-interrupted", data);
    match decoded {
        AcpEventData::TurnInterrupted { reason, request_id } => {
            assert_eq!(reason, "user cancelled");
            assert_eq!(request_id, None, "requestId 缺失时应为 None");
        }
        _ => panic!("expected TurnInterrupted"),
    }
}

#[test]
fn test_decode_turn_interrupted_with_request_id() {
    let data = serde_json::json!({"reason": "user cancelled", "requestId": "rid-1"});
    let decoded = AcpEventData::decode("turn-interrupted", data);
    match decoded {
        AcpEventData::TurnInterrupted { reason, request_id } => {
            assert_eq!(reason, "user cancelled");
            assert_eq!(request_id.as_deref(), Some("rid-1"));
        }
        _ => panic!("expected TurnInterrupted"),
    }
}

#[test]
fn test_decode_tool_count() {
    let data = serde_json::json!({"count": 3});
    let decoded = AcpEventData::decode("tool-count", data);
    match decoded {
        AcpEventData::ToolCount(tc) => assert_eq!(tc.count, 3),
        _ => panic!("expected ToolCount"),
    }
}

#[test]
fn test_decode_budget_warning() {
    let data = serde_json::json!({
        "used": 85000,
        "limit": 100000,
        "threshold": "0.85"
    });
    let decoded = AcpEventData::decode("budget-warning", data);
    match decoded {
        AcpEventData::BudgetWarning(bw) => assert_eq!(bw.threshold, "0.85"),
        _ => panic!("expected BudgetWarning"),
    }
}

#[test]
fn test_decode_system_notification() {
    let data = serde_json::json!({"text": "model switched", "level": "info"});
    let decoded = AcpEventData::decode("system-notification", data);
    match decoded {
        AcpEventData::SystemNotification(sn) => assert_eq!(sn.level, "info"),
        _ => panic!("expected SystemNotification"),
    }
}

#[test]
fn test_decode_prediction() {
    let data = serde_json::json!({"text": "fix typo"});
    let decoded = AcpEventData::decode("prediction", data);
    match decoded {
        AcpEventData::Prediction(p) => assert_eq!(p.text, "fix typo"),
        _ => panic!("expected Prediction"),
    }
}

#[test]
fn test_decode_file_suggestions() {
    let data = serde_json::json!({"files": ["src/main.rs", "src/lib.rs"]});
    let decoded = AcpEventData::decode("file-suggestions", data);
    match decoded {
        AcpEventData::FileSuggestions(fs) => assert_eq!(fs.files.len(), 2),
        _ => panic!("expected FileSuggestions"),
    }
}

#[test]
fn test_decode_rewind_preview() {
    let data = serde_json::json!({"files": [], "messages": []});
    let decoded = AcpEventData::decode("rewind-preview", data);
    match decoded {
        AcpEventData::RewindPreview(rp) => assert!(rp.files.is_empty()),
        _ => panic!("expected RewindPreview"),
    }
}

#[test]
fn test_decode_oauth_needed() {
    let data = serde_json::json!({
        "server_name": "github-mcp",
        "auth_url": "https://github.com/login/oauth"
    });
    let decoded = AcpEventData::decode("oauth-needed", data);
    match decoded {
        AcpEventData::OauthNeeded(on) => assert_eq!(on.server_name, "github-mcp"),
        _ => panic!("expected OauthNeeded"),
    }
}

#[test]
fn test_decode_subagent_started() {
    let data = serde_json::json!({
        "agent_id": "sa-1",
        "agent_name": "file-searcher"
    });
    let decoded = AcpEventData::decode("subagent-started", data);
    match decoded {
        AcpEventData::SubagentStarted { agent_name, .. } => {
            assert_eq!(agent_name, "file-searcher")
        }
        _ => panic!("expected SubagentStarted"),
    }
}

#[test]
fn test_decode_subagent_stopped() {
    // legacy 通道缺省：无 result/is_error 字段 → 空字符串 / false（向后兼容）
    let data = serde_json::json!({"agent_id": "sa-1"});
    let decoded = AcpEventData::decode("subagent-stopped", data);
    match decoded {
        AcpEventData::SubagentStopped {
            agent_id,
            result,
            is_error,
        } => {
            assert_eq!(agent_id, "sa-1");
            assert_eq!(result, "", "legacy 缺省 result 应为空");
            assert!(!is_error, "legacy 缺省 is_error 应为 false");
        }
        _ => panic!("expected SubagentStopped"),
    }
    // 显式字段（canonical 主通道 peri/agent_event）
    let data = serde_json::json!({
        "agent_id": "sa-2",
        "result": "loop failed: llm error",
        "is_error": true
    });
    let decoded = AcpEventData::decode("subagent-stopped", data);
    match decoded {
        AcpEventData::SubagentStopped {
            agent_id,
            result,
            is_error,
        } => {
            assert_eq!(agent_id, "sa-2");
            assert_eq!(result, "loop failed: llm error");
            assert!(is_error);
        }
        _ => panic!("expected SubagentStopped"),
    }
}

#[test]
fn test_decode_unknown_event_name() {
    let data = serde_json::json!({"foo": "bar"});
    let decoded = AcpEventData::decode("future-event", data);
    match decoded {
        AcpEventData::Unknown { event, data } => {
            assert_eq!(event, "future-event");
            assert_eq!(data["foo"], "bar");
        }
        _ => panic!("expected Unknown"),
    }
}

#[test]
fn test_decode_malformed_data_falls_to_unknown() {
    let data = serde_json::json!("not an object");
    let decoded = AcpEventData::decode("future-event-xyz", data);
    match decoded {
        AcpEventData::Unknown { event, .. } => assert_eq!(event, "future-event-xyz"),
        _ => panic!("expected Unknown for malformed data"),
    }
}
