use super::*;
use peri_acp_types::tools::ToolDefinition;

fn checkpoint(request: &serde_json::Value, definitions: &[ToolDefinition]) -> ReasonRequest {
    request_checkpoint(
        &RequestCheckpoint {
            authorization_ref: "授权/🛰",
            request,
            tool_definitions: definitions,
        },
        "model".into(),
        "授权/🛰".into(),
    )
    .unwrap()
}

#[test]
fn borrowed_checkpoint_matches_large_legacy_json_bytes_and_digest() {
    let body = "上下文🛰\\\"\n\t".repeat(64 * 1024);
    let request = serde_json::json!({
        "provider": "fixture",
        "body": {
            "messages": [{"role": "user", "content": body}],
            "temperature": -0.0,
            "top_p": 0.125,
            "exponent": 1.25e-30,
            "integer": u64::MAX,
            "nested": {"z": null, "a": [true, "雪", 1.2345678901234567]},
        }
    });
    let definitions = vec![
        ToolDefinition {
            name: "z工具".into(),
            description: "first\n\"🛰".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"z": {"default": -0.0}, "a": {"enum": ["雪", "🛰"]}},
                "required": ["z", "a"],
            }),
        },
        ToolDefinition {
            name: "a工具".into(),
            description: "second".into(),
            parameters: serde_json::json!({"type": "object"}),
        },
    ];
    let legacy = request_checkpoint(
        &serde_json::json!({
            "request": request,
            "authorizationRef": "授权/🛰",
            "toolDefinitions": definitions,
        }),
        "model".into(),
        "授权/🛰".into(),
    )
    .unwrap();
    let borrowed = checkpoint(&request, &definitions);
    assert!(borrowed.serialized_request.len() > 1024 * 1024);
    assert_eq!(borrowed, legacy);
    assert_eq!(
        borrowed.request_digest,
        format!(
            "{:x}",
            Sha256::digest(borrowed.serialized_request.as_bytes())
        )
    );
    let decoded: serde_json::Value = serde_json::from_str(&borrowed.serialized_request).unwrap();
    assert_eq!(decoded["toolDefinitions"][0]["name"], "z工具");
    assert_eq!(decoded["toolDefinitions"][1]["name"], "a工具");
}

#[test]
fn borrowed_checkpoint_preserves_empty_tools_and_exact_outer_field_order() {
    let request = serde_json::json!({"z": "雪", "a": -0.0});
    let result = checkpoint(&request, &[]);
    assert_eq!(result.serialized_request, "{\"authorizationRef\":\"授权/🛰\",\"request\":{\"a\":-0.0,\"z\":\"雪\"},\"toolDefinitions\":[]}");
    assert_eq!(result, request_checkpoint(
        &serde_json::json!({"toolDefinitions": [], "request": request, "authorizationRef": "授权/🛰"}),
        "model".into(), "授权/🛰".into(),
    ).unwrap());
}
