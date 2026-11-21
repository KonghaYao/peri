use axum::http::{HeaderMap, HeaderValue};
use serde_json::json;

use super::{resolve_submission, resolve_tools, CreateSessionBody, SubmitEventBody};
use crate::domain::{ErrorCode, ErrorKind, InputKind, InputMode};

fn body(value: serde_json::Value) -> CreateSessionBody {
    serde_json::from_value(value).expect("create body")
}

fn event_body(value: serde_json::Value) -> SubmitEventBody {
    serde_json::from_value(value).expect("event body")
}

fn idempotency_headers(key: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "idempotency-key",
        HeaderValue::from_str(key).expect("idempotency header"),
    );
    headers
}

fn official_envelope(content: serde_json::Value) -> serde_json::Value {
    json!({
        "events": [{
            "type": "agent.session.input.message",
            "input": [{ "role": "user", "content": content }]
        }]
    })
}

fn field(error: &crate::domain::SwarmError) -> String {
    error.details()["field"]
        .as_str()
        .expect("field path")
        .to_owned()
}

#[test]
fn official_mcp_shape_is_accepted_at_the_agent_position() {
    let tools = resolve_tools(&body(json!({
        "logicalId": "agent-01",
        "agent": {
            "tools": [{
                "type": "mcp",
                "server_label": "deepwiki",
                "server_url": "https://mcp.example.test/mcp",
                "headers": { "Authorization": "Bearer caller-token" },
                "require_approval": "never",
                "allowed_tools": ["read_wiki_structure"],
                "defer_loading": true
            }]
        }
    })))
    .expect("tools");

    assert_eq!(tools.len(), 1);
    let tool = &tools[0];
    assert_eq!(tool.server_label, "deepwiki");
    assert_eq!(tool.server_url, "https://mcp.example.test/mcp");
    assert_eq!(tool.headers.len(), 1);
    assert_eq!(tool.headers[0].name, "Authorization");
    assert_eq!(tool.headers[0].value, "Bearer caller-token");
    assert_eq!(tool.require_approval, Some(json!("never")));
    assert_eq!(tool.allowed_tools, Some(json!(["read_wiki_structure"])));
    assert_eq!(tool.extra["defer_loading"], json!(true));
}

#[test]
fn multiple_tools_and_array_headers_keep_every_declaration() {
    let tools = resolve_tools(&body(json!({
        "logicalId": "agent-01",
        "tools": [
            {
                "type": "mcp",
                "server_label": "first",
                "server_url": "http://127.0.0.1:9001/mcp",
                "headers": [
                    { "name": "X-B", "value": "2" },
                    { "name": "X-A", "value": "1" },
                    { "name": "X-B", "value": "3" }
                ]
            },
            {
                "type": "mcp",
                "server_label": "second",
                "server_url": "http://127.0.0.1:9002/mcp",
                "require_approval": { "always": { "tool_names": ["write"] } },
                "allowed_tools": { "tool_names": ["read"], "read_only": true }
            }
        ]
    })))
    .expect("tools");

    assert_eq!(tools.len(), 2);
    assert_eq!(
        tools[0]
            .headers
            .iter()
            .map(|header| (header.name.as_str(), header.value.as_str()))
            .collect::<Vec<_>>(),
        vec![("X-A", "1"), ("X-B", "2"), ("X-B", "3")],
        "数组形式按名称稳定排序，重名保持相对顺序"
    );
    assert_eq!(
        tools[1].require_approval,
        Some(json!({ "always": { "tool_names": ["write"] } })),
        "对象形式的 require_approval 原样保留"
    );
    assert_eq!(
        tools[1].allowed_tools,
        Some(json!({ "tool_names": ["read"], "read_only": true })),
        "allowed_tools 过滤对象原样保留"
    );
}

#[test]
fn object_and_array_headers_normalize_to_the_same_declaration() {
    let object_form = resolve_tools(&body(json!({
        "logicalId": "agent-01",
        "tools": [{
            "type": "mcp",
            "server_label": "same",
            "server_url": "https://mcp.example.test/mcp",
            "headers": { "X-B": "2", "X-A": "1" }
        }]
    })))
    .expect("object headers");
    let array_form = resolve_tools(&body(json!({
        "logicalId": "agent-01",
        "tools": [{
            "type": "mcp",
            "server_label": "same",
            "server_url": "https://mcp.example.test/mcp",
            "headers": [
                { "name": "X-A", "value": "1" },
                { "name": "X-B", "value": "2" }
            ]
        }]
    })))
    .expect("array headers");

    assert_eq!(object_form, array_form);
}

