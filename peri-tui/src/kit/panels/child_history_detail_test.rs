use super::*;
use serde_json::json;

fn scope(child: &str, reset: u64) -> Scope {
    Scope {
        parent: OperationScope::new("parent", reset),
        selected: format!("instance-{child}"),
        child: child.into(),
    }
}

fn page(child: &str, text: &str) -> Value {
    json!({
        "scope": "child", "parentSessionId": "parent", "sessionId": child,
        "sessionGeneration": "generation", "structureVersion": 1,
        "snapshotToken": "snapshot", "readTicket": "ticket",
        "committedTail": "fixed-tail",
        "beforeCursor": "before", "afterCursor": "after",
        "items": [{
            "entryId": format!("{child}:message"), "source": "own", "position": 1,
            "subItems": [{
                "position": 0, "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {"type": "text", "text": text}
                }
            }]
        }]
    })
}

fn opened() -> ChildHistoryView {
    let mut view = ChildHistoryView::default();
    view.reset(scope("child", 1));
    let (ticket, request) = view.begin(Action::Open).unwrap();
    assert!(view.finish(&ticket, &request, Ok(page("child", "saved"))));
    assert!(view.error.is_none());
    view
}

#[test]
fn selected_child_reset_and_close_reject_old_responses() {
    let mut view = ChildHistoryView::default();
    view.reset(scope("a", 1));
    let (old, request) = view.begin(Action::Open).unwrap();
    assert!(view.begin(Action::Open).is_none(), "one in-flight request");
    view.reset(scope("b", 1));
    assert!(old.is_cancelled());
    assert!(!view.finish(&old, &request, Ok(page("a", "old secret"))));
    assert!(view.page.is_none());

    let (old, request) = view.begin(Action::Open).unwrap();
    view.reset(scope("b", 2));
    assert!(!view.finish(&old, &request, Ok(page("b", "old reset"))));
    let (closed, request) = view.begin(Action::Open).unwrap();
    view.close();
    assert!(!view.finish(&closed, &request, Ok(page("b", "closed"))));
    assert!(view.page.is_none());
    assert!(!view.enabled);
}

#[test]
fn history_pages_replace_previous_bodies_and_keep_snapshot_fixed() {
    let mut view = opened();
    for index in 0..100 {
        let old = Arc::downgrade(&view.page.as_ref().unwrap().text);
        let (ticket, request) = view.begin(Action::Older).unwrap();
        let marker = format!("page-{index}:{}", "x".repeat(32768));
        assert!(view.finish(&ticket, &request, Ok(page("child", &marker))));
        assert!(
            old.upgrade().is_none(),
            "previous page body must be released"
        );
        let stored = &view.page.as_ref().unwrap().text;
        assert!(stored.contains(&marker));
        assert!(
            stored.len() < 128 * 1024,
            "one page, not a growing transcript"
        );
    }
    let (ticket, request) = view.begin(Action::Older).unwrap();
    let mut changed = page("child", "other snapshot");
    changed["structureVersion"] = json!(2);
    view.finish(&ticket, &request, Ok(changed));
    assert!(view.error.is_some());
    assert!(!view.page.as_ref().unwrap().text.contains("other snapshot"));
}

#[test]
fn wrong_parent_child_and_oversize_pages_are_rejected() {
    for invalid in ["parent", "child", "entry", "bytes"] {
        let mut view = opened();
        let (ticket, request) = view.begin(Action::Open).unwrap();
        let mut raw = page("child", "untrusted");
        match invalid {
            "parent" => raw["parentSessionId"] = json!("other"),
            "child" => raw["sessionId"] = json!("other"),
            "entry" => raw["items"][0]["entryId"] = json!("other:message"),
            "bytes" => {
                raw["items"][0]["subItems"][0]["update"]["content"]["text"] =
                    json!("x".repeat(TEXT_BYTES + 1));
            }
            _ => unreachable!(),
        }
        view.finish(&ticket, &request, Ok(raw));
        assert!(view.error.is_some(), "{invalid}");
        assert!(view.page.as_ref().unwrap().text.contains("saved"));
    }
}

fn descriptor_page() -> Value {
    let mut raw = page("child", "");
    raw["items"] = json!([]);
    raw["bodyDescriptors"] = json!([{
        "entryId": "child:large", "source": "own",
        "totalBytes": 10_000_000, "chunkSize": CHUNK_BYTES,
        "readTicket": "ticket", "snapshotToken": "snapshot"
    }]);
    raw
}

