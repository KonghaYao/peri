use std::time::Duration;

use serde_json::{json, Value};
use url::Url;

use crate::{
    prompt_cache::{combine_system_prompt_with_dynamic, SYSTEM_PROMPT_DYNAMIC_BOUNDARY},
    ContentBlock, JsonObject, ModelMessage, ModelRequest, ModelRuntimeConfig, RetryConfig,
    ToolCall, ToolDefinition, ToolResult,
};

use super::{request::body_for_test, AnthropicConfig};

fn config() -> AnthropicConfig {
    config_with_retry(1)
}

fn config_with_retry(max_attempts: u32) -> AnthropicConfig {
    AnthropicConfig::new(
        Url::parse("https://proxy.example.test/custom/").expect("valid endpoint"),
        "test-credential",
        "claude-test",
    )
    .with_runtime(
        ModelRuntimeConfig::default().with_retry(
            RetryConfig::default()
                .with_max_attempts(max_attempts)
                .with_base_delay(Duration::ZERO)
                .with_jitter(false),
        ),
    )
}

fn request() -> ModelRequest {
    let schema = JsonObject::from_value(json!({ "type": "object" })).expect("object");
    ModelRequest::new(vec![
        ModelMessage::system_text(format!("static\n{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}\ndynamic")),
        ModelMessage::system_text("middleware content"),
        ModelMessage::user_text("first question"),
        ModelMessage::assistant(
            vec![ContentBlock::Reasoning {
                text: "previous reasoning".into(),
                signature: Some("sig-1".into()),
            }],
            vec![ToolCall::new(
                "call-a",
                "Read",
                JsonObject::from_value(json!({ "path": "a.rs" })).expect("object"),
            )],
        ),
        ModelMessage::tool_result(ToolResult::success("call-a", "Read", "a")),
        ModelMessage::tool_result(ToolResult::error("call-b", "Write", "denied")),
    ])
    .with_tools(vec![
        ToolDefinition::new("Read", schema).with_description("read a file")
    ])
    .with_max_tokens(123)
}

#[test]
fn request_contract_preserves_system_cache_thinking_and_tool_result_order() {
    let body = body_for_test(&config().with_extended_thinking(456, "high"), &request());
    let system = body["system"].as_array().expect("cached system blocks");
    assert_eq!(system[0]["text"], "static");
    assert_eq!(system[0]["cache_control"]["type"], "ephemeral");
    assert!(system[1]["text"]
        .as_str()
        .expect("text")
        .contains("middleware content"));
    assert!(system[1].get("cache_control").is_none());
    assert_eq!(
        body["thinking"],
        json!({ "type": "enabled", "budget_tokens": 456 })
    );
    assert_eq!(body["output_config"], json!({ "effort": "high" }));
    assert_eq!(
        body["tools"][0]["input_schema"],
        json!({ "type": "object" })
    );
    assert_eq!(body["messages"][1]["content"][0]["type"], "thinking");
    assert_eq!(body["messages"][1]["content"][0]["signature"], "sig-1");
    let results = body["messages"][2]["content"]
        .as_array()
        .expect("tool results");
    assert_eq!(results[0]["tool_use_id"], "call-a");
    assert_eq!(results[1]["tool_use_id"], "call-b");
    assert_eq!(results[1]["is_error"], true);
}

/// [回归测试] 连续工具轮次必须把 prompt cache 断点推进到最新 tool_result；
/// 否则长上下文会把新增工具历史持续留在未缓存后缀，命中率逐轮下降。
#[test]
fn test_cache_breakpoint_advances_to_latest_tool_result_round() {
    let history = ModelRequest::new(vec![
        ModelMessage::user_text("run a tool"),
        ModelMessage::assistant(
            Vec::new(),
            vec![
                ToolCall::new(
                    "call-cache-a",
                    "Read",
                    JsonObject::from_value(json!({ "path": "a.rs" })).expect("object"),
                ),
                ToolCall::new(
                    "call-cache-b",
                    "Read",
                    JsonObject::from_value(json!({ "path": "b.rs" })).expect("object"),
                ),
            ],
        ),
        ModelMessage::tool_result(ToolResult::success(
            "call-cache-a",
            "Read",
            "first tool output",
        )),
        ModelMessage::tool_result(ToolResult::success(
            "call-cache-b",
            "Read",
            "second tool output",
        )),
    ]);
    let body = body_for_test(&config(), &history);
    let latest_results = body["messages"][2]["content"]
        .as_array()
        .expect("最后一条 user message 必须包含 tool results");
    assert_eq!(
        latest_results.len(),
        2,
        "同轮工具结果必须合并为一条 user message"
    );
    assert!(
        latest_results[0].get("cache_control").is_none(),
        "同一 user message 只标记最后一个可缓存 block"
    );
    assert_eq!(
        latest_results.last().expect("至少一个 tool result")["cache_control"]["type"],
        "ephemeral"
    );
    assert!(
        cache_breakpoint_count(&body) <= 4,
        "总断点数不得超过 Anthropic 上限"
    );
}

