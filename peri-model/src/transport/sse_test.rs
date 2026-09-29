use super::*;
use crate::ModelResult;

fn push(parser: &mut SseParser, bytes: &[u8]) -> ModelResult<Vec<SseEvent>> {
    parser.push(bytes).into_iter().collect()
}

#[test]
fn test_sse_parser_emits_basic_data_event() {
    let mut parser = SseParser::new();
    let events = push(&mut parser, b"data: {\"key\":\"value\"}\n\n").expect("valid UTF-8");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event, None);
    assert_eq!(events[0].data, "{\"key\":\"value\"}");
}

#[test]
fn test_sse_parser_handles_crlf_multiple_data_and_events() {
    let mut parser = SseParser::new();
    let events = push(&mut parser,
            b"event: first\r\ndata: line one\r\ndata: line two\r\n\r\nevent: second\r\ndata: value\r\n\r\n",
        )
        .expect("valid UTF-8");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event.as_deref(), Some("first"));
    assert_eq!(events[0].data, "line one\nline two");
    assert_eq!(events[1].event.as_deref(), Some("second"));
    assert_eq!(events[1].data, "value");
}

#[test]
fn test_sse_parser_joins_line_split_across_chunks() {
    let mut parser = SseParser::new();
    assert!(push(&mut parser, b"data: {\"partial")
        .expect("valid UTF-8")
        .is_empty());
    let events = push(&mut parser, b"_key\":\"value\"}\n\n").expect("valid UTF-8");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].data, "{\"partial_key\":\"value\"}");
}

#[test]
fn test_sse_parser_preserves_utf8_split_across_chunks() {
    let mut parser = SseParser::new();
    assert!(push(&mut parser, b"data: \xf0\x9f")
        .expect("incomplete UTF-8 is buffered")
        .is_empty());
    let events = push(&mut parser, b"\x8e\x89\n\n").expect("valid UTF-8");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].data, "🎉");
}

#[test]
fn test_sse_parser_rejects_invalid_utf8() {
    let mut parser = SseParser::new();
    let error = push(&mut parser, b"data: \xff\n\n").expect_err("invalid UTF-8");

    assert_eq!(
        error.protocol_error().map(|error| error.kind()),
        Some(crate::ProtocolErrorKind::Provider)
    );
    assert!(!parser.is_done());
}

#[test]
fn test_sse_parser_dispatches_empty_data_and_ignores_event_without_data() {
    let mut parser = SseParser::new();
    let events =
        push(&mut parser, b"event: ignored\n\ndata: \n\ndata: value\n\n").expect("valid UTF-8");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event, None);
    assert_eq!(events[0].data, "");
    assert_eq!(events[1].event, None);
    assert_eq!(events[1].data, "value");
}

#[test]
fn test_sse_parser_preserves_empty_data_lines_in_multiline_events() {
    let mut parser = SseParser::new();
    let events = push(
        &mut parser,
        b"event: message\ndata: first\ndata:\ndata: third\n\n",
    )
    .expect("valid UTF-8");

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event.as_deref(), Some("message"));
    assert_eq!(events[0].data, "first\n\nthird");
}

#[test]
fn test_sse_parser_stops_at_done_without_emitting_it() {
    let mut parser = SseParser::new();
    let events = push(
        &mut parser,
        b"data: before\n\ndata:[DONE]\n\ndata: after\n\n",
    )
    .expect("valid UTF-8");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].data, "before");
    assert!(parser.is_done());
    assert!(push(&mut parser, b"data: ignored\n\n")
        .expect("valid UTF-8")
        .is_empty());
}

#[test]
fn test_sse_parser_keeps_incomplete_tail_buffered() {
    let mut parser = SseParser::new();
    assert!(push(&mut parser, b"data: partial")
        .expect("incomplete line is buffered")
        .is_empty());
    assert!(push(&mut parser, b"")
        .expect("incomplete line is buffered")
        .is_empty());
    assert!(!parser.is_done());
}
