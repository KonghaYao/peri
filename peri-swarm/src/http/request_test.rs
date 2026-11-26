use axum::http::{HeaderMap, HeaderValue};
use serde_json::json;

use super::{
    resolve_submission, resolve_tools, resolve_tools_value, CreateSessionBody, SubmitEventBody,
};
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
fn official_transport_shape_maps_onto_the_flat_spec() {
    let tools = resolve_tools(&body(json!({
        "logicalId": "agent-01",
        "agent": {
            "tools": [{
                "type": "mcp",
                "server_label": "get-time",
                "transport": {
                    "type": "http",
                    "server_url": "https://mcp.example.test/mcp",
                    "headers": { "X-Trace": "abc" },
                    "authorization": "caller-token"
                },
                "allowed_tools": ["get_time"]
            }]
        }
    })))
    .expect("official transport");

    let tool = &tools[0];
    assert_eq!(tool.server_label, "get-time");
    assert_eq!(tool.server_url, "https://mcp.example.test/mcp");
    assert_eq!(
        tool.headers
            .iter()
            .map(|header| (header.name.as_str(), header.value.as_str()))
            .collect::<Vec<_>>(),
        vec![("Authorization", "Bearer caller-token"), ("X-Trace", "abc"),],
        "authorization 补成 Bearer 头，headers 参与同一规范化排序"
    );
    assert_eq!(tool.allowed_tools, Some(json!(["get_time"])));
    assert!(
        !tool.extra.contains_key("transport"),
        "transport 已被解析，不再落进 extra"
    );
}

#[test]
fn an_explicit_authorization_header_wins_over_transport_authorization() {
    let tools = resolve_tools(&body(json!({
        "logicalId": "agent-01",
        "tools": [{
            "type": "mcp",
            "server_label": "get-time",
            "transport": {
                "type": "http",
                "server_url": "https://mcp.example.test/mcp",
                "headers": { "authorization": "Bearer explicit" },
                "authorization": "ignored"
            }
        }]
    })))
    .expect("transport");

    assert_eq!(tools[0].headers.len(), 1);
    assert_eq!(tools[0].headers[0].name, "authorization");
    assert_eq!(tools[0].headers[0].value, "Bearer explicit");
}

#[test]
fn transport_authorization_only_gains_a_missing_bearer_scheme() {
    for (authorization, expected) in [
        ("token", "Bearer token"),
        ("Bearer token", "Bearer token"),
        ("Basic abc", "Basic abc"),
        ("bearer token", "bearer token"),
    ] {
        let tools = resolve_tools(&body(json!({
            "logicalId": "agent-01",
            "tools": [{
                "type": "mcp",
                "server_label": "get-time",
                "transport": {
                    "type": "http",
                    "server_url": "https://mcp.example.test/mcp",
                    "authorization": authorization
                }
            }]
        })))
        .expect("transport");

        let authorization_headers = tools[0]
            .headers
            .iter()
            .filter(|header| header.name.eq_ignore_ascii_case("authorization"))
            .collect::<Vec<_>>();
        assert_eq!(
            authorization_headers.len(),
            1,
            "authorization 只能产生一条头：{authorization}"
        );
        assert_eq!(authorization_headers[0].value, expected, "{authorization}");
        assert!(
            !authorization_headers[0].value.contains("Bearer Bearer"),
            "不应重复 Bearer 前缀：{authorization}"
        );
    }
}

#[test]
fn an_empty_transport_authorization_is_rejected() {
    for authorization in ["", "   "] {
        let error = resolve_tools(&body(json!({
            "logicalId": "agent-01",
            "tools": [{
                "type": "mcp",
                "server_label": "get-time",
                "transport": {
                    "type": "http",
                    "server_url": "https://mcp.example.test/mcp",
                    "authorization": authorization
                }
            }]
        })))
        .expect_err("empty authorization");

        assert_eq!(error.code(), ErrorCode::InvalidInput, "{error:?}");
        assert_eq!(field(&error), "tools[0].transport.authorization");
        assert!(
            error.message().contains("empty"),
            "理由应写明不能为空：{}",
            error.message()
        );
    }
}

