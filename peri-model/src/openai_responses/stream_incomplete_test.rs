use serde_json::{json, Value};
use url::Url;

use crate::{
    runtime::stream::{SseDecoder, SseDecoders},
    transport::SseEvent,
    ContentBlock, ModelMessage, ModelRequest, ModelResult, ModelStreamEvent, StopReason,
};

use super::{request::build_request, request_test::tool, ResponsesConfig};

fn config() -> ResponsesConfig {
    ResponsesConfig::new(
        Url::parse("https://api.openai.com/v1").unwrap(),
        "fixture-key",
        "fixture-model",
    )
    .with_credential_ref("fixture-account")
}

fn decoder() -> SseDecoders {
    let built =
        build_request(&config(), &ModelRequest::default().with_tools(vec![tool()])).unwrap();
    super::stream::decoders(built.binding, built.tool_names)()
}

fn feed(decode: &SseDecoder, value: Value) -> ModelResult<Vec<ModelStreamEvent>> {
    decode(
        SseEvent {
            event: value["type"].as_str().map(str::to_owned),
            data: value.to_string(),
        },
        Some("request_a".into()),
    )
}

fn incomplete() -> Value {
    json!({"type":"response.incomplete","response":{"id":"resp_a","status":"incomplete",
        "incomplete_details":{"reason":"max_output_tokens"}}})
}

fn message(text: &str) -> Value {
    json!({"id":"msg_a","type":"message","role":"assistant","status":"incomplete",
        "content":[{"type":"output_text","text":text}]})
}

fn call(status: &str, arguments: &str) -> Value {
    json!({"id":"fc_a","type":"function_call","name":"read_file","call_id":"call_a",
        "status":status,"arguments":arguments})
}

#[test]
fn max_output_tokens_keeps_usage_partial_text_and_safe_replay() {
    let (decode, eof) = decoder();
    feed(
        &decode,
        json!({"type":"response.output_item.added","output_index":0,
        "item":{"id":"msg_a","type":"message","role":"assistant","content":[]}}),
    )
    .unwrap();
    feed(
        &decode,
        json!({"type":"response.output_text.delta","output_index":0,
        "item_id":"msg_a","content_index":0,"delta":"部分"}),
    )
    .unwrap();
    let mut terminal = incomplete();
    terminal["response"]["output"] = json!([message("部分正文")]);
    terminal["response"]["usage"] = json!({"input_tokens":30,"output_tokens":8,
        "input_tokens_details":{"cached_tokens":12,"cache_write_tokens":3}});
    let events = feed(&decode, terminal).unwrap();
    assert!(matches!(&events[0], ModelStreamEvent::TextDelta {text} if text == "正文"));
    let ModelStreamEvent::Completed(response) = events.last().unwrap() else {
        panic!("缺少 Completed")
    };
    assert_eq!(response.stop_reason(), &StopReason::MaxTokens);
    assert_eq!(response.assistant_text().as_deref(), Some("部分正文"));
    assert_eq!(response.request_id(), Some("request_a"));
    let usage = response.usage().unwrap();
    assert_eq!((usage.input_tokens, usage.output_tokens), (30, 8));
    assert_eq!(usage.cache_read_input_tokens, Some(12));
    assert_eq!(usage.cache_creation_input_tokens, Some(3));
    assert!(matches!(&events[events.len()-2], ModelStreamEvent::Usage(value) if value == usage));
    let built = build_request(
        &config(),
        &ModelRequest::new(vec![response.message().clone()]),
    )
    .unwrap();
    let input = built.body["input"].as_array().unwrap().clone();
    assert_eq!(input.len(), 1);
    assert_eq!(input[0]["content"][0]["text"], "部分正文");
    assert!(input[0].get("status").is_none());
    assert!(input[0].get("id").is_none());
    eof().unwrap();
    assert!(feed(&decode, incomplete()).is_err());
}

#[test]
fn incomplete_usage_remains_strict_and_optional() {
    for usage in [
        json!({"input_tokens":1}),
        json!({"input_tokens":1,"output_tokens":-1}),
        json!({"input_tokens":1,"output_tokens":4294967296_u64}),
        json!({"input_tokens":1,"output_tokens":2,"input_tokens_details":[]}),
        json!({"input_tokens":1,"output_tokens":2,"input_tokens_details":{"cached_tokens":2}}),
        json!("malformed"),
    ] {
        let (decode, _) = decoder();
        let mut terminal = incomplete();
        terminal["response"]["usage"] = usage;
        assert!(feed(&decode, terminal)
            .unwrap_err()
            .is_terminal_provider_failure());
    }
    for usage in [None, Some(Value::Null)] {
        let (decode, eof) = decoder();
        let mut terminal = incomplete();
        if let Some(usage) = usage {
            terminal["response"]["usage"] = usage;
        }
        let events = feed(&decode, terminal).unwrap();
        let ModelStreamEvent::Completed(response) = events.last().unwrap() else {
            panic!("缺少 Completed")
        };
        assert_eq!(response.stop_reason(), &StopReason::MaxTokens);
        assert!(response.usage().is_none());
        eof().unwrap();
    }
}

