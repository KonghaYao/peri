use super::*;

fn submission(input_id: &str, text: &str) -> InputSubmission {
    InputSubmission {
        input_id: input_id.to_owned(),
        kind: InputKind::User,
        mode: InputMode::Turn,
        text: Some(text.to_owned()),
        attachments: Vec::new(),
        plugin_selections: Vec::new(),
        original_draft: None,
    }
}

#[test]
fn fingerprint_is_stable_and_field_sensitive() {
    let base = submission("input-1", "hello");
    assert_eq!(base.fingerprint(), base.fingerprint());
    assert_eq!(base.fingerprint().len(), 64);

    let mut changed_text = base.clone();
    changed_text.text = Some("hello!".to_owned());
    assert_ne!(base.fingerprint(), changed_text.fingerprint());

    let mut changed_mode = base.clone();
    changed_mode.mode = InputMode::Steer;
    assert_ne!(base.fingerprint(), changed_mode.fingerprint());

    let mut changed_plugin = base.clone();
    changed_plugin.plugin_selections = vec!["alpha".to_owned()];
    assert_ne!(base.fingerprint(), changed_plugin.fingerprint());
}

#[test]
fn effective_text_joins_text_and_attachments() {
    let mut input = submission("input-1", "first");
    input.attachments = vec![ContentBlock::text("second"), ContentBlock::text("third")];
    assert_eq!(input.effective_text(), "first\nsecond\nthird");
}

#[test]
fn emptiness_accounts_for_whitespace_and_attachments() {
    let empty = submission("input-1", "   ");
    assert!(empty.is_empty());

    let mut with_attachment = empty;
    with_attachment.attachments = vec![ContentBlock::text("content")];
    assert!(!with_attachment.is_empty());

    let mut whitespace_blocks = submission("input-1", "");
    whitespace_blocks.text = None;
    whitespace_blocks.attachments = vec![ContentBlock::text("  ")];
    assert!(whitespace_blocks.is_empty());
}

#[test]
fn content_blocks_accept_only_text_and_reject_unknown_fields() {
    let block: ContentBlock =
        serde_json::from_value(serde_json::json!({ "type": "text", "text": "hi" }))
            .expect("text block");
    assert_eq!(block.as_text(), "hi");

    let wrong_tag = serde_json::from_value::<ContentBlock>(serde_json::json!({
        "type": "image",
        "url": "https://example.invalid/x.png"
    }));
    assert!(wrong_tag.is_err());

    let unknown_field = serde_json::from_value::<ContentBlock>(serde_json::json!({
        "type": "text",
        "text": "hi",
        "extra": true
    }));
    assert!(unknown_field.is_err());
}
