use crate::{transport::SseEvent, ProtocolErrorKind};

#[test]
fn decoder_preserves_json_parse_error_and_original_frame() {
    let factory = super::stream::decoders();
    let (decoder, _) = factory();
    let body = "{invalid sk-live-secret JSON 诊断";
    let error = decoder(
        SseEvent {
            event: None,
            data: body.into(),
        },
        None,
    )
    .unwrap_err();
    let diagnostic = error.diagnostic();
    assert_eq!(diagnostic.protocol(), Some(ProtocolErrorKind::Provider));
    assert_eq!(diagnostic.body(), Some(body));
    assert!(diagnostic.message().unwrap().contains("line 1"));
    assert!(error.to_string().contains("sk-live-secret"));
}

#[test]
fn decoder_preserves_provider_error_message_and_raw_frame() {
    let factory = super::stream::decoders();
    let (decoder, _) = factory();
    let body = r#"{"type":"error","error":{"message":"sk-live-secret 诊断"}}"#;
    let error = decoder(
        SseEvent {
            event: Some("error".into()),
            data: body.into(),
        },
        None,
    )
    .unwrap_err();
    let diagnostic = error.diagnostic();
    assert_eq!(diagnostic.protocol(), Some(ProtocolErrorKind::Provider));
    assert_eq!(diagnostic.body(), Some(body));
    assert!(diagnostic
        .message()
        .unwrap()
        .contains("sk-live-secret 诊断"));
}
