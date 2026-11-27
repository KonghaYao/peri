use super::history_body_displayable_to_client_buffered;
use crate::history_page::history_body_displayable_to_client;
use crate::messages::BaseMessage;
use crate::store::{serialize_persisted_payload, PersistedPayload};
use std::io::{BufReader, Cursor};

fn assert_same(data: &[u8], sizes: &[usize]) {
    let expected = history_body_displayable_to_client(data).ok();
    for &size in sizes {
        let mut reader = BufReader::with_capacity(size, Cursor::new(data));
        assert_eq!(
            history_body_displayable_to_client_buffered(&mut reader).ok(),
            expected,
            "buffer size {size}"
        );
    }
}

#[test]
fn buffered_visibility_matches_canonical_tool_text_at_json_and_utf8_boundaries() {
    let text = "shell 输出：🪿\n\t\"x\"\\\u{8}\r".repeat(8192);
    let payload = PersistedPayload::Message(BaseMessage::tool_error("shell", text));
    let canonical = serialize_persisted_payload(&payload).unwrap();
    let start =
        super::tool_content_start(canonical.as_bytes()).expect("recognize the actual serializer");
    assert_same(
        canonical.as_bytes(),
        &[start + 1, 127, 128, 129, 1023, 1024, 1025, 65536],
    );
    let escaped = br#"\uD83D\uDE00\u0041\\\"\/\t\rx"#.repeat(64);
    let head = &canonical.as_bytes()[..start];
    let suffix = br#"","is_error":true}}"#;
    let value = [head, escaped.as_slice(), suffix.as_slice()].concat();
    assert!(history_body_displayable_to_client(value.as_slice()).unwrap());
    // Every possible cut around the first surrogate pair and subsequent escapes.
    for size in start + 1..=start + escaped.len().min(64) {
        assert_same(&value, &[size]);
    }
}

#[test]
fn buffered_visibility_preserves_trailing_fields_hidden_roles_and_malformed_rows() {
    let head =
        br#"{"version":1,"type":"message","message":{"id":"sample","role":"tool","content":""#;
    let mut prefix = head.to_vec();
    prefix.extend_from_slice(&vec![b'x'; 128 * 1024]);
    for tail in [
        br#""}}"#.as_slice(),
        br#"","role":"system"}}"#,
        br#""},"type":"system_reminder"}"#,
        br#"","content":"duplicate ignored field"}}"#,
        br#""}} extra"#,
        br#"\q"}}"#,
        br#"\uZ123"}}"#,
        br#"unterminated"#,
        br#""}"#,
        b"\x00\"}}",
        b"\xff\"}}",
        br#"\uD800"}}"#,
    ] {
        let data = [prefix.as_slice(), tail].concat();
        assert_same(&data, &[257, 4096, 65536]);
    }
    for noncanonical in [
        br#"{"role":"system","content":"hidden"}"#.as_slice(),
        br#"{"version":1,"type":"message","message":{"id":"a","role":"system","content":"hidden"}}"#,
        br#"{"version":1,"type":"message","message":{"role":"tool","content":[{"type":"text","text":"blocks"}]}}"#,
        br#"{"version":1,"type":"message","message":{"id":"a","x":{"content":"fake"},"role":"system"}}"#,
        br#"{"version":1,"type":"message","message":{"id":"a","content":"first","role":"system"},"other":{"content":"fake"}}"#,
        br#"{"version":1,"type":"system_reminder","reminder":{"content":"not a tool result"}}"#,
    ] {
        assert!(super::tool_content_start(noncanonical).is_none());
        assert_same(noncanonical, &[8,4096]);
    }
}
