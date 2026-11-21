use serde_json::{json, Map, Value};

use super::{McpHeader, McpToolKind, McpToolSpec};

fn spec() -> McpToolSpec {
    McpToolSpec {
        kind: McpToolKind::Mcp,
        server_label: "deepwiki".to_owned(),
        server_url: "https://mcp.example.test/mcp".to_owned(),
        headers: vec![
            McpHeader {
                name: "Authorization".to_owned(),
                value: "Bearer caller-token".to_owned(),
            },
            McpHeader {
                name: "X-Trace".to_owned(),
                value: "abc".to_owned(),
            },
        ],
        require_approval: Some(json!({ "never": { "tool_names": ["read"] } })),
        allowed_tools: Some(json!({ "tool_names": ["read"], "read_only": true })),
        extra: Map::from_iter([("defer_loading".to_owned(), json!(true))]),
    }
}

#[test]
fn canonical_json_is_the_complete_declaration_and_round_trips() {
    let tools = vec![spec()];
    let text = McpToolSpec::encode_declaration(&tools)
        .expect("encode")
        .expect("non-empty");
    let value: Value = serde_json::from_str(&text).expect("json");

    assert_eq!(value[0]["type"], json!("mcp"));
    assert_eq!(value[0]["server_label"], json!("deepwiki"));
    assert_eq!(
        value[0]["server_url"],
        json!("https://mcp.example.test/mcp")
    );
    assert_eq!(
        value[0]["headers"],
        json!([
            { "name": "Authorization", "value": "Bearer caller-token" },
            { "name": "X-Trace", "value": "abc" }
        ])
    );
    assert_eq!(
        value[0]["require_approval"],
        json!({ "never": { "tool_names": ["read"] } })
    );
    assert_eq!(
        value[0]["allowed_tools"],
        json!({ "tool_names": ["read"], "read_only": true })
    );
    assert_eq!(value[0]["defer_loading"], json!(true));

    assert_eq!(
        McpToolSpec::decode_declaration(Some(&text)).expect("decode"),
        tools
    );
}

#[test]
fn an_empty_declaration_is_null_and_reads_back_as_an_empty_list() {
    assert_eq!(McpToolSpec::encode_declaration(&[]).unwrap(), None);
    assert_eq!(McpToolSpec::decode_declaration(None).unwrap(), Vec::new());
    assert_eq!(
        McpToolSpec::decode_declaration(Some("[]")).unwrap(),
        Vec::new()
    );
}

#[test]
fn a_declaration_without_optional_fields_omits_them_from_the_canonical_json() {
    let text = McpToolSpec::encode_declaration(&[McpToolSpec::new("label", "https://mcp.test")])
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&text).unwrap(),
        json!([{
            "type": "mcp",
            "server_label": "label",
            "server_url": "https://mcp.test"
        }])
    );
}

#[test]
fn invalid_stored_json_is_reported_instead_of_becoming_an_empty_list() {
    assert!(McpToolSpec::decode_declaration(Some("not json")).is_err());
    assert!(McpToolSpec::decode_declaration(Some(r#"[{"type":"function"}]"#)).is_err());
}