#[test]
fn flat_and_nested_tools_merge_and_disagree_visibly() {
    let declaration = json!([{
        "type": "mcp",
        "server_label": "flat",
        "server_url": "https://mcp.example.test/mcp"
    }]);
    assert_eq!(
        resolve_tools(&body(json!({
            "logicalId": "agent-01",
            "tools": declaration.clone(),
            "agent": { "tools": declaration.clone() }
        })))
        .expect("same declaration"),
        resolve_tools(&body(
            json!({ "logicalId": "agent-01", "tools": declaration })
        ))
        .expect("flat declaration")
    );

    let error = resolve_tools(&body(json!({
        "logicalId": "agent-01",
        "tools": [{
            "type": "mcp",
            "server_label": "flat",
            "server_url": "https://mcp.example.test/mcp"
        }],
        "agent": {
            "tools": [{
                "type": "mcp",
                "server_label": "nested",
                "server_url": "https://mcp.example.test/mcp"
            }]
        }
    })))
    .expect_err("disagreement");
    assert_eq!(error.code(), ErrorCode::InvalidInput);
    assert_eq!(field(&error), "tools");
}

#[test]
fn a_create_without_tools_declares_nothing() {
    assert!(resolve_tools(&body(json!({ "logicalId": "agent-01" })))
        .unwrap()
        .is_empty());
    assert!(resolve_tools(&body(json!({
        "logicalId": "agent-01",
        "tools": [],
        "agent": { "tools": [] }
    })))
    .unwrap()
    .is_empty());
    assert!(resolve_tools(&body(json!({
        "logicalId": "agent-01",
        "tools": null
    })))
    .unwrap()
    .is_empty());
}

#[test]
fn tools_must_be_an_array_at_either_position() {
    let error = resolve_tools(&body(json!({
        "logicalId": "agent-01",
        "tools": { "type": "mcp" }
    })))
    .expect_err("non-array tools");
    assert_eq!(error.code(), ErrorCode::InvalidInput);
    assert_eq!(field(&error), "tools");

    let error = resolve_tools(&body(json!({
        "logicalId": "agent-01",
        "agent": { "tools": "mcp" }
    })))
    .expect_err("non-array agent.tools");
    assert_eq!(error.code(), ErrorCode::InvalidInput);
    assert_eq!(field(&error), "agent.tools");
}

#[test]
fn malformed_tool_shapes_report_their_field_path() {
    for (entry, expected) in [
        (json!("not-an-object"), "agent.tools[0]"),
        (
            json!({ "server_label": "x", "server_url": "https://mcp.test" }),
            "agent.tools[0].type",
        ),
        (
            json!({ "type": 7, "server_label": "x", "server_url": "https://mcp.test" }),
            "agent.tools[0].type",
        ),
        (
            json!({ "type": "mcp", "server_url": "https://mcp.test" }),
            "agent.tools[0].server_label",
        ),
        (
            json!({ "type": "mcp", "server_label": "x" }),
            "agent.tools[0].server_url",
        ),
        (
            json!({
                "type": "mcp",
                "server_label": "x",
                "server_url": "https://mcp.test",
                "headers": "Authorization: Bearer x"
            }),
            "agent.tools[0].headers",
        ),
        (
            json!({
                "type": "mcp",
                "server_label": "x",
                "server_url": "https://mcp.test",
                "headers": [{ "name": "X" }]
            }),
            "agent.tools[0].headers[0].value",
        ),
        (
            json!({
                "type": "mcp",
                "server_label": "x",
                "server_url": "https://mcp.test",
                "require_approval": 3
            }),
            "agent.tools[0].require_approval",
        ),
        (
            json!({
                "type": "mcp",
                "server_label": "x",
                "server_url": "https://mcp.test",
                "allowed_tools": "read"
            }),
            "agent.tools[0].allowed_tools",
        ),
    ] {
        let error = resolve_tools(&body(json!({
            "logicalId": "agent-01",
            "agent": { "tools": [entry] }
        })))
        .expect_err("malformed tool");
        assert_eq!(error.code(), ErrorCode::InvalidInput, "{error:?}");
        assert_eq!(field(&error), expected);
    }
}

