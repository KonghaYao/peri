use super::*;

#[test]
fn current_metadata_keeps_distinct_invocation_and_model_call_ids() {
    let mut context = ToolContext::new(&[], ".").with_session_identity("session", "turn");
    context.invocation_id = Some("invocation".into());
    context.tool_call_id = Some("model-call".into());
    let mut existing = RequestMetaObject::default();
    existing
        .0
         .0
        .insert("trusted-scope".into(), serde_json::json!("scope"));
    let metadata = request_meta(&context, Some(existing)).unwrap();
    assert_eq!(metadata.0 .0["trusted-scope"], "scope");
    assert_eq!(
        metadata.0 .0[INVOCATION_META_KEY]["invocationId"],
        "invocation"
    );
    assert_eq!(
        metadata.0 .0[INVOCATION_META_KEY]["toolCallId"],
        "model-call"
    );
    assert_eq!(
        metadata.0 .0[INVOCATION_META_KEY]["initiatorSessionId"],
        "session"
    );
}

#[test]
fn ordinary_call_does_not_require_durable_identity() {
    let context = ToolContext::new(&[], ".");
    let metadata = request_meta(&context, None).unwrap();
    assert!(metadata.0 .0[INVOCATION_META_KEY]["invocationId"].is_null());
    assert!(metadata.0 .0[INVOCATION_META_KEY]
        .get("recipientLifecycle")
        .is_none());
}

#[test]
fn untrusted_invocation_metadata_cannot_replace_current_context() {
    let context = ToolContext::new(&[], ".");
    let metadata = request_meta(&context, None).unwrap();
    assert!(request_meta(&context, Some(metadata)).is_err());
}
