use super::{initialize_params, prompt_blocks, setup_params};
use serde_json::json;

#[test]
fn v2_negotiation_never_inherits_v1_client_tool_capabilities() {
    let params = initialize_params(&json!({
        "info":{"name":"client","version":"1"}, "clientCapabilities": {"terminal":true}
    }))
    .unwrap();
    assert!(params["clientCapabilities"].get("terminal").is_none());
    assert_eq!(params["protocolVersion"], 1);
    assert!(initialize_params(&json!({"info":{"name":"client"}})).is_err());
}

#[test]
fn initialize_keeps_the_client_elicitation_declaration_and_internal_caps() {
    let params = initialize_params(&json!({
        "protocolVersion": 2,
        "info": {"name": "client", "version": "1"},
        "clientCapabilities": {"elicitation": {"form": {}}, "auth": {"terminal": {}}}
    }))
    .unwrap();
    let capabilities = &params["clientCapabilities"];
    assert_eq!(capabilities["elicitation"], json!({"form": {}}));
    assert!(
        capabilities.get("auth").is_none(),
        "v2 has no authentication to forward"
    );
    assert_eq!(capabilities["_meta"]["peri.userInputQueue"], true);
    assert_eq!(capabilities["_meta"]["peri.replay"], true);
}

#[test]
fn setup_requires_an_absolute_cwd_and_refuses_unadvertised_fields() {
    assert!(setup_params(&json!({"cwd": "/workspace"})).is_ok());
    assert_eq!(
        setup_params(&json!({})).unwrap_err().code,
        -32602,
        "cwd is required"
    );
    assert_eq!(
        setup_params(&json!({"cwd": "relative/path"}))
            .unwrap_err()
            .code,
        -32602
    );
    assert_eq!(
        setup_params(&json!({"cwd": "/workspace", "additionalDirectories": ["/other"]}))
            .unwrap_err()
            .code,
        -32602,
        "additionalDirectories is not advertised"
    );
    assert!(setup_params(&json!({"cwd": "/workspace", "additionalDirectories": []})).is_ok());
    assert_eq!(
        setup_params(&json!({"cwd": "/workspace", "systemPrompt": {"text": "hi"}}))
            .unwrap_err()
            .code,
        -32602,
        "systemPrompt is not advertised"
    );
    assert!(setup_params(&json!({"cwd": "/workspace", "systemPrompt": null})).is_ok());
}

#[test]
fn prompt_content_accepts_text_image_and_resource_links_only() {
    let accepted = prompt_blocks(&json!({"prompt": [
        {"type": "text", "text": "hello"},
        {"type": "image", "mimeType": "image/png", "data": "AA=="},
        {"type": "resource_link", "uri": "file:///notes.md", "name": "notes.md"}
    ]}))
    .unwrap();
    let blocks = accepted.as_array().unwrap();
    assert_eq!(blocks[0]["type"], "text");
    assert_eq!(blocks[1]["type"], "image");
    assert_eq!(blocks[2]["type"], "text");
    assert_eq!(blocks[2]["text"], "notes.md: file:///notes.md");

    for unsupported in [
        json!({"prompt": []}),
        json!({"prompt": [{"type": "audio", "mimeType": "audio/wav", "data": "AA=="}]}),
        json!({"prompt": [{"type": "resource", "resource": {"uri": "file:///a"}}]}),
        json!({"prompt": [{"type": "text"}]}),
        json!({}),
    ] {
        assert_eq!(
            prompt_blocks(&unsupported).unwrap_err().code,
            -32602,
            "{unsupported}"
        );
    }
}