/// [回归测试] append 新工具轮次时，旧请求的语义前缀必须逐字节保持；仅允许
/// cache_control 从旧 latest 滑动为新 second-last，并在新 latest 增加断点。
#[test]
fn test_cache_breakpoint_preserves_prefix_when_tool_round_is_appended() {
    let before = tool_history_request(3, false);
    let after = tool_history_request(4, false);
    let before_body = body_for_test(&config(), &before);
    let after_body = body_for_test(&config(), &after);

    assert_eq!(
        serde_json::to_vec(&before_body["system"]).expect("system serializes"),
        serde_json::to_vec(&after_body["system"]).expect("system serializes"),
        "固定 system 必须逐字节稳定"
    );
    assert_eq!(
        serde_json::to_vec(&before_body["tools"]).expect("tools serialize"),
        serde_json::to_vec(&after_body["tools"]).expect("tools serialize"),
        "固定 tools 必须逐字节稳定"
    );
    let before_messages = before_body["messages"].as_array().expect("messages array");
    let after_messages = after_body["messages"].as_array().expect("messages array");
    assert_eq!(
        strip_cache_control(Value::Array(before_messages.clone())),
        strip_cache_control(Value::Array(
            after_messages[..before_messages.len()].to_vec()
        )),
        "append 前后的旧 message 语义前缀必须相同"
    );
    assert_eq!(
        before_messages.last().expect("before latest user")["content"][0]["cache_control"]["type"],
        "ephemeral"
    );
    assert_eq!(
        after_messages[before_messages.len() - 1]["content"][0]["cache_control"]["type"],
        "ephemeral",
        "旧 latest tool_result 在 append 后必须保留为 second-last breakpoint"
    );
    assert_eq!(
        after_messages.last().expect("after latest user")["content"][0]["cache_control"]["type"],
        "ephemeral",
        "新 latest tool_result 必须成为最新 breakpoint"
    );
    assert!(cache_breakpoint_count(&before_body) <= 4);
    assert!(cache_breakpoint_count(&after_body) <= 4);
}

/// [回归测试] 快速输入普通 Human prompt 时，上一轮 tool_result 必须仍处于
/// second-last breakpoint；否则第一次请求只能命中更旧的文本历史。
#[test]
fn test_cache_breakpoint_keeps_latest_tool_result_before_new_human_prompt() {
    let body = body_for_test(&config(), &tool_history_request(3, true));
    let messages = body["messages"].as_array().expect("messages array");
    let previous_tool_result = &messages[messages.len() - 2]["content"][0];
    let latest_human = &messages[messages.len() - 1]["content"][0];

    assert_eq!(previous_tool_result["type"], "tool_result");
    assert_eq!(previous_tool_result["cache_control"]["type"], "ephemeral");
    assert_eq!(latest_human["type"], "text");
    assert_eq!(latest_human["cache_control"]["type"], "ephemeral");
    assert!(cache_breakpoint_count(&body) <= 4);
}

fn tool_history_request(rounds: usize, append_human: bool) -> ModelRequest {
    let schema = JsonObject::from_value(json!({
        "type": "object",
        "properties": { "path": { "type": "string" } },
    }))
    .expect("object");
    let mut messages = vec![
        ModelMessage::system_text("stable system"),
        ModelMessage::user_text("inspect files"),
    ];
    for round in 0..rounds {
        let call_id = format!("call-{round}");
        messages.push(ModelMessage::assistant(
            Vec::new(),
            vec![ToolCall::new(
                call_id.clone(),
                "Read",
                JsonObject::from_value(json!({ "path": format!("{round}.rs") })).expect("object"),
            )],
        ));
        messages.push(ModelMessage::tool_result(ToolResult::success(
            call_id,
            "Read",
            format!("result-{round}"),
        )));
    }
    if append_human {
        messages.push(ModelMessage::user_text("continue quickly"));
    }
    ModelRequest::new(messages).with_tools(vec![
        ToolDefinition::new("Read", schema).with_description("read a file")
    ])
}

fn strip_cache_control(mut value: Value) -> Value {
    match &mut value {
        Value::Array(values) => {
            for value in values {
                *value = strip_cache_control(value.take());
            }
        }
        Value::Object(map) => {
            map.remove("cache_control");
            for value in map.values_mut() {
                *value = strip_cache_control(value.take());
            }
        }
        _ => {}
    }
    value
}

fn cache_breakpoint_count(value: &Value) -> usize {
    match value {
        Value::Array(values) => values.iter().map(cache_breakpoint_count).sum(),
        Value::Object(map) => {
            usize::from(map.contains_key("cache_control"))
                + map.values().map(cache_breakpoint_count).sum::<usize>()
        }
        _ => 0,
    }
}