fn body_response(offset: u64, text: &[u8], complete: bool) -> Value {
    json!({
        "scope": "child", "parentSessionId": "parent", "childThreadId": "child",
        "snapshotToken": "snapshot", "format": "display_text",
        "committedTail": "fixed-tail",
        "entryId": "child:large", "source": "own", "offset": offset, "bytes": text,
        "nextOffset": if complete { None } else { Some(offset + text.len() as u64) },
        "complete": complete
    })
}

#[test]
fn body_chunks_are_bounded_replace_previous_and_reject_raw_envelopes() {
    let mut view = opened();
    let (ticket, request) = view.begin(Action::Open).unwrap();
    view.finish(&ticket, &request, Ok(descriptor_page()));
    assert!(view.error.is_none());
    let (ticket, request) = view.begin(Action::Body).unwrap();
    let mut unsafe_raw = body_response(0, b"provider replay secret", true);
    unsafe_raw.as_object_mut().unwrap().remove("format");
    view.finish(&ticket, &request, Ok(unsafe_raw));
    assert!(view.body.is_none());
    assert!(view.error.is_some());

    let (ticket, request) = view.begin(Action::Body).unwrap();
    view.finish(&ticket, &request, Ok(body_response(0, b"first", false)));
    assert_eq!(view.body.as_ref().unwrap().text.as_ref(), "first");
    let (ticket, request) = view.begin(Action::NextChunk).unwrap();
    view.finish(&ticket, &request, Ok(body_response(5, b"second", true)));
    assert_eq!(view.body.as_ref().unwrap().text.as_ref(), "second");
    assert!(view.begin(Action::NextChunk).is_none());

    view.back_to_page();
    assert!(view.body.is_none());
    assert!(view.page.is_some());
    let (ticket, request) = view.begin(Action::Body).unwrap();
    let mut wrong = body_response(0, b"wrong", true);
    wrong["snapshotToken"] = json!("old");
    view.finish(&ticket, &request, Ok(wrong));
    assert!(view.body.is_none());
    assert!(view.error.is_some());
}

#[test]
fn unicode_chunk_edges_and_wrapping_do_not_expose_terminal_controls() {
    let mut lines = Vec::new();
    wrap("中中A\u{1b}[31m", 3, &mut lines);
    assert!(lines.iter().all(|line| line.width() <= 3));
    assert!(
        lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .all(|span| !span.content.contains('\u{1b}'))
    );
}

#[test]
fn canonical_history_formatting_keeps_user_tool_and_reasoning() {
    let mut raw = page("child", "answer");
    let sub_items = raw["items"][0]["subItems"].as_array_mut().unwrap();
    sub_items.push(json!({"position": 1, "update": {
        "sessionUpdate": "user_message_chunk", "content": {"type": "text", "text": "question"}
    }}));
    sub_items.push(json!({"position": 2, "update": {
        "sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "reasoning"}
    }}));
    sub_items.push(json!({"position": 3, "update": {
        "sessionUpdate": "tool_call_update", "toolCallId": "call", "status": "failed",
        "rawOutput": "visible failure"
    }}));
    let result = Page::from_response(decode_window_page_response(&raw).unwrap()).unwrap();
    for expected in ["question", "reasoning", "answer", "visible failure"] {
        assert!(result.text.contains(expected), "{expected}");
    }
    assert!(
        result
            .text
            .contains(&i18n::tr("subagent-history-tool-error"))
    );
}

fn body_opened() -> ChildHistoryView {
    let mut view = opened();
    let (ticket, request) = view.begin(Action::Open).unwrap();
    view.finish(&ticket, &request, Ok(descriptor_page()));
    let (ticket, request) = view.begin(Action::Body).unwrap();
    view.finish(
        &ticket,
        &request,
        Ok(body_response(0, b"saved chunk", false)),
    );
    assert!(view.error.is_none());
    view
}

#[test]
fn page_and_body_cannot_change_the_committed_tail() {
    let mut view = opened();
    let old = Arc::downgrade(&view.page.as_ref().unwrap().text);
    let (ticket, request) = view.begin(Action::Older).unwrap();
    let mut raw = page("child", "new snapshot body");
    raw["committedTail"] = json!("changed-tail");
    view.finish(&ticket, &request, Ok(raw));
    assert!(view.page.is_none());
    assert!(old.upgrade().is_none());
    assert_eq!(
        view.error.as_deref(),
        Some(i18n::tr("subagent-history-invalid-response").as_str())
    );

    let mut view = body_opened();
    let old = Arc::downgrade(&view.body.as_ref().unwrap().text);
    let (ticket, request) = view.begin(Action::NextChunk).unwrap();
    let mut raw = body_response(11, b"new body", true);
    raw["committedTail"] = Value::Null;
    view.finish(&ticket, &request, Ok(raw));
    assert!(view.page.is_none());
    assert!(view.body.is_none());
    assert!(old.upgrade().is_none());
    assert!(view.error.is_some());
}