#[test]
fn flat_and_transport_server_urls_must_agree() {
    let accepted = resolve_tools(&body(json!({
        "logicalId": "agent-01",
        "tools": [{
            "type": "mcp",
            "server_label": "get-time",
            "server_url": "https://mcp.example.test/mcp",
            "transport": {
                "type": "http",
                "server_url": "https://mcp.example.test/mcp"
            }
        }]
    })))
    .expect("same URL in both shapes");
    assert_eq!(accepted[0].server_url, "https://mcp.example.test/mcp");

    let error = resolve_tools(&body(json!({
        "logicalId": "agent-01",
        "agent": {
            "tools": [{
                "type": "mcp",
                "server_label": "get-time",
                "server_url": "https://legacy.example.test/mcp",
                "transport": {
                    "type": "http",
                    "server_url": "https://mcp.example.test/mcp"
                }
            }]
        }
    })))
    .expect_err("disagreeing URLs");
    assert_eq!(error.code(), ErrorCode::InvalidInput);
    assert_eq!(field(&error), "agent.tools[0].transport");
}

#[test]
fn stdio_and_unknown_transports_are_a_stated_gap() {
    for (transport, expected) in [
        (
            json!({ "type": "stdio", "command": "uvx", "cwd": "/tmp" }),
            "stdio",
        ),
        (
            json!({ "type": "sse", "server_url": "https://mcp.test" }),
            "sse",
        ),
    ] {
        let error = resolve_tools(&body(json!({
            "logicalId": "agent-01",
            "tools": [{
                "type": "mcp",
                "server_label": "get-time",
                "transport": transport
            }]
        })))
        .expect_err("unsupported transport");
        assert_eq!(error.code(), ErrorCode::UnsupportedTool, "{error:?}");
        assert_eq!(error.kind(), ErrorKind::Unprocessable);
        assert_eq!(field(&error), "tools[0].transport.type");
        assert_eq!(error.details()["value"], json!(expected));
        assert_eq!(error.details()["supported"], json!(["http"]));
    }
}

#[test]
fn a_malformed_transport_reports_its_nested_path() {
    for (transport, expected) in [
        (json!("http"), "tools[0].transport"),
        (
            json!({ "server_url": "https://mcp.test" }),
            "tools[0].transport.type",
        ),
        (json!({ "type": 7 }), "tools[0].transport.type"),
        (json!({ "type": "http" }), "tools[0].transport.server_url"),
        (
            json!({ "type": "http", "server_url": "" }),
            "tools[0].transport.server_url",
        ),
        (
            json!({
                "type": "http",
                "server_url": "https://mcp.test",
                "headers": "X-Trace: abc"
            }),
            "tools[0].transport.headers",
        ),
        (
            json!({
                "type": "http",
                "server_url": "https://mcp.test",
                "authorization": 7
            }),
            "tools[0].transport.authorization",
        ),
    ] {
        let error = resolve_tools(&body(json!({
            "logicalId": "agent-01",
            "tools": [{
                "type": "mcp",
                "server_label": "get-time",
                "transport": transport
            }]
        })))
        .expect_err("malformed transport");
        assert_eq!(error.code(), ErrorCode::InvalidInput, "{error:?}");
        assert_eq!(field(&error), expected);
    }
}

