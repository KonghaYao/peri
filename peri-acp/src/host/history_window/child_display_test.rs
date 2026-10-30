use super::*;
use peri_acp_types::messages::{BaseMessage, ContentBlock};
use peri_acp_types::store::{serialize_persisted_payload, PersistedPayload};

struct OneByte<R>(R);
impl<R: Read> Read for OneByte<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let len = output.len().min(1);
        self.0.read(&mut output[..len])
    }
}

#[test]
fn child_display_filters_provider_state_signature_and_unknown_fields() {
    let payload = PersistedPayload::Message(BaseMessage::ai_from_blocks(vec![
        ContentBlock::reasoning_with_signature("visible reasoning", "private signature"),
        ContentBlock::text("visible answer"),
        ContentBlock::ProviderState {
            provider: "openai_responses".into(),
            data: serde_json::json!({"text":"private replay", "content":"private content"}),
        },
        ContentBlock::tool_use("tool-1", "Read", serde_json::json!({"path":"visible.rs"})),
        ContentBlock::tool_result("tool-1", vec![ContentBlock::text("visible result")], false),
    ]));
    let encoded = serialize_persisted_payload(&payload).unwrap();
    let chunk = project(encoded.as_bytes(), 0, 4096).unwrap();
    let text = String::from_utf8(chunk.bytes).unwrap();
    for visible in [
        "visible reasoning",
        "visible answer",
        "visible.rs",
        "visible result",
    ] {
        assert!(text.contains(visible));
    }
    for private in [
        "private signature",
        "private replay",
        "private content",
        "encrypted_content",
    ] {
        assert!(!text.contains(private));
    }
    assert_eq!(
        text.matches("visible.rs").count(),
        1,
        "mirrored tool_calls must not duplicate embedded calls"
    );
    assert!(chunk.complete);
}

#[test]
fn child_display_handles_split_escapes_surrogates_and_utf8_windows() {
    let encoded = r#"{"version":1,"type":"message","message":{"role":"assistant","id":"x","content":"A\n\"\\\t\uD83D\uDE00中文"}}"#;
    let full = project(OneByte(encoded.as_bytes()), 0, 4096).unwrap().bytes;
    let text = String::from_utf8(full.clone()).unwrap();
    assert!(text.contains("A\n\"\\\t😀中文"));
    let mut joined = Vec::new();
    let mut offset = 0;
    loop {
        let chunk = project(OneByte(encoded.as_bytes()), offset, 7).unwrap();
        assert!(std::str::from_utf8(&chunk.bytes).is_ok());
        assert!(chunk.bytes.len() <= 7);
        let end = offset + chunk.bytes.len() as u64;
        joined.extend_from_slice(&chunk.bytes);
        if let Some(next) = chunk.next_offset {
            assert_eq!(next, end);
            offset = next;
        } else {
            assert!(chunk.complete);
            break;
        }
    }
    assert_eq!(joined, full);
}

