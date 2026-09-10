use super::*;
use crate::ws_handler::protocol::OutputDecoder;

#[test]
fn split_dsr_cannot_bypass_backpressure_after_a_partial_write() {
    let mut input = InputQueue::default();
    for _ in 0..15 {
        assert!(input.enqueue(b"user-input\n".to_vec()));
    }
    assert!(input.enqueue(b"initial-command\n".to_vec()));
    input.advance(5);
    assert_eq!(input.pending(), b"input\n");

    // PTY output can arrive while its stdin remains blocked. A query split over
    // reads must use the same admission rule as the already queued input.
    let mut decoder = OutputDecoder::default();
    assert!(!decoder.push(b"output\x1b[").1);
    let (text, reply) = decoder.push(b"6n");
    assert_eq!(text, "6n");
    assert!(reply);
    assert!(
        !input.enqueue(b"\x1b[1;1R".to_vec()),
        "DSR overflow must close the connection, not grow the queue"
    );

    // Rejection neither overwrites nor advances the bytes that were already admitted.
    let mut remaining = Vec::new();
    while !input.pending().is_empty() {
        remaining.extend_from_slice(input.pending());
        input.advance(input.pending().len());
    }
    assert_eq!(
        remaining,
        [
            b"input\n".as_slice(),
            &b"user-input\n".repeat(14),
            b"initial-command\n"
        ]
        .concat()
    );
    assert!(
        !decoder.push(b"\x1b[6n\x1b[6n").1,
        "repeated DSR is still answered only once"
    );
}

#[test]
fn late_output_query_and_pending_commands_cannot_reopen_closed_stdin() {
    let mut input = InputQueue::default();
    assert!(input.enqueue(b"partially-written-input".to_vec()));
    input.advance(4);
    input.close();

    let mut decoder = OutputDecoder::default();
    let (_, reply) = decoder.push(b"final output\x1b[6n");
    assert!(
        reply,
        "the late query is real protocol input, not a fabricated flag"
    );
    assert!(input.enqueue(b"\x1b[1;1R".to_vec()));
    assert!(input.enqueue(b"delayed-initial-command\n".to_vec()));
    assert!(input.enqueue(b"late-client-input\n".to_vec()));
    assert!(!input.is_open());
    assert!(
        input.pending().is_empty(),
        "closed stdin must discard every input origin"
    );
}
