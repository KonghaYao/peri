use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{
    transport::{HttpRequest, HttpResponse, HttpTransport, SseEvent},
    Model, ModelError, ModelMessage, ModelRequest, ModelResult, ModelStreamEvent, ToolResult,
};

use super::{
    request::build_request,
    request_test::{output_items, tool},
    ResponsesConfig, ResponsesModel,
};

fn config() -> ResponsesConfig {
    ResponsesConfig::new(
        Url::parse("https://api.openai.com/v1").unwrap(),
        "test-secret",
        "test-model",
    )
    .with_credential_ref("test-account")
    .with_runtime(
        crate::ModelRuntimeConfig::default()
            .with_retry(crate::RetryConfig::default().with_max_attempts(1)),
    )
}

fn completed(items: Vec<Value>) -> Value {
    json!({"type":"response.completed","response":{"id":"resp_a","status":"completed","output":items,"usage":{"input_tokens":30,"output_tokens":8,"input_tokens_details":{"cached_tokens":12}}}})
}

fn decoder() -> crate::runtime::stream::SseDecoders {
    let built =
        build_request(&config(), &ModelRequest::default().with_tools(vec![tool()])).unwrap();
    super::stream::decoders(built.binding, built.tool_names)()
}

fn feed(
    decode: &crate::runtime::stream::SseDecoder,
    event: Value,
) -> ModelResult<Vec<ModelStreamEvent>> {
    decode(
        SseEvent {
            event: event["type"].as_str().map(str::to_owned),
            data: event.to_string(),
        },
        Some("request_a".into()),
    )
}