#[test]
fn typed_invalidation_and_deleted_entries_release_cached_page_and_chunk() {
    for error in [
        HistoryWindowError::WindowInvalidated {
            detail: "rewound".into(),
        },
        HistoryWindowError::AnchorUnavailable {
            detail: "entry deleted".into(),
        },
        HistoryWindowError::SessionNotFound,
        HistoryWindowError::ReadTicketExpired,
        HistoryWindowError::SnapshotMismatch {
            detail: "snapshot changed".into(),
        },
    ] {
        let mut view = body_opened();
        let page = Arc::downgrade(&view.page.as_ref().unwrap().text);
        let body = Arc::downgrade(&view.body.as_ref().unwrap().text);
        let mut cache = super::super::render_cache::DetailRenderCache::default();
        let (header, text) = view.presentation(80);
        cache.prepare_text(header, text, 80);
        let (ticket, request) = view.begin(Action::NextChunk).unwrap();
        let failure = AcpError::new(i64::from(error.code()), "typed invalidation")
            .with_data(json!({"reason": error.reason()}));
        view.finish(&ticket, &request, Err(ReadFailure::Remote(failure)));
        assert!(view.page.is_none() && view.body.is_none(), "{error}");
        let (header, text) = view.presentation(80);
        cache.prepare_text(header, text, 80);
        assert!(
            page.upgrade().is_none() && body.upgrade().is_none(),
            "{error}"
        );
        assert!(view.error.as_ref().unwrap().contains("typed invalidation"));
        assert!(view.begin(Action::NextChunk).is_none());
        assert!(view.begin(Action::Open).is_some());
    }
}

#[test]
fn transient_failure_keeps_valid_snapshot_and_never_guesses_from_message_text() {
    for failure in [
        ReadFailure::Disconnected,
        ReadFailure::Remote(AcpError::new(-32603, "connection temporarily unavailable")),
        ReadFailure::Remote(
            AcpError::new(-32603, "backend read temporarily failed")
                .with_data(json!({"reason": "history_read_failed"})),
        ),
        ReadFailure::Remote(AcpError::new(-32012, "history_window_invalidated")),
        ReadFailure::Remote(
            AcpError::new(-32603, "wrong code")
                .with_data(json!({"reason": "history_window_invalidated"})),
        ),
    ] {
        let mut view = body_opened();
        let page = Arc::downgrade(&view.page.as_ref().unwrap().text);
        let body = Arc::downgrade(&view.body.as_ref().unwrap().text);
        let (ticket, request) = view.begin(Action::NextChunk).unwrap();
        view.finish(&ticket, &request, Err(failure));
        assert!(page.upgrade().is_some() && body.upgrade().is_some());
        assert_eq!(view.body.as_ref().unwrap().text.as_ref(), "saved chunk");
        assert!(view.error.as_ref().is_some_and(|error| !error.is_empty()));
    }
}

#[test]
fn explicit_reopen_response_releases_old_snapshot() {
    for mut raw in [
        page("child", "discarded"),
        json!({"scope":"child", "parentSessionId":"parent"}),
    ] {
        let mut view = opened();
        let old = Arc::downgrade(&view.page.as_ref().unwrap().text);
        let (ticket, request) = view.begin(Action::Older).unwrap();
        raw["rewindInvalidation"] = json!({"reopenRequired": true});
        view.finish(&ticket, &request, Ok(raw));
        assert!(view.page.is_none());
        assert!(old.upgrade().is_none());
        assert!(view.error.is_some());
    }
}

#[test]
fn old_typed_invalidation_cannot_erase_a_new_child_snapshot() {
    let mut view = opened();
    let (old, old_request) = view.begin(Action::Older).unwrap();
    view.reset(scope("other", 1));
    let (ticket, request) = view.begin(Action::Open).unwrap();
    view.finish(&ticket, &request, Ok(page("other", "new child body")));
    let error = HistoryWindowError::WindowInvalidated {
        detail: "old child rewound".into(),
    };
    let failure = AcpError::new(i64::from(error.code()), error.to_string())
        .with_data(json!({"reason": error.reason()}));
    assert!(!view.finish(&old, &old_request, Err(ReadFailure::Remote(failure))));
    assert!(view.page.as_ref().unwrap().text.contains("new child body"));
    assert!(view.error.is_none());
}