#[test]
fn other_or_missing_incomplete_reason_is_terminal() {
    for details in [
        Value::Null,
        json!({}),
        json!({"reason":"content_filter"}),
        json!({"reason":"unknown"}),
        json!({"reason":3}),
    ] {
        let (decode, _) = decoder();
        let mut terminal = incomplete();
        terminal["response"]["incomplete_details"] = details;
        assert!(feed(&decode, terminal)
            .unwrap_err()
            .is_terminal_provider_failure());
    }
    let (decode, _) = decoder();
    let mut terminal = incomplete();
    terminal["response"]
        .as_object_mut()
        .unwrap()
        .remove("incomplete_details");
    assert!(feed(&decode, terminal)
        .unwrap_err()
        .is_terminal_provider_failure());
}

#[test]
fn truncated_tools_and_reasoning_never_enter_projection_or_replay() {
    for done in [false, true] {
        let (decode, eof) = decoder();
        for (index, item) in [message("正文"), call("incomplete", "{\"path\":"),
            json!({"id":"rs_a","type":"reasoning","status":"incomplete",
                "summary":[{"type":"summary_text","text":"半截思考"}],"encrypted_content":"partial"})]
            .into_iter().enumerate() {
            let kind = if done { "response.output_item.done" } else { "response.output_item.added" };
            feed(&decode, json!({"type":kind,"output_index":index,"item":item})).unwrap();
        }
        let events = feed(&decode, incomplete()).unwrap();
        let ModelStreamEvent::Completed(response) = events.last().unwrap() else {
            panic!("缺少 Completed")
        };
        assert_eq!(response.stop_reason(), &StopReason::MaxTokens);
        let ModelMessage::Assistant {
            content,
            tool_calls,
        } = response.message()
        else {
            unreachable!()
        };
        assert!(tool_calls.is_empty());
        assert!(!content
            .iter()
            .any(|block| matches!(block, ContentBlock::Reasoning { .. })));
        let built = build_request(
            &config(),
            &ModelRequest::new(vec![response.message().clone()]),
        )
        .unwrap();
        let input = &built.body["input"];
        assert_eq!(input.as_array().unwrap().len(), 1);
        assert_eq!(input[0]["type"], "message");
        assert_eq!(input[0]["content"][0]["text"], "正文");
        eof().unwrap();
    }
}

#[test]
fn finished_items_survive_compact_incomplete_and_complete_tools_keep_tool_use() {
    for compact in [false, true] {
        let (decode, _) = decoder();
        let item = call("completed", "{\"path\":\"safe\"}");
        feed(
            &decode,
            json!({"type":"response.output_item.done","output_index":0,"item":item}),
        )
        .unwrap();
        let reasoning = json!({"id":"rs_a","type":"reasoning","status":"completed","summary":[],
            "encrypted_content":"finished"});
        feed(
            &decode,
            json!({"type":"response.output_item.done","output_index":1,"item":reasoning}),
        )
        .unwrap();
        let mut terminal = incomplete();
        if !compact {
            terminal["response"]["output"] = json!([item, reasoning]);
        }
        let events = feed(&decode, terminal).unwrap();
        let ModelStreamEvent::Completed(response) = events.last().unwrap() else {
            panic!("缺少 Completed")
        };
        assert_eq!(response.stop_reason(), &StopReason::ToolUse);
        let ModelMessage::Assistant {
            content,
            tool_calls,
        } = response.message()
        else {
            unreachable!()
        };
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id(), "call_a");
        assert!(content.iter().any(
            |block| matches!(block, ContentBlock::ProviderState { data, .. }
            if data["items"][1]["encrypted_content"] == "finished")
        ));
    }
}

#[test]
fn discarding_partial_items_does_not_hide_duplicate_calls_or_failed_status() {
    let (decode, _) = decoder();
    feed(
        &decode,
        json!({"type":"response.output_item.added","output_index":0,
        "item":call("in_progress", "{")}),
    )
    .unwrap();
    let mut second = call("completed", "{}");
    second["id"] = json!("fc_b");
    feed(
        &decode,
        json!({"type":"response.output_item.done","output_index":1,"item":second}),
    )
    .unwrap();
    assert!(feed(&decode, incomplete())
        .unwrap_err()
        .is_terminal_provider_failure());

    let (decode, _) = decoder();
    feed(
        &decode,
        json!({"type":"response.output_item.added","output_index":0,
        "item":call("failed", "{")}),
    )
    .unwrap();
    assert!(feed(&decode, incomplete())
        .unwrap_err()
        .is_terminal_provider_failure());
}

#[test]
fn incomplete_does_not_relax_identity_status_or_completed_argument_validation() {
    for output in [
        json!([call("completed", "{")]),
        json!([call("completed", "[]")]),
        json!([{"type":"computer_call","id":"hosted"}]),
        json!([call("failed", "{}")]),
    ] {
        let (decode, _) = decoder();
        let mut terminal = incomplete();
        terminal["response"]["output"] = output;
        assert!(feed(&decode, terminal)
            .unwrap_err()
            .is_terminal_provider_failure());
    }
    let (decode, _) = decoder();
    feed(
        &decode,
        json!({"type":"response.created","response":{"id":"another"}}),
    )
    .unwrap();
    assert!(feed(&decode, incomplete()).is_err());
    let (decode, _) = decoder();
    let mut terminal = incomplete();
    terminal["response"]["status"] = json!("completed");
    assert!(feed(&decode, terminal).is_err());
    let (decode, _) = decoder();
    feed(&decode, json!({"type":"response.output_item.added","output_index":0,"item":call("in_progress", "{")})).unwrap();
    let mut terminal = incomplete();
    terminal["response"]["output"] = json!([call("incomplete", "[]")]);
    assert!(
        feed(&decode, terminal).is_err(),
        "快照不能改写已发出的参数前缀"
    );
}
