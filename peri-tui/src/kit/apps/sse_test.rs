use super::*;

fn events(parser: &mut SseParser, chunks: &[&[u8]]) -> Vec<RawSseEvent> {
    let mut out = Vec::new();
    for chunk in chunks {
        out.extend(parser.feed(chunk));
    }
    out
}

#[test]
fn parses_event_and_data_across_chunk_boundaries() {
    let mut parser = SseParser::new();
    let got = events(
        &mut parser,
        &[
            b"event: fr",
            b"ame\ndata: {\"seq\":1}",
            b"\n\n",
            b"event: closed\ndata: {\"reason\":null}\n\n",
        ],
    );
    assert_eq!(
        got,
        vec![
            RawSseEvent {
                event: "frame".to_string(),
                data: "{\"seq\":1}".to_string()
            },
            RawSseEvent {
                event: "closed".to_string(),
                data: "{\"reason\":null}".to_string()
            }
        ]
    );
}

#[test]
fn ignores_keepalive_comments_and_unknown_fields() {
    let mut parser = SseParser::new();
    let got = events(
        &mut parser,
        &[b": keepalive\n\nid: 7\nretry: 1000\nevent: meta\ndata: {\"state\":\"starting\"}\n\n"],
    );
    assert_eq!(
        got,
        vec![RawSseEvent {
            event: "meta".to_string(),
            data: "{\"state\":\"starting\"}".to_string()
        }]
    );
}

#[test]
fn multiline_data_is_joined_with_newlines() {
    let mut parser = SseParser::new();
    let got = events(&mut parser, &[b"event: frame\ndata: a\ndata: b\n\n"]);
    assert_eq!(got[0].data, "a\nb");
}

#[test]
fn crlf_line_endings_are_accepted() {
    let mut parser = SseParser::new();
    let got = events(
        &mut parser,
        &[b"event: state\r\ndata: {\"state\":\"ready\"}\r\n\r\n"],
    );
    assert_eq!(got[0].event, "state");
    assert_eq!(got[0].data, "{\"state\":\"ready\"}");
}

#[test]
fn data_without_event_defaults_to_message() {
    let mut parser = SseParser::new();
    let got = events(&mut parser, &[b"data: hello\n\n"]);
    assert_eq!(got[0].event, "message");
}

#[test]
fn empty_data_frame_is_not_emitted() {
    let mut parser = SseParser::new();
    assert!(events(&mut parser, &[b"event: meta\n\n"]).is_empty());
}

#[test]
fn overflow_resets_buffer_and_reports() {
    let mut parser = SseParser::new();
    let oversized = vec![b'x'; MAX_BUFFER_BYTES + 1];
    let got = parser.feed(&oversized);
    assert!(got.is_empty());
    assert!(parser.overflowed());
    // 溢出后不再尝试解析，避免无界内存。
    assert!(parser.feed(b"event: meta\ndata: {}\n\n").is_empty());
}