#[test]
fn delta_done_and_completed_snapshots_are_not_accumulated_twice() {
    let (decode, _) = decoder();
    let mut events = Vec::new();
    for event in [
        json!({"type":"response.output_item.added","output_index":0,"item":{"id":"msg_a","type":"message","role":"assistant","content":[]}}),
        json!({"type":"response.output_text.delta","output_index":0,"item_id":"msg_a","content_index":0,"delta":"你好"}),
        json!({"type":"response.output_text.done","output_index":0,"item_id":"msg_a","content_index":0,"text":"你好"}),
        json!({"type":"response.output_item.done","output_index":0,"item":{"id":"msg_a","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"你好"}]}}),
        completed(vec![
            json!({"id":"msg_a","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"你好"}]}),
        ]),
    ] {
        events.extend(feed(&decode, event).unwrap());
    }
    let text: String = events
        .iter()
        .filter_map(|event| match event {
            ModelStreamEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "你好");
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ModelStreamEvent::Completed(_)))
            .count(),
        1
    );
    let ModelStreamEvent::Completed(response) = events.last().unwrap() else {
        panic!("completion expected")
    };
    assert_eq!(response.usage().unwrap().cache_read_input_tokens, Some(12));
    assert_eq!(response.request_id(), Some("request_a"));
}

#[test]
fn metadata_does_not_interrupt_stream_or_change_response_identity() {
    let (decode, complete) = decoder();
    feed(
        &decode,
        json!({"type":"response.created","response":{"id":"resp_a"}}),
    )
    .unwrap();
    assert!(feed(
        &decode,
        json!({"type":"response.metadata","metadata":{"service_tier":"default"}})
    )
    .unwrap()
    .is_empty());
    assert!(feed(
        &decode,
        json!({"type":"response.metadata","response":{"id":"resp_a"},"metadata":{}})
    )
    .unwrap()
    .is_empty());
    assert!(feed(&decode, completed(Vec::new()))
        .unwrap()
        .iter()
        .any(|event| matches!(event, ModelStreamEvent::Completed(_))));
    complete().unwrap();

    let (decode, _) = decoder();
    feed(
        &decode,
        json!({"type":"response.created","response":{"id":"resp_a"}}),
    )
    .unwrap();
    assert!(feed(
        &decode,
        json!({"type":"response.metadata","response":{"id":"other"}})
    )
    .is_err());
}

#[test]
fn compact_terminal_reconstructs_finished_items_without_output_or_status() {
    let (decode, eof) = decoder();
    let item = json!({"id":"msg_compact","type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]});
    let events = feed(
        &decode,
        json!({"type":"response.output_item.done","item":item}),
    )
    .unwrap();
    assert!(events
        .iter()
        .any(|event| matches!(event, ModelStreamEvent::TextDelta { text } if text == "done")));
    let events = feed(
        &decode,
        json!({"type":"response.completed","response":{"id":"resp_a"}}),
    )
    .unwrap();
    let Some(ModelStreamEvent::Completed(result)) = events.last() else {
        panic!("completion expected")
    };
    assert_eq!(result.assistant_text().as_deref(), Some("done"));
    eof().unwrap();
}

#[test]
fn done_only_tool_call_retains_its_identity_and_replay_without_final_output() {
    let (decode, eof) = decoder();
    let item = json!({"id":"fc_compact","type":"function_call","name":"read_file","call_id":"call_compact","arguments":"{}"});
    let events = feed(
        &decode,
        json!({"type":"response.output_item.done","item":item}),
    )
    .unwrap();
    assert!(events.iter().any(|event| matches!(event, ModelStreamEvent::ToolCallDelta { id: Some(id), name: Some(name), .. } if id == "call_compact" && name == "read_file")));
    let events = feed(
        &decode,
        json!({"type":"response.completed","response":{"id":"resp_a"}}),
    )
    .unwrap();
    let Some(ModelStreamEvent::Completed(response)) = events.last() else {
        panic!("completion expected")
    };
    let ModelMessage::Assistant { tool_calls, .. } = response.message() else {
        panic!("assistant expected")
    };
    assert_eq!(tool_calls[0].id(), "call_compact");
    eof().unwrap();
}

#[test]
fn done_only_message_without_item_id_keeps_native_replay() {
    let (decode, eof) = decoder();
    let item = json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello"}]});
    feed(
        &decode,
        json!({"type":"response.output_item.done","item":item}),
    )
    .unwrap();
    let events = feed(
        &decode,
        json!({"type":"response.completed","response":{"id":"resp_a"}}),
    )
    .unwrap();
    let Some(ModelStreamEvent::Completed(response)) = events.last() else {
        panic!("completion expected")
    };
    assert_eq!(response.assistant_text().as_deref(), Some("hello"));
    let ModelMessage::Assistant { content, .. } = response.message() else {
        panic!("assistant expected")
    };
    assert!(content.iter().any(|block| matches!(block, crate::ContentBlock::ProviderState { data, .. } if data["items"][0].get("id").is_none())));
    eof().unwrap();
}

#[test]
fn compact_terminal_refuses_unfinished_items_and_accepts_empty_output() {
    let (decode, _) = decoder();
    feed(&decode, json!({"type":"response.output_item.added","output_index":0,"item":{"id":"msg","type":"message","role":"assistant","content":[]}})).unwrap();
    assert!(feed(
        &decode,
        json!({"type":"response.completed","response":{"id":"resp_a"}})
    )
    .is_err());
    let (decode, eof) = decoder();
    assert!(feed(
        &decode,
        json!({"type":"response.completed","response":{"id":"resp_a"}})
    )
    .is_ok());
    eof().unwrap();
}

#[test]
fn partial_turn_is_not_projected_as_final_answer() {
    let (decode, _) = decoder();
    let events = feed(
        &decode,
        json!({"type":"response.completed","response":{"id":"resp_a","end_turn":false}}),
    )
    .unwrap();
    let Some(ModelStreamEvent::Completed(result)) = events.last() else {
        panic!("completion expected")
    };
    assert_eq!(result.stop_reason(), &crate::StopReason::Continue);
}

#[test]
fn metadata_extensions_do_not_interrupt_but_unknown_output_does() {
    let (decode, _) = decoder();
    assert!(feed(
        &decode,
        json!({"type":"codex.response.metadata","metadata":{"tier":"default"}})
    )
    .unwrap()
    .is_empty());
    assert!(feed(
        &decode,
        json!({"type":"response.queue_metadata","metadata":{}})
    )
    .unwrap()
    .is_empty());
    assert!(feed(
        &decode,
        json!({"type":"response.output_tool.delta","delta":"{}"})
    )
    .is_err());
    assert!(feed(
        &decode,
        json!({"type":"response.unknown_event","item":{"type":"function_call"}})
    )
    .is_err());
}

#[test]
fn parallel_calls_keep_item_ids_separate_from_call_ids_and_done_can_supply_arguments() {
    let (decode, _) = decoder();
    let call = |id: &str, call_id: &str, arguments: &str| json!({"id":id,"type":"function_call","name":"read_file","call_id":call_id,"arguments":arguments});
    let mut events = Vec::new();
    for event in [
        json!({"type":"response.output_item.added","output_index":1,"item":call("fc_b","call_b","")}),
        json!({"type":"response.output_item.added","output_index":0,"item":call("fc_a","call_a","")}),
        json!({"type":"response.function_call_arguments.delta","output_index":1,"item_id":"fc_b","delta":"{\"path\":"}),
        json!({"type":"response.function_call_arguments.delta","output_index":1,"item_id":"fc_b","delta":"\"b\"}"}),
        json!({"type":"response.function_call_arguments.done","output_index":0,"item_id":"fc_a","arguments":"{\"path\":\"a\"}"}),
        completed(vec![
            call("fc_a", "call_a", "{\"path\":\"a\"}"),
            call("fc_b", "call_b", "{\"path\":\"b\"}"),
        ]),
    ] {
        events.extend(feed(&decode, event).unwrap());
    }
    let ModelStreamEvent::Completed(response) = events.last().unwrap() else {
        panic!("completion expected")
    };
    let ModelMessage::Assistant { tool_calls, .. } = response.message() else {
        unreachable!()
    };
    assert_eq!(
        tool_calls.iter().map(|call| call.id()).collect::<Vec<_>>(),
        ["call_a", "call_b"]
    );
    assert_eq!(tool_calls[0].arguments().as_map()["path"], "a");
    let args: String = events
        .iter()
        .filter_map(|event| match event {
            ModelStreamEvent::ToolCallDelta {
                index: 1,
                arguments_delta,
                ..
            } => Some(arguments_delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(args, "{\"path\":\"b\"}");
}

#[test]
fn terminal_failures_never_complete_and_unsupported_output_fails_closed() {
    for event in [
        json!({"type":"response.failed","response":{"error":{"code":"usage_limit_reached"}}}),
        json!({"type":"response.incomplete","response":{"id":"resp_a","incomplete_details":{"reason":"content_filter"}}}),
        completed(vec![json!({"type":"computer_call","id":"cc_a"})]),
    ] {
        let (decode, _) = decoder();
        let error = feed(&decode, event).unwrap_err();
        assert!(error.is_terminal_provider_failure());
    }
    let (_, eof) = decoder();
    assert!(eof().is_err());
}

#[test]
fn cache_write_usage_is_a_subset_of_input_and_counters_cannot_overflow() {
    let usage = |input: u64, cached: u64, written: u64| json!({"input_tokens":input,"output_tokens":7,"input_tokens_details":{"cached_tokens":cached,"cache_write_tokens":written}});
    for (counts, expected) in [
        (usage(100, 40, 60), (100, 40, 60)),
        (
            usage(u32::MAX as u64, u32::MAX as u64, 0),
            (u32::MAX, u32::MAX, 0),
        ),
    ] {
        let (decode, _) = decoder();
        let mut terminal = completed(Vec::new());
        terminal["response"]["usage"] = counts;
        let events = feed(&decode, terminal).unwrap();
        let Some(ModelStreamEvent::Completed(response)) = events.last() else {
            panic!("completion expected")
        };
        let usage = response.usage().unwrap();
        assert_eq!(usage.input_tokens, expected.0);
        assert_eq!(usage.cache_read_input_tokens, Some(expected.1));
        assert_eq!(usage.cache_creation_input_tokens, Some(expected.2));
    }
    for counts in [
        usage(100, 41, 60),
        usage(u32::MAX as u64, u32::MAX as u64, 1),
        usage(u32::MAX as u64 + 1, 0, 0),
        usage(100, 0, u32::MAX as u64 + 1),
    ] {
        let (decode, _) = decoder();
        let mut terminal = completed(Vec::new());
        terminal["response"]["usage"] = counts;
        assert!(feed(&decode, terminal)
            .unwrap_err()
            .is_terminal_provider_failure());
    }
}

#[test]
fn rejects_malformed_json_wrong_ids_changed_snapshot_and_unoffered_tool() {
    let (decode, _) = decoder();
    assert!(decode(
        SseEvent {
            event: None,
            data: "{broken".into()
        },
        None
    )
    .is_err());
    let added = json!({"type":"response.output_item.added","output_index":0,"item":{"type":"message","id":"m","role":"assistant","content":[]}});
    let (decode, _) = decoder();
    feed(&decode, added.clone()).unwrap();
    assert!(feed(&decode, json!({"type":"response.output_text.delta","output_index":0,"item_id":"other","content_index":0,"delta":"x"})).is_err());
    let (decode, _) = decoder();
    feed(&decode, added).unwrap();
    feed(&decode, json!({"type":"response.output_text.delta","output_index":0,"item_id":"m","content_index":0,"delta":"abc"})).unwrap();
    assert!(feed(&decode, completed(vec![json!({"type":"message","id":"m","role":"assistant","content":[{"type":"output_text","text":"different"}]})])).is_err());
    let (decode, _) = decoder();
    assert!(feed(
        &decode,
        completed(vec![
            json!({"type":"function_call","id":"f","call_id":"c","name":"unknown","arguments":"{}"})
        ])
    )
    .is_err());
}

struct QueueTransport {
    responses: Mutex<VecDeque<Vec<Vec<u8>>>>,
    requests: Mutex<Vec<Value>>,
}

#[async_trait]
impl HttpTransport for QueueTransport {
    async fn send(
        &self,
        request: HttpRequest,
        cancellation: CancellationToken,
    ) -> ModelResult<HttpResponse> {
        assert_eq!(
            request.request.headers()[reqwest::header::AUTHORIZATION],
            "Bearer test-secret"
        );
        let body: Value =
            serde_json::from_slice(request.request.body().unwrap().as_bytes().unwrap()).unwrap();
        self.requests.lock().unwrap().push(body);
        let chunks = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(ModelError::cancelled)?;
        Ok(HttpResponse::new(
            200,
            Some("request_http".into()),
            Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))),
            cancellation,
        ))
    }
}

fn fragmented(event: Value) -> Vec<Vec<u8>> {
    // One byte per HTTP chunk deliberately splits multibyte UTF-8 and SSE lines.
    format!(
        "event: {}\ndata: {event}\n\n",
        event["type"].as_str().unwrap()
    )
    .bytes()
    .map(|byte| vec![byte])
    .collect()
}

#[tokio::test]
async fn two_turn_tool_roundtrip_reuses_transport_and_exact_ordered_native_items() {
    let items = output_items();
    let final_item = json!({"id":"msg_final","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"完成"}]});
    let transport = Arc::new(QueueTransport {
        responses: Mutex::new(VecDeque::from([
            fragmented(completed(items.clone())),
            fragmented(completed(vec![final_item])),
        ])),
        requests: Mutex::new(Vec::new()),
    });
    let model = ResponsesModel::with_transport(config(), transport.clone());
    let mut request =
        ModelRequest::new(vec![ModelMessage::user_text("read a.txt")]).with_tools(vec![tool()]);
    let first = model
        .complete(request.clone(), CancellationToken::new())
        .await
        .unwrap();
    request.messages.push(first.message().clone());
    request
        .messages
        .push(ModelMessage::tool_result(ToolResult::success(
            "call_a",
            "read_file",
            "contents",
        )));
    let second = model
        .complete(request, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(second.assistant_text().as_deref(), Some("完成"));
    let requests = transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(&requests[1]["input"].as_array().unwrap()[1..4], &items);
    assert_eq!(requests[1]["input"][4]["call_id"], "call_a");
    assert!(requests[1].get("previous_response_id").is_none());
}

#[tokio::test]
async fn prepared_observation_checkpoint_and_execution_share_the_frozen_body() {
    let transport = Arc::new(QueueTransport {
        responses: Mutex::new(VecDeque::from([fragmented(completed(Vec::new()))])),
        requests: Mutex::new(Vec::new()),
    });
    let model = ResponsesModel::with_transport(config(), transport.clone());
    let request = ModelRequest::new(vec![
        ModelMessage::system_text("developer"),
        ModelMessage::user_text("hello"),
    ])
    .with_tools(vec![tool()]);
    let observation = model.prepare_request(&request).unwrap();
    let call = model.prepare_stream(request).unwrap();
    let checkpoint = call.checkpoint().clone();
    assert_eq!(checkpoint["body"], *observation.body().as_value());
    assert!(!checkpoint.to_string().contains("test-secret"));
    assert_eq!(transport.requests.lock().unwrap().len(), 0);
    let events: Vec<_> = call
        .start(CancellationToken::new())
        .unwrap()
        .collect()
        .await;
    assert!(matches!(
        events.last(),
        Some(Ok(ModelStreamEvent::Completed(_)))
    ));
    assert_eq!(checkpoint["body"], transport.requests.lock().unwrap()[0]);
}

#[test]
fn reasoning_summary_stream_and_encrypted_item_survive_completion() {
    let (decode, _) = decoder();
    let mut events = Vec::new();
    for event in [
        json!({"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"r","summary":[]}}),
        json!({"type":"response.reasoning_summary_text.delta","output_index":0,"item_id":"r","summary_index":0,"delta":"inspect"}),
        completed(vec![
            json!({"type":"reasoning","id":"r","summary":[{"type":"summary_text","text":"inspect file"}],"encrypted_content":"cipher"}),
        ]),
    ] {
        events.extend(feed(&decode, event).unwrap());
    }
    let summary: String = events
        .iter()
        .filter_map(|event| match event {
            ModelStreamEvent::ReasoningDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(summary, "inspect file");
    let Some(ModelStreamEvent::Completed(result)) = events.last() else {
        panic!("completed expected")
    };
    let ModelMessage::Assistant { content, .. } = result.message() else {
        unreachable!()
    };
    assert!(content.iter().any(|block| matches!(block, crate::ContentBlock::ProviderState {data,..} if data["items"][0]["encrypted_content"]=="cipher")));
}

#[test]
fn completed_compacted_output_recovers_only_finished_stream_items() {
    let (decode, eof) = decoder();
    let reasoning = json!({"type":"reasoning","id":"reason_0","status":"completed","summary":[{"type":"summary_text","text":"checked"}],"encrypted_content":"cipher"});
    let message = json!({"type":"message","id":"message_1","role":"assistant","status":"completed","content":[{"type":"output_text","text":"answer"}]});
    for event in [
        json!({"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"reason_0","summary":[]}}),
        json!({"type":"response.reasoning_summary_text.delta","output_index":0,"item_id":"reason_0","summary_index":0,"delta":"checked"}),
        json!({"type":"response.output_item.done","output_index":0,"item":reasoning}),
        json!({"type":"response.output_item.added","output_index":1,"item":{"type":"message","id":"message_1","role":"assistant","content":[]}}),
        json!({"type":"response.output_text.delta","output_index":1,"item_id":"message_1","content_index":0,"delta":"answer"}),
        json!({"type":"response.output_item.done","output_index":1,"item":message.clone()}),
    ] {
        feed(&decode, event).unwrap();
    }
    let events = feed(&decode, completed(vec![message])).unwrap();
    assert!(!events.iter().any(|event| matches!(
        event,
        ModelStreamEvent::TextDelta { .. } | ModelStreamEvent::ReasoningDelta { .. }
    )));
    let Some(ModelStreamEvent::Completed(result)) = events.last() else {
        panic!("completion expected");
    };
    let ModelMessage::Assistant { content, .. } = result.message() else {
        unreachable!();
    };
    assert!(content.iter().any(|block| matches!(block, crate::ContentBlock::ProviderState { data, .. } if data["items"].as_array().is_some_and(|items| items.len() == 2) && data["items"][0]["encrypted_content"] == "cipher")));
    eof().unwrap();
}

#[test]
fn completed_cannot_omit_unfinished_emitted_output() {
    let (decode, _) = decoder();
    feed(&decode, json!({"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"reason_0","summary":[]}})).unwrap();
    feed(&decode, json!({"type":"response.output_item.added","output_index":1,"item":{"type":"message","id":"message_1","role":"assistant","content":[]}})).unwrap();
    let message = json!({"type":"message","id":"message_1","role":"assistant","content":[{"type":"output_text","text":"answer"}]});
    let error = feed(&decode, completed(vec![message])).unwrap_err();
    assert!(error.to_string().contains("unfinished output item"));
}

#[test]
fn compacted_completion_preserves_finished_tool_call() {
    let (decode, _) = decoder();
    let call = json!({"type":"function_call","id":"call_item","call_id":"call_id","name":"read_file","arguments":"{\"path\":\"doc\"}"});
    for event in [
        json!({"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","id":"call_item","call_id":"call_id","name":"read_file","arguments":""}}),
        json!({"type":"response.output_item.done","output_index":0,"item":call}),
        json!({"type":"response.output_item.added","output_index":1,"item":{"type":"message","id":"message","role":"assistant","content":[]}}),
        json!({"type":"response.output_item.done","output_index":1,"item":{"type":"message","id":"message","role":"assistant","content":[{"type":"output_text","text":"Reading"}]}}),
    ] {
        feed(&decode, event).unwrap();
    }
    let events = feed(&decode, completed(vec![json!({"type":"message","id":"message","role":"assistant","content":[{"type":"output_text","text":"Reading"}]})])).unwrap();
    let Some(ModelStreamEvent::Completed(result)) = events.last() else {
        panic!("completion expected");
    };
    let ModelMessage::Assistant { tool_calls, .. } = result.message() else {
        unreachable!();
    };
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0].id(), "call_id");
}

#[tokio::test]
async fn compacted_completion_finishes_public_stream_without_protocol_failure() {
    let reasoning = json!({"type":"reasoning","id":"reason","status":"completed","summary":[{"type":"summary_text","text":"inspect"}]});
    let message = json!({"type":"message","id":"answer","role":"assistant","status":"completed","content":[{"type":"output_text","text":"done"}]});
    let chunks = [
        json!({"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"reason","summary":[]}}),
        json!({"type":"response.output_item.done","output_index":0,"item":reasoning}),
        json!({"type":"response.output_item.added","output_index":1,"item":{"type":"message","id":"answer","role":"assistant","content":[]}}),
        json!({"type":"response.output_item.done","output_index":1,"item":message.clone()}),
        completed(vec![message]),
    ].into_iter().flat_map(fragmented).collect();
    let transport = Arc::new(QueueTransport {
        responses: Mutex::new(VecDeque::from([chunks])),
        requests: Mutex::new(Vec::new()),
    });
    let model = ResponsesModel::with_transport(config(), transport);
    let events: Vec<_> = model
        .stream(ModelRequest::default(), CancellationToken::new())
        .await
        .unwrap()
        .collect()
        .await;
    assert!(
        events.iter().all(Result::is_ok),
        "stream should not fail: {events:?}"
    );
    assert!(events
        .iter()
        .any(|event| matches!(event, Ok(ModelStreamEvent::Completed(_)))));
}

#[tokio::test]
async fn done_without_completed_and_cancelled_request_cannot_execute_tools() {
    let transport = Arc::new(QueueTransport {
        responses: Mutex::new(VecDeque::from([vec![b"data: [DONE]\n\n".to_vec()]])),
        requests: Mutex::new(Vec::new()),
    });
    let model = ResponsesModel::with_transport(config(), transport.clone());
    assert!(model
        .complete(ModelRequest::default(), CancellationToken::new())
        .await
        .is_err());
    let token = CancellationToken::new();
    token.cancel();
    assert!(model.stream(ModelRequest::default(), token).await.is_err());
}

#[tokio::test]
async fn visible_failure_returns_terminal_interruption_without_completed() {
    let added = json!({"type":"response.output_item.added","output_index":0,"item":{"id":"msg_a","type":"message","role":"assistant","content":[]}});
    let delta = json!({"type":"response.output_text.delta","output_index":0,"item_id":"msg_a","content_index":0,"delta":"partial"});
    let failed =
        json!({"type":"response.failed","response":{"error":{"code":"usage_limit_reached"}}});
    let chunks: Vec<_> = [added, delta, failed]
        .into_iter()
        .flat_map(fragmented)
        .collect();
    let transport = Arc::new(QueueTransport {
        responses: Mutex::new(VecDeque::from([chunks])),
        requests: Mutex::new(Vec::new()),
    });
    let model = ResponsesModel::with_transport(config(), transport.clone());
    let events: Vec<_> = model
        .stream(ModelRequest::default(), CancellationToken::new())
        .await
        .unwrap()
        .collect()
        .await;
    assert!(events
        .iter()
        .any(|event| matches!(event, Ok(ModelStreamEvent::TextDelta { .. }))));
    assert!(!events
        .iter()
        .any(|event| matches!(event, Ok(ModelStreamEvent::Completed(_)))));
    let Some(Ok(ModelStreamEvent::Interrupted { error, .. })) = events.last() else {
        panic!("terminal interruption expected: {events:?}")
    };
    assert!(error.is_terminal_provider_failure());
    assert_eq!(transport.requests.lock().unwrap().len(), 1);
}

#[test]
fn absent_namespace_and_explicit_null_are_the_same_identity() {
    let (decode, _) = decoder();
    let mut item =
        json!({"id":"f","type":"function_call","call_id":"c","name":"read_file","arguments":""});
    feed(
        &decode,
        json!({"type":"response.output_item.added","output_index":0,"item":item}),
    )
    .unwrap();
    item["namespace"] = Value::Null;
    item["arguments"] = json!("{}");
    let events = feed(&decode, completed(vec![item])).unwrap();
    assert!(matches!(
        events.last(),
        Some(ModelStreamEvent::Completed(_))
    ));
}

#[test]
fn plan_function_namespace_maps_to_original_tool_name_and_is_preserved_for_replay() {
    let config = config().with_profile(super::ResponsesProfile::ChatGptPlan);
    let built = build_request(&config, &ModelRequest::default().with_tools(vec![tool()])).unwrap();
    let factory = super::stream::decoders(built.binding, built.tool_names);
    let item = json!({"id":"fc_plan","type":"function_call","call_id":"call_plan","name":"read_file","namespace":"peri","arguments":"{}"});
    let (decode, _) = factory();
    let events = feed(&decode, completed(vec![item.clone()])).unwrap();
    let Some(ModelStreamEvent::Completed(response)) = events.last() else {
        panic!("completion expected")
    };
    let ModelMessage::Assistant { tool_calls, .. } = response.message() else {
        unreachable!()
    };
    assert_eq!(tool_calls[0].name(), "read_file");
    assert_eq!(tool_calls[0].id(), "call_plan");
    let replayed = build_request(
        &config,
        &ModelRequest::new(vec![response.message().clone()]),
    )
    .unwrap();
    assert_eq!(replayed.body["input"][0], item);
    for namespace in [Value::Null, json!("other")] {
        let (decode, _) = factory();
        let mut invalid = item.clone();
        invalid["namespace"] = namespace;
        assert!(feed(&decode, completed(vec![invalid]))
            .unwrap_err()
            .is_terminal_provider_failure());
    }
}

struct PendingTransport {
    dropped: Arc<std::sync::atomic::AtomicBool>,
}

struct PendingBody {
    dropped: Arc<std::sync::atomic::AtomicBool>,
    initial: Option<Vec<u8>>,
}

impl futures::Stream for PendingBody {
    type Item = ModelResult<Vec<u8>>;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        match self.initial.take() {
            Some(bytes) => std::task::Poll::Ready(Some(Ok(bytes))),
            None => std::task::Poll::Pending,
        }
    }
}

impl Drop for PendingBody {
    fn drop(&mut self) {
        self.dropped
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[async_trait]
impl HttpTransport for PendingTransport {
    async fn send(
        &self,
        _request: HttpRequest,
        cancellation: CancellationToken,
    ) -> ModelResult<HttpResponse> {
        let added = json!({"type":"response.output_item.added","output_index":0,"item":{"id":"m","type":"message","role":"assistant","content":[{"type":"output_text","text":"started"}]}});
        Ok(HttpResponse::new(
            200,
            None,
            Box::pin(PendingBody {
                dropped: self.dropped.clone(),
                initial: Some(format!("data: {added}\n\n").into_bytes()),
            }),
            cancellation,
        ))
    }
}

#[tokio::test]
async fn aborting_an_inflight_stream_drops_the_pending_network_body() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let dropped = Arc::new(AtomicBool::new(false));
    let model = ResponsesModel::with_transport(
        config(),
        Arc::new(PendingTransport {
            dropped: dropped.clone(),
        }),
    );
    let parent = CancellationToken::new();
    let mut stream = model
        .stream(ModelRequest::default(), parent.clone())
        .await
        .unwrap();
    assert!(matches!(
        stream.next().await,
        Some(Ok(ModelStreamEvent::TextDelta { .. }))
    ));
    stream.abort();
    assert!(stream.next().await.unwrap().is_err());
    assert!(!parent.is_cancelled());
    drop(stream);
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while !dropped.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancelled response body must be dropped");
}
