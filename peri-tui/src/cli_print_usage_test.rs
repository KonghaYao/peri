use super::*;

fn usage_update(input: u64, output: u64, read: u64, creation: u64) -> Value {
    json!({"update": {"sessionUpdate": "usage_update", "_meta": {
        "inputTokens": input, "outputTokens": output,
        "cacheReadTokens": read, "cacheCreationTokens": creation,
        "requestId": "reused-gateway-id"
    }}})
}

#[test]
fn test_print_usage_per_call_and_summary_do_not_overlap_cache() {
    let mut output = PrintOutput::new(OutputFormat::StreamJson);
    let first: Value = serde_json::from_str(
        &output
            .handle_session_update(&usage_update(100, 7, 60, 10))
            .unwrap(),
    )
    .unwrap();
    let second: Value = serde_json::from_str(
        &output
            .handle_session_update(&usage_update(250, 9, 80, 20))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(first["type"], "assistant");
    assert_ne!(
        first["message"]["id"], second["message"]["id"],
        "不同调用不可因网关 ID 复用被去重"
    );
    assert_eq!(
        first["message"]["usage"],
        json!({"input_tokens": 30, "cache_read_input_tokens": 60, "cache_creation_input_tokens": 10, "output_tokens": 7})
    );
    assert_eq!(second["message"]["usage"]["input_tokens"], 150);
    assert_eq!(
        output.result(StopReason::EndTurn, None),
        json!({"type": "result", "stop_reason": "end_turn", "status": "completed", "is_error": false, "usage": {
        "input_tokens": 180, "cache_read_input_tokens": 140,
        "cache_creation_input_tokens": 30, "output_tokens": 16
    }, "total_cost_usd": null})
    );
}

#[test]
fn test_print_usage_missing_invalid_and_replay_are_not_counted() {
    let mut output = PrintOutput::new(OutputFormat::StreamJson);
    let mut replay = usage_update(100, 7, 60, 10);
    replay["update"]["_meta"]["periReplay"] = json!(true);
    for update in [
        json!({"update": {"sessionUpdate": "usage_update", "used": 100, "size": 200000}}),
        usage_update(10, 7, 60, 10),
        replay,
    ] {
        assert!(output.handle_session_update(&update).is_none());
    }
    assert!(
        output.result(StopReason::EndTurn, None)["usage"].is_null(),
        "未知 usage 不能伪装成零"
    );
}

#[test]
fn test_print_usage_without_cache_fields_or_request_id() {
    let mut output = PrintOutput::new(OutputFormat::StreamJson);
    let event: Value = serde_json::from_str(&output.handle_session_update(&json!({
        "update": {"sessionUpdate": "usage_update", "_meta": {"inputTokens": 12, "outputTokens": 0}}
    })).unwrap()).unwrap();
    assert_eq!(
        event["message"]["usage"],
        json!({"input_tokens": 12, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0, "output_tokens": 0})
    );
    assert!(!event["message"]["id"].as_str().unwrap().is_empty());
}

#[test]
fn test_print_usage_does_not_change_text_or_json_output() {
    for format in [OutputFormat::Text, OutputFormat::Json] {
        let mut output = PrintOutput::new(format);
        assert!(
            output
                .handle_session_update(&usage_update(100, 7, 60, 10))
                .is_none()
        );
        assert!(output.handle_session_update(&json!({"update": {"sessionUpdate": "agent_message_chunk", "content": {"text": "OK"}}})).is_none());
        assert_eq!(output.text_buffer, "OK");
    }
}