#[test]
fn child_display_rejects_unknown_late_types_and_malformed_json() {
    let mixed = br#"{"version":1,"type":"message","message":{"role":"assistant","content":"visible"},"content":"private unknown envelope field"}"#;
    assert_eq!(
        project(mixed.as_slice(), 0, 4096).unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
    for block in [
        r#"{"text":"must not escape","type":"provider_state"}"#,
        r#"{"type":"future","text":"must not escape"}"#,
    ] {
        let encoded = format!(
            r#"{{"version":1,"type":"message","message":{{"role":"assistant","content":[{block}]}}}}"#
        );
        assert_eq!(
            project(encoded.as_bytes(), 0, 4096).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
    }
    for encoded in [
        r#"{"role":"assistant","content":"unterminated}"#,
        r#"{"role":"assistant","content":"\uD800x"}"#,
        r#"{"role":"assistant","content":"\uDC00"}"#,
        r#"{"role":"assistant","content":"\q"}"#,
        r#"{"role":"assistant","content":"ok",}"#,
    ] {
        assert!(
            project(OneByte(encoded.as_bytes()), 0, 32).is_err(),
            "accepted malformed input"
        );
    }
    assert_eq!(
        project(
            br#"{"version":2,"type":"message","message":{"content":"x"}}"#.as_slice(),
            0,
            32
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::Unsupported
    );
}

#[test]
fn child_display_large_visible_string_only_retains_the_requested_window() {
    let prefix = std::io::Cursor::new(br#"{"role":"assistant","content":""#.as_slice());
    let body = std::io::repeat(b'x').take(4 * 1024 * 1024);
    let suffix = std::io::Cursor::new(br#""}"#.as_slice());
    let chunk = project(prefix.chain(body).chain(suffix), 2 * 1024 * 1024, 64).unwrap();
    assert_eq!(chunk.bytes, vec![b'x'; 64]);
    assert!(chunk.bytes.capacity() <= 64);
    assert_eq!(chunk.next_offset, Some(2 * 1024 * 1024 + 64));
    assert!(!chunk.complete);
}

#[test]
fn child_visibility_matches_shared_reminder_egress_with_maximum_identifiers() {
    use peri_acp_types::system_reminder::*;
    for category in [ReminderCategory::Task, ReminderCategory::Legacy] {
        for delivery in [
            ReminderDelivery::Required,
            ReminderDelivery::Configurable,
            ReminderDelivery::DiagnosticOnly,
        ] {
            for audience in [ReminderAudience::Tui, ReminderAudience::Model] {
                let dto = SystemReminder {
                    version: SYSTEM_REMINDER_VERSION,
                    category: category.clone(),
                    source: ReminderSource("s".repeat(128)),
                    kind: "k".repeat(128),
                    severity: ReminderSeverity::Info,
                    delivery,
                    audiences: ReminderAudiences(vec![audience]),
                    body: String::new(),
                    summary: Some("summary only".into()),
                    metadata: serde_json::json!({"small":[1,true,"x"]}),
                };
                let encoded = serde_json::json!({
                    "version":1, "type":"system_reminder", "reminder":dto,
                })
                .to_string();
                let expected = peri_acp_types::history_page::history_body_displayable_to_client(
                    encoded.as_bytes(),
                )
                .unwrap();
                assert_eq!(visible(OneByte(encoded.as_bytes())).unwrap(), expected);
                if expected {
                    let text = project(encoded.as_bytes(), 0, 4096).unwrap();
                    assert!(String::from_utf8(text.bytes)
                        .unwrap()
                        .contains("summary only"));
                } else {
                    assert!(project(encoded.as_bytes(), 0, 4096).is_err());
                }
            }
        }
    }
}

struct CountRead<R> {
    reader: R,
    max_buffer: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    bytes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}
impl<R: Read> Read for CountRead<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        use std::sync::atomic::Ordering;
        self.max_buffer.fetch_max(output.len(), Ordering::Relaxed);
        let count = self.reader.read(output)?;
        self.bytes.fetch_add(count, Ordering::Relaxed);
        Ok(count)
    }
}

#[test]
fn child_visibility_rejects_generated_large_reminder_without_collecting_its_body() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for metadata in [false, true] {
        let prefix = if metadata {
            br#"{"version":1,"type":"system_reminder","reminder":{"body":"","metadata":{"huge":""#
                .as_slice()
        } else {
            br#"{"version":1,"type":"system_reminder","reminder":{"body":""#.as_slice()
        };
        let suffix = if metadata {
            br#""}}}"#.as_slice()
        } else {
            br#""}}"#.as_slice()
        };
        let max_buffer = std::sync::Arc::new(AtomicUsize::new(0));
        let bytes = std::sync::Arc::new(AtomicUsize::new(0));
        let reader = CountRead {
            reader: std::io::Cursor::new(prefix)
                .chain(std::io::repeat(b'x').take(4 * 1024 * 1024))
                .chain(std::io::Cursor::new(suffix)),
            max_buffer: max_buffer.clone(),
            bytes: bytes.clone(),
        };
        assert_eq!(
            visible(reader).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        assert!(max_buffer.load(Ordering::Relaxed) <= 8 * 1024);
        if metadata {
            assert!(
                bytes.load(Ordering::Relaxed) < 32 * 1024,
                "metadata limit must stop before reading the whole value"
            );
        }
    }
}

#[test]
fn child_display_rejects_all_mixed_envelope_variants() {
    let reminder = r#"{"version":1,"category":"task","source":"test","kind":"notice","severity":"info","delivery":"required","audiences":["tui"],"body":"visible","metadata":{}}"#;
    for encoded in [
        format!(r#"{{"version":1,"type":"system_reminder","reminder":{reminder},"message":{{"role":"assistant","content":"PRIVATE"}}}}"#),
        format!(r#"{{"version":1,"type":"message","message":{{"role":"assistant","content":"visible"}},"reminder":{reminder}}}"#),
        r#"{"role":"assistant","content":"visible","message":{"role":"assistant","content":"PRIVATE"}}"#.into(),
    ] {
        assert_eq!(project(encoded.as_bytes(), 0, 4096).unwrap_err().kind(), io::ErrorKind::Unsupported);
        assert!(visible(encoded.as_bytes()).is_err());
    }
}

#[test]
fn child_visibility_and_display_reject_duplicate_policy_or_envelope_fields() {
    for encoded in [
        r#"{"role":"system","role":"assistant","content":"PRIVATE"}"#,
        r#"{"version":1,"type":"message","message":{"role":"system","content":"PRIVATE"},"message":{"role":"assistant","content":"visible"}}"#,
        r#"{"version":1,"version":1,"type":"message","message":{"role":"assistant","content":"visible"}}"#,
        r#"{"version":1,"type":"message","message":{"role":"system","role":"assistant","content":"PRIVATE"}}"#,
    ] {
        assert!(visible(encoded.as_bytes()).is_err());
        assert!(project(encoded.as_bytes(), 0, 4096).is_err());
    }
}
