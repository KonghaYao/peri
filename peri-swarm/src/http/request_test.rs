use serde_json::json;

use super::{resolve_tools, CreateSessionBody};
use crate::domain::{ErrorCode, ErrorKind};

fn body(value: serde_json::Value) -> CreateSessionBody {
    serde_json::from_value(value).expect("create body")
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