#[test]
fn request_contract_without_cache_uses_plain_top_level_system() {
    let body = body_for_test(&config().without_cache(), &request());
    assert!(body["system"].is_string());
    assert!(!body["system"]
        .as_str()
        .expect("system text")
        .contains(SYSTEM_PROMPT_DYNAMIC_BOUNDARY));
    assert!(body["messages"][0]["content"][0]
        .get("cache_control")
        .is_none());
}

#[test]
fn system_cache_boundary_four_state_matrix_and_fallback_are_explicit() {
    let cases = [
        (
            format!("STATIC{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}\n\nDYNAMIC"),
            json!([
                { "type": "text", "text": "STATIC", "cache_control": { "type": "ephemeral" } },
                { "type": "text", "text": "DYNAMIC" },
            ]),
        ),
        (
            format!("STATIC{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}"),
            json!([
                { "type": "text", "text": "STATIC", "cache_control": { "type": "ephemeral" } },
            ]),
        ),
        (
            format!("{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}\n\nDYNAMIC"),
            json!([{ "type": "text", "text": "DYNAMIC" }]),
        ),
        (
            "LEGACY-WITHOUT-BOUNDARY".to_string(),
            json!([
                { "type": "text", "text": "LEGACY-WITHOUT-BOUNDARY", "cache_control": { "type": "ephemeral" } },
            ]),
        ),
    ];
    for (system_text, expected) in cases {
        let body = body_for_test(
            &config(),
            &ModelRequest::new(vec![
                ModelMessage::system_text(system_text),
                ModelMessage::user_text("go"),
            ]),
        );
        assert_eq!(body["system"], expected);
        assert!(!serde_json::to_string(&body["system"])
            .expect("system serializes")
            .contains(SYSTEM_PROMPT_DYNAMIC_BOUNDARY));
    }

    let empty = body_for_test(
        &config(),
        &ModelRequest::new(vec![
            ModelMessage::system_text(""),
            ModelMessage::user_text("go"),
        ]),
    );
    assert!(empty.get("system").is_none());
}

#[test]
fn repeated_system_cache_boundary_fails_closed_without_wire_leak() {
    let system = format!(
        "STATIC{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}DYNAMIC{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}TAIL"
    );
    let body = body_for_test(
        &config(),
        &ModelRequest::new(vec![
            ModelMessage::system_text(system),
            ModelMessage::user_text("go"),
        ]),
    );
    let blocks = body["system"].as_array().expect("system blocks");

    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0]["text"], "STATICDYNAMICTAIL");
    assert!(blocks[0].get("cache_control").is_none());
    assert!(!serde_json::to_string(blocks)
        .expect("blocks serialize")
        .contains(SYSTEM_PROMPT_DYNAMIC_BOUNDARY));
}

#[test]
fn duplicate_boundary_plus_dynamic_contribution_remains_uncached_and_byte_preserving() {
    let base = format!("A{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}B{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}C");
    let combined = combine_system_prompt_with_dynamic(Some(&base), "REQUEST").unwrap();
    let body = body_for_test(
        &config(),
        &ModelRequest::new(vec![
            ModelMessage::system_text(combined),
            ModelMessage::user_text("go"),
        ]),
    );
    let blocks = body["system"].as_array().expect("system blocks");
    assert_eq!(
        blocks,
        &[json!({ "type": "text", "text": "ABC\n\nREQUEST" })]
    );
    assert!(blocks[0].get("cache_control").is_none());
}

#[test]
fn system_cache_prefix_is_stable_and_dynamic_order_is_preserved() {
    let body = |dynamic: &str| {
        body_for_test(
            &config(),
            &ModelRequest::new(vec![
                ModelMessage::system_text(format!(
                    "BASE-STATIC{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}\n\n{dynamic}"
                )),
                ModelMessage::system_text("REQUEST-MIDDLEWARE"),
                ModelMessage::user_text("go"),
            ]),
        )
    };
    let first = body("BASE-DYNAMIC-A");
    let second = body("BASE-DYNAMIC-B");
    let first_system = first["system"].as_array().expect("system blocks");
    let second_system = second["system"].as_array().expect("system blocks");

    assert_eq!(
        serde_json::to_vec(&first_system[0]).expect("static block serializes"),
        serde_json::to_vec(&second_system[0]).expect("static block serializes"),
        "动态 suffix 改变时 cached block 必须逐字节稳定"
    );
    assert_eq!(
        first_system[1]["text"],
        "BASE-DYNAMIC-A\n\nREQUEST-MIDDLEWARE"
    );
    assert_eq!(
        second_system[1]["text"],
        "BASE-DYNAMIC-B\n\nREQUEST-MIDDLEWARE"
    );
    assert!(first_system[1].get("cache_control").is_none());
    assert!(second_system[1].get("cache_control").is_none());
}