#[test]
fn a_non_mcp_tool_is_reported_as_an_implementation_gap() {
    let error = resolve_tools(&body(json!({
        "logicalId": "agent-01",
        "tools": [{ "type": "function", "name": "run" }]
    })))
    .expect_err("non-mcp tool");

    assert_eq!(error.code(), ErrorCode::UnsupportedTool);
    assert_eq!(error.kind(), ErrorKind::Unprocessable);
    assert_eq!(field(&error), "tools[0].type");
    assert_eq!(error.details()["received"], json!("function"));
    assert_eq!(error.details()["supported"], json!(["mcp"]));
}

#[test]
fn unknown_official_fields_are_preserved_not_rejected() {
    let tools = resolve_tools(&body(json!({
        "logicalId": "agent-01",
        "tools": [{
            "type": "mcp",
            "server_label": "deepwiki",
            "server_url": "https://mcp.example.test/mcp",
            "server_description": "wiki",
            "allowed_tools": ["read"],
            "require_approval": "always"
        }]
    })))
    .expect("tools");
    assert_eq!(tools[0].extra["server_description"], json!("wiki"));
    assert_eq!(tools[0].require_approval, Some(json!("always")));
}

#[test]
fn official_events_envelope_maps_to_one_user_turn() {
    let submission = resolve_submission(
        event_body(official_envelope(json!([
            { "type": "input_text", "text": "hello" },
            { "type": "input_text", "text": "world" }
        ]))),
        &HeaderMap::new(),
    )
    .expect("official envelope");

    assert_eq!(submission.kind, InputKind::User);
    assert_eq!(submission.mode, InputMode::Turn);
    assert_eq!(submission.text.as_deref(), Some("hello\nworld"));
    assert!(submission.attachments.is_empty());
    assert!(submission.plugin_selections.is_empty());
    assert!(submission.original_draft.is_none());
    let generated = uuid::Uuid::parse_str(&submission.input_id).expect("generated UUID");
    assert_eq!(
        generated.get_version_num(),
        7,
        "the no-key carve-out generates a UUIDv7"
    );
}

#[test]
fn official_events_envelope_reuses_the_idempotency_key() {
    let input_id = uuid::Uuid::now_v7().to_string();
    let submission = resolve_submission(
        event_body(official_envelope(json!([
            { "type": "input_text", "text": "via header" }
        ]))),
        &idempotency_headers(&input_id),
    )
    .expect("official envelope with key");

    assert_eq!(submission.input_id, input_id);
    assert_eq!(submission.text.as_deref(), Some("via header"));
}

#[test]
fn official_events_envelope_rejects_a_non_uuid_idempotency_key() {
    let error = resolve_submission(
        event_body(official_envelope(json!([
            { "type": "input_text", "text": "hello" }
        ]))),
        &idempotency_headers("not-a-uuid"),
    )
    .expect_err("non-UUID key");

    assert_eq!(error.code(), ErrorCode::InvalidInput);
    assert_eq!(error.message(), "inputId must be a UUID");
}

#[test]
fn native_event_body_without_input_id_still_requires_one() {
    let error = resolve_submission(event_body(json!({ "text": "hello" })), &HeaderMap::new())
        .expect_err("native path must not generate inputId");

    assert_eq!(error.code(), ErrorCode::InvalidInput);
    assert!(error.message().contains("inputId is required"), "{error:?}");
}

#[test]
fn official_events_envelope_accepts_only_one_event() {
    let message_event = json!({
        "type": "agent.session.input.message",
        "input": [{ "role": "user", "content": [{ "type": "input_text", "text": "a" }] }]
    });

    let error = resolve_submission(
        event_body(json!({ "events": [message_event.clone(), message_event] })),
        &HeaderMap::new(),
    )
    .expect_err("multiple events");
    assert_eq!(error.code(), ErrorCode::InvalidInput);
    assert_eq!(field(&error), "events");
    assert!(error.message().contains("only one event"), "{error:?}");

    let error = resolve_submission(event_body(json!({ "events": [] })), &HeaderMap::new())
        .expect_err("empty events");
    assert_eq!(field(&error), "events");
}

