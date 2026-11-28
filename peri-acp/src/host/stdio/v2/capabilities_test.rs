use super::{initialize_response, ClientCapabilities};
use serde_json::json;

#[test]
fn elicitation_modes_require_an_explicit_declaration() {
    let form = ClientCapabilities::capture(&json!({
        "capabilities": {"elicitation": {"form": {}}}
    }));
    assert!(form.elicitation_form);
    assert!(!form.elicitation_url);
    assert!(form.supports_form_elicitation());

    let url = ClientCapabilities::capture(&json!({
        "capabilities": {"elicitation": {"url": {}}}
    }));
    assert!(!url.elicitation_form);
    assert!(url.elicitation_url);
    assert!(
        !url.supports_form_elicitation(),
        "url-only clients cannot answer form elicitations"
    );

    // Absent, null and malformed declarations all mean "not advertised": a
    // malformed value must not unlock a request the client cannot answer.
    for absent in [
        json!({}),
        json!({"capabilities": {}}),
        json!({"capabilities": {"elicitation": null}}),
        json!({"capabilities": {"elicitation": {"form": null}}}),
        json!({"capabilities": {"elicitation": {"form": true}}}),
        // The v1 field name is not a v2 declaration.
        json!({"clientCapabilities": {"elicitation": {"form": {}}}}),
    ] {
        let caps = ClientCapabilities::capture(&absent);
        assert!(!caps.elicitation_form, "{absent}");
        assert!(!caps.elicitation_url, "{absent}");
    }
}

#[test]
fn initialize_response_advertises_only_what_the_preview_honours() {
    let response = initialize_response();
    assert_eq!(response["protocolVersion"], 2);
    assert_eq!(response["info"]["name"], "peri");
    let session = &response["capabilities"]["session"];
    assert_eq!(session["prompt"]["image"], json!({}));
    assert_eq!(session["mcp"]["stdio"], json!({}));
    assert_eq!(session["mcp"]["http"], json!({}));
    assert_eq!(session["delete"], json!({}), "S1 lands session/delete");
    // Not advertised: auth, extra roots, system prompts, config controls.
    assert!(response.get("authMethods").is_none());
    assert!(session.get("additionalDirectories").is_none());
    assert!(session.get("systemPrompt").is_none());
    assert!(session.get("configOptions").is_none());
    assert!(response["capabilities"].get("_meta").is_none());
}