#[test]
fn sessions_update_tools_accept_the_official_transport_shape() {
    let tools = resolve_tools_value(
        &json!([{
            "type": "mcp",
            "server_label": "get-time",
            "transport": {
                "type": "http",
                "server_url": "https://mcp.example.test/mcp",
                "authorization": "caller-token"
            },
            "allowed_tools": ["get_time"]
        }]),
        "agent.tools",
    )
    .expect("update tools");

    assert_eq!(tools[0].server_url, "https://mcp.example.test/mcp");
    assert_eq!(tools[0].headers[0].name, "Authorization");
    assert_eq!(tools[0].headers[0].value, "Bearer caller-token");
    assert_eq!(tools[0].allowed_tools, Some(json!(["get_time"])));
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

#[test]
fn an_approval_result_event_resolves_to_a_decision() {
    let event = super::resolve_event(
        event_body(json!({
            "events": [{
                "type": "agent.session.input.mcp_approval_request_result",
                "request_id": "appr_0192f0c5-0000-7000-8000-000000000001",
                "response": { "type": "mcp_tool_call", "decision": "approve" },
            }]
        })),
        &HeaderMap::new(),
    )
    .expect("approval event");
    let super::SubmitEvent::Approval(answer) = event else {
        panic!("expected an approval answer");
    };
    assert_eq!(
        answer.request_id,
        "appr_0192f0c5-0000-7000-8000-000000000001"
    );
    assert_eq!(answer.decision, crate::domain::ApprovalDecision::Approve);
    assert!(answer.input_id.is_none());
}

#[test]
fn the_native_top_level_decision_alias_still_resolves() {
    let event = super::resolve_event(
        event_body(json!({
            "events": [{
                "type": "agent.session.input.mcp_approval_request_result",
                "requestId": "appr_alias",
                "decision": "deny",
            }]
        })),
        &HeaderMap::new(),
    )
    .expect("native extension");
    let super::SubmitEvent::Approval(answer) = event else {
        panic!("expected an approval answer");
    };
    assert_eq!(answer.request_id, "appr_alias");
    assert_eq!(answer.decision, crate::domain::ApprovalDecision::Deny);
}

#[test]
fn an_approval_response_type_must_be_mcp_tool_call() {
    let error = super::resolve_event(
        event_body(json!({
            "events": [{
                "type": "agent.session.input.mcp_approval_request_result",
                "request_id": "appr_1",
                "response": { "type": "computer_use", "decision": "approve" },
            }]
        })),
        &HeaderMap::new(),
    )
    .expect_err("wrong response type");
    assert_eq!(error.code(), ErrorCode::InvalidInput);
    assert_eq!(field(&error), "events[0].response.type");
    assert!(error.message().contains("mcp_tool_call"));
}

#[test]
fn an_approval_result_carries_the_idempotency_key_and_rejects_unknown_decisions() {
    let key = "0192f0c5-0000-7000-8000-0000000000bb";
    let event = super::resolve_event(
        event_body(json!({
            "events": [{
                "type": "agent.session.input.mcp_approval_request_result",
                "request_id": "appr_1",
                "response": { "type": "mcp_tool_call", "decision": "cancel" },
            }]
        })),
        &idempotency_headers(key),
    )
    .expect("approval event");
    let super::SubmitEvent::Approval(answer) = event else {
        panic!("expected an approval answer");
    };
    assert_eq!(answer.input_id.as_deref(), Some(key));
    assert_eq!(answer.decision, crate::domain::ApprovalDecision::Cancel);

    let error = super::resolve_event(
        event_body(json!({
            "events": [{
                "type": "agent.session.input.mcp_approval_request_result",
                "request_id": "appr_1",
                "response": { "type": "mcp_tool_call", "decision": "maybe" },
            }]
        })),
        &HeaderMap::new(),
    )
    .expect_err("unknown decision");
    assert_eq!(error.code(), ErrorCode::InvalidInput);
    assert_eq!(field(&error), "events[0].response.decision");
    assert!(error.message().contains("approve"));
}

#[test]
fn an_approval_result_requires_the_pending_id() {
    let error = super::resolve_event(
        event_body(json!({
            "events": [{
                "type": "agent.session.input.mcp_approval_request_result",
                "response": { "type": "mcp_tool_call", "decision": "approve" },
            }]
        })),
        &HeaderMap::new(),
    )
    .expect_err("missing id");
    assert_eq!(field(&error), "events[0].request_id");
}

#[test]
fn an_approval_result_requires_the_official_response_object() {
    let error = super::resolve_event(
        event_body(json!({
            "events": [{
                "type": "agent.session.input.mcp_approval_request_result",
                "request_id": "appr_1",
            }]
        })),
        &HeaderMap::new(),
    )
    .expect_err("missing response");
    assert_eq!(field(&error), "events[0].response");

    let error = super::resolve_event(
        event_body(json!({
            "events": [{
                "type": "agent.session.input.mcp_approval_request_result",
                "request_id": "appr_1",
                "response": { "type": "mcp_tool_call" },
            }]
        })),
        &HeaderMap::new(),
    )
    .expect_err("missing decision");
    assert_eq!(field(&error), "events[0].response.decision");
}
