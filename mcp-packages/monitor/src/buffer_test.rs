use std::sync::Arc;

use parking_lot::Mutex;
use tokio::io::BufReader;

use super::*;

fn captured(text: &str) -> CapturedEvent {
    CapturedEvent {
        ts: "2026-11-01T00:00:00Z".into(),
        stream: "stdout",
        kind: "line",
        text: text.to_string(),
        bytes: text.len() as u64,
        truncated: false,
        partial: false,
    }
}

#[test]
fn read_is_exclusive_and_next_seq_is_the_cursor() {
    let mut buffer = EventBuffer::new(10, MIN_TASK_BYTES);
    buffer.push(captured("one"));
    buffer.push(captured("two"));

    let first = buffer.read(None, 10, MAX_READ_BYTES);
    assert_eq!(first.events.len(), 2);
    assert_eq!(first.events[0].seq, 1);
    assert_eq!(first.events[1].seq, 2);
    assert_eq!(first.next_seq, 2);
    assert!(!first.truncated);
    assert_eq!(first.dropped, 0);

    let again = buffer.read(Some(first.next_seq), 10, MAX_READ_BYTES);
    assert!(again.events.is_empty());
    assert_eq!(again.next_seq, 2);

    buffer.push(captured("three"));
    let third = buffer.read(Some(2), 10, MAX_READ_BYTES);
    assert_eq!(third.events.len(), 1);
    assert_eq!(third.events[0].seq, 3);
    assert_eq!(third.next_seq, 3);
}

#[test]
fn empty_buffer_starts_at_zero_and_does_not_skip_first_event() {
    let mut buffer = EventBuffer::new(10, MIN_TASK_BYTES);
    let empty = buffer.read(None, 10, MAX_READ_BYTES);
    assert_eq!(empty.next_seq, 0);
    assert!(empty.events.is_empty());

    buffer.push(captured("first"));
    let page = buffer.read(Some(empty.next_seq), 10, MAX_READ_BYTES);
    assert_eq!(page.events.len(), 1);
    assert_eq!(page.events[0].seq, 1);
    assert_eq!(page.next_seq, 1);
}

#[test]
fn read_limit_marks_truncated_and_resumes_from_cursor() {
    let mut buffer = EventBuffer::new(10, MIN_TASK_BYTES);
    for index in 0..3 {
        buffer.push(captured(&format!("line-{index}")));
    }
    let page = buffer.read(None, 2, MAX_READ_BYTES);
    assert_eq!(page.events.len(), 2);
    assert!(page.truncated);
    assert_eq!(page.next_seq, 2);

    let rest = buffer.read(Some(page.next_seq), 2, MAX_READ_BYTES);
    assert_eq!(rest.events.len(), 1);
    assert!(!rest.truncated);
    assert_eq!(rest.next_seq, 3);
}

#[test]
fn read_byte_budget_marks_truncated() {
    let mut buffer = EventBuffer::new(10, MIN_TASK_BYTES);
    buffer.push(captured("aaaa"));
    buffer.push(captured("bbbb"));
    let page = buffer.read(None, 10, 4);
    assert_eq!(page.events.len(), 1);
    assert!(page.truncated);
    assert_eq!(page.next_seq, 1);
}

#[test]
fn read_below_one_event_size_still_returns_one_and_advances() {
    let mut buffer = EventBuffer::new(10, MIN_TASK_BYTES);
    buffer.push(captured("aaaa"));
    buffer.push(captured("bbbb"));

    let first = buffer.read(None, 10, 1);
    assert_eq!(
        first.events.len(),
        1,
        "a too-small byte budget must not return an empty page"
    );
    assert_eq!(first.events[0].text, "aaaa");
    assert!(
        first.truncated,
        "an over-budget single event must be flagged as truncated"
    );
    assert_eq!(first.next_seq, 1);

    let second = buffer.read(Some(first.next_seq), 10, 1);
    assert_eq!(second.events.len(), 1);
    assert_eq!(second.events[0].text, "bbbb");
    assert!(second.truncated);
    assert_eq!(second.next_seq, 2);

    let done = buffer.read(Some(second.next_seq), 10, 1);
    assert!(done.events.is_empty());
    assert!(!done.truncated);
}

#[test]
fn ring_buffer_evicts_oldest_and_reports_gap() {
    let mut buffer = EventBuffer::new(2, MIN_TASK_BYTES);
    buffer.push(captured("one"));
    buffer.push(captured("two"));
    buffer.push(captured("three"));

    assert_eq!(buffer.len(), 2);
    assert_eq!(buffer.dropped(), 1);
    let page = buffer.read(None, 10, MAX_READ_BYTES);
    assert_eq!(page.dropped, 1);
    assert_eq!(page.events.len(), 2);
    assert_eq!(page.events[0].seq, 2);
    assert_eq!(page.events[0].dropped, 0);
    assert_eq!(page.events[1].seq, 3);
    assert_eq!(page.events[1].dropped, 1);
}