#[test]
fn official_events_envelope_accepts_only_one_user_message() {
    let error = resolve_submission(
        event_body(json!({
            "events": [{
                "type": "agent.session.input.message",
                "input": [
                    { "role": "user", "content": [{ "type": "input_text", "text": "a" }] },
                    { "role": "user", "content": [{ "type": "input_text", "text": "b" }] }
                ]
            }]
        })),
        &HeaderMap::new(),
    )
    .expect_err("multiple messages");
    assert_eq!(error.code(), ErrorCode::InvalidInput);
    assert_eq!(field(&error), "events[0].input");

    let error = resolve_submission(
        event_body(json!({
            "events": [{
                "type": "agent.session.input.message",
                "input": []
            }]
        })),
        &HeaderMap::new(),
    )
    .expect_err("empty messages");
    assert_eq!(field(&error), "events[0].input");
}

#[test]
fn official_events_envelope_rejects_image_content_at_its_index() {
    let error = resolve_submission(
        event_body(official_envelope(json!([
            { "type": "input_text", "text": "before" },
            { "type": "input_image", "image_url": "https://example.invalid/x.png" }
        ]))),
        &HeaderMap::new(),
    )
    .expect_err("input_image is unsupported");

    assert_eq!(error.code(), ErrorCode::InvalidInput);
    assert_eq!(field(&error), "events[0].input[0].content[1]");
    assert!(error.message().contains("input_text"), "{error:?}");
}

#[test]
fn official_events_envelope_rejects_unknown_event_types_by_name() {
    for kind in [
        "agent.session.input.cancel",
        "mcp_approval_request_result",
        "agent.session.input.unknown",
    ] {
        let error = resolve_submission(
            event_body(json!({ "events": [{ "type": kind }] })),
            &HeaderMap::new(),
        )
        .expect_err("unsupported event type");
        assert_eq!(error.code(), ErrorCode::InvalidInput, "{kind}");
        assert_eq!(field(&error), "events[0].type", "{kind}");
        assert!(error.message().contains("agent.session.input.message"));
    }
}

#[test]
fn official_events_envelope_rejects_unknown_event_fields() {
    let error = resolve_submission(
        event_body(json!({
            "events": [{
                "type": "agent.session.input.message",
                "input": [],
                "unexpected": true
            }]
        })),
        &HeaderMap::new(),
    )
    .expect_err("unsupported event field");

    assert_eq!(error.code(), ErrorCode::InvalidInput);
    assert_eq!(field(&error), "events[0].unexpected");
}

#[test]
fn official_events_envelope_cannot_be_mixed_with_native_fields() {
    let envelope = official_envelope(json!([{ "type": "input_text", "text": "hello" }]));
    for (native, expected) in [
        (
            json!({ "inputId": uuid::Uuid::now_v7().to_string() }),
            "inputId",
        ),
        (json!({ "kind": "control" }), "kind"),
        (json!({ "mode": "steer" }), "mode"),
        (json!({ "text": "native" }), "text"),
        (json!({ "attachments": [] }), "attachments"),
        (json!({ "pluginSelections": [] }), "pluginSelections"),
        (json!({ "originalDraft": "draft" }), "originalDraft"),
    ] {
        let mut mixed = envelope.clone();
        let object = mixed.as_object_mut().expect("object");
        for (key, value) in native.as_object().expect("native fields") {
            object.insert(key.clone(), value.clone());
        }
        let error = resolve_submission(event_body(mixed), &HeaderMap::new())
            .expect_err("mixed envelope and native fields");
        assert_eq!(error.code(), ErrorCode::InvalidInput, "{expected}");
        assert_eq!(field(&error), expected);
    }
}

#[test]
fn official_events_envelope_keeps_the_existing_empty_input_check() {
    let error = resolve_submission(
        event_body(official_envelope(
            json!([{ "type": "input_text", "text": "   " }]),
        )),
        &HeaderMap::new(),
    )
    .expect_err("blank text");

    assert_eq!(error.code(), ErrorCode::EmptyInput);
}
