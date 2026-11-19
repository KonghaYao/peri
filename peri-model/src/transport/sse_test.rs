use super::sse::{MAX_SSE_EVENT_BYTES, MAX_SSE_LINE_BYTES};
use super::{SseEvent, SseParser};
use crate::{ModelResult, ProtocolErrorKind};

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

#[test]
fn test_sse_parser_rejects_unterminated_line_beyond_budget() {
    let mut parser = SseParser::new();
    let oversized = vec![b'x'; MAX_SSE_LINE_BYTES + 1];
    let error = parser
        .push(&oversized)
        .into_iter()
        .next()
        .expect("budget error")
        .expect_err("unterminated oversized line");

    assert_eq!(
        error.protocol_error().map(|error| error.kind()),
        Some(ProtocolErrorKind::SseDecodeBudgetExceeded)
    );
    assert_eq!(parser.buffered_bytes(), 0);
    assert!(parser.push(b"data: after\n\n").is_empty());
}

#[test]
fn test_sse_parser_rejects_terminated_line_beyond_budget() {
    let mut parser = SseParser::new();
    let mut oversized = b"data: ".to_vec();
    oversized.extend(std::iter::repeat_n(b'x', MAX_SSE_LINE_BYTES));
    oversized.extend_from_slice(b"\n\n");
    let error = parser
        .push(&oversized)
        .into_iter()
        .next()
        .expect("budget error")
        .expect_err("terminated oversized line");

    assert_eq!(
        error.protocol_error().map(|error| error.kind()),
        Some(ProtocolErrorKind::SseDecodeBudgetExceeded)
    );
    assert_eq!(parser.buffered_bytes(), 0);
}

#[test]
fn test_sse_parser_accepts_exact_limit_crlf_across_chunk_boundaries() {
    let prefix = b"data: ";
    let mut content = prefix.to_vec();
    content.extend(std::iter::repeat_n(b'x', MAX_SSE_LINE_BYTES - prefix.len()));
    let expected_data = "x".repeat(MAX_SSE_LINE_BYTES - prefix.len());

    let mut unsplit = content.clone();
    unsplit.extend_from_slice(b"\r\n\r\n");
    let mut parser = SseParser::new();
    let events = push(&mut parser, &unsplit).expect("unsplit CRLF");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].data, expected_data);

    // 切在 \r 之后：pending 比内容上限多出 1 字节，仍属于未完成的 CRLF 终止符。
    let mut split_at_cr = content.clone();
    split_at_cr.push(b'\r');
    let mut parser = SseParser::new();
    assert!(push(&mut parser, &split_at_cr)
        .expect("awaiting LF")
        .is_empty());
    let events = push(&mut parser, b"\n\r\n").expect("split CRLF");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].data, expected_data);

    // 同样多出 1 字节但若是内容而非 \r，仍须判超限。
    let mut over_limit = content;
    over_limit.extend_from_slice(b"x\r");
    let mut parser = SseParser::new();
    let error = push(&mut parser, &over_limit).expect_err("over-limit line");
    assert_eq!(
        error.protocol_error().map(|error| error.kind()),
        Some(ProtocolErrorKind::SseDecodeBudgetExceeded)
    );
}

#[test]
fn test_sse_parser_rejects_event_beyond_budget() {
    let mut parser = SseParser::new();
    let line = format!("data: {}\n", "x".repeat(512 * 1024));
    let oversized = line.repeat(MAX_SSE_EVENT_BYTES / (512 * 1024) + 2);
    let error = parser
        .push(oversized.as_bytes())
        .into_iter()
        .next()
        .expect("budget error")
        .expect_err("oversized event");

    assert_eq!(
        error.protocol_error().map(|error| error.kind()),
        Some(ProtocolErrorKind::SseDecodeBudgetExceeded)
    );
    assert_eq!(parser.buffered_bytes(), 0);
}

#[test]
fn test_sse_parser_keeps_prior_events_before_budget_error() {
    let mut parser = SseParser::new();
    let mut bytes = b"data: first\n\nevent: huge\ndata: ".to_vec();
    bytes.extend(std::iter::repeat_n(b'x', MAX_SSE_LINE_BYTES + 1));
    let events = parser.push(&bytes);

    assert_eq!(events.len(), 2);
    assert_eq!(events[0].as_ref().expect("prior event").data, "first");
    assert_eq!(
        events[1]
            .as_ref()
            .expect_err("budget error")
            .protocol_error()
            .map(|error| error.kind()),
        Some(ProtocolErrorKind::SseDecodeBudgetExceeded)
    );
    assert_eq!(parser.buffered_bytes(), 0);
}

#[test]
fn test_sse_parser_counts_line_overhead_against_event_budget() {
    let mut parser = SseParser::new();
    let lines = MAX_SSE_EVENT_BYTES / std::mem::size_of::<String>() + 1;
    let empty_data_flood = "data:\n".repeat(lines);
    let error = parser
        .push(empty_data_flood.as_bytes())
        .into_iter()
        .next()
        .expect("budget error")
        .expect_err("empty data line flood");

    assert_eq!(
        error.protocol_error().map(|error| error.kind()),
        Some(ProtocolErrorKind::SseDecodeBudgetExceeded)
    );
    assert_eq!(parser.buffered_bytes(), 0);
}