fn assert_read_matches_linear_oracle(buffer: &EventBuffer, after_seq: Option<u64>) {
    let expected_events = buffer
        .events
        .iter()
        .filter(|event| after_seq.is_none_or(|after| event.seq > after))
        .cloned()
        .collect::<Vec<_>>();
    let current_max = buffer.events.back().unwrap().seq;
    let expected_next_seq = expected_events
        .last()
        .map(|event| event.seq)
        .unwrap_or_else(|| after_seq.unwrap_or(0).min(current_max));

    let page = buffer.read(after_seq, MAX_READ_EVENTS, MAX_READ_BYTES);
    assert_eq!(page.events, expected_events, "cursor: {after_seq:?}");
    assert_eq!(page.next_seq, expected_next_seq, "cursor: {after_seq:?}");
    assert!(!page.truncated, "cursor: {after_seq:?}");
    assert_eq!(page.dropped, buffer.dropped(), "cursor: {after_seq:?}");
}

#[test]
fn read_cursor_matches_linear_oracle_across_wrapped_ring() {
    let mut buffer = EventBuffer::new(4, MIN_TASK_BYTES);
    for index in 0..32 {
        buffer.push(captured(&format!("line-{index}")));
        if !buffer.events.as_slices().1.is_empty() {
            break;
        }
    }
    assert!(!buffer.events.as_slices().1.is_empty());
    assert!(buffer.dropped() > 0);
    assert_eq!(buffer.len(), 4);

    assert_read_matches_linear_oracle(&buffer, None);
    let last_seq = buffer.events.back().unwrap().seq;
    for after_seq in 0..=last_seq
        .checked_add(1)
        .expect("ordinary fixture must not overflow")
    {
        assert_read_matches_linear_oracle(&buffer, Some(after_seq));
    }
    assert_read_matches_linear_oracle(&buffer, Some(u64::MAX));

    let near_max_sequences = [u64::MAX - 4, u64::MAX - 3, u64::MAX - 2, u64::MAX - 1];
    for (event, seq) in buffer.events.iter_mut().zip(near_max_sequences) {
        event.seq = seq;
    }
    buffer.next_seq = u64::MAX;

    assert!(!buffer.events.as_slices().1.is_empty());
    assert_eq!(
        buffer
            .events
            .iter()
            .map(|event| event.seq)
            .collect::<Vec<_>>(),
        near_max_sequences
    );
    assert_read_matches_linear_oracle(&buffer, None);
    let mut near_max_cursors = vec![
        0,
        u64::MAX - 5,
        u64::MAX - 4,
        u64::MAX - 3,
        u64::MAX - 2,
        u64::MAX - 1,
        u64::MAX,
    ];
    near_max_cursors.sort_unstable();
    near_max_cursors.dedup();
    for after_seq in near_max_cursors {
        assert_read_matches_linear_oracle(&buffer, Some(after_seq));
    }
}

#[test]
fn event_text_is_capped_and_flagged_as_truncated() {
    let mut buffer = EventBuffer::new(10, 64 * 1024);
    let long = "x".repeat(MAX_EVENT_BYTES + 10);
    buffer.push(captured(&long));
    let page = buffer.read(None, 10, MAX_READ_BYTES);
    assert_eq!(page.events[0].text.len(), MAX_EVENT_BYTES);
    assert!(page.events[0].truncated);
    assert_eq!(page.events[0].bytes, long.len() as u64);
}

#[tokio::test]
async fn capture_lines_splits_lines_and_marks_partial_tail() {
    let buffer = Arc::new(Mutex::new(EventBuffer::new(10, MIN_TASK_BYTES)));
    capture_lines(
        BufReader::new(&b"alpha\nbeta\npartial"[..]),
        "stdout",
        Arc::clone(&buffer),
    )
    .await;
    let page = buffer.lock().read(None, 10, MAX_READ_BYTES);
    let texts: Vec<&str> = page
        .events
        .iter()
        .map(|event| event.text.as_str())
        .collect();
    assert_eq!(texts, vec!["alpha", "beta", "partial"]);
    assert!(!page.events[0].partial);
    assert!(!page.events[1].partial);
    assert!(page.events[2].partial);
    assert!(page.events.iter().all(|event| event.untrusted));
}

#[tokio::test]
async fn capture_lines_bounds_a_line_without_newline() {
    let buffer = Arc::new(Mutex::new(EventBuffer::new(10, MIN_TASK_BYTES)));
    let long = vec![b'x'; MAX_EVENT_BYTES + 1024];
    capture_lines(BufReader::new(&long[..]), "stderr", Arc::clone(&buffer)).await;
    let page = buffer.lock().read(None, 10, MAX_READ_BYTES);
    assert_eq!(page.events.len(), 1);
    assert_eq!(page.events[0].text.len(), MAX_EVENT_BYTES);
    assert!(page.events[0].truncated);
    assert!(page.events[0].partial);
}

#[tokio::test]
async fn capture_lines_strips_crlf() {
    let buffer = Arc::new(Mutex::new(EventBuffer::new(10, MIN_TASK_BYTES)));
    capture_lines(
        BufReader::new(&b"windows\r\n"[..]),
        "stdout",
        Arc::clone(&buffer),
    )
    .await;
    let page = buffer.lock().read(None, 10, MAX_READ_BYTES);
    assert_eq!(page.events[0].text, "windows");
    assert_eq!(page.events[0].bytes, 7);
    assert!(!page.events[0].partial);
}
