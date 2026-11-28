//! Cursor codec, request admission and page walking for `session/list`.

use std::path::PathBuf;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use peri_acp_types::session_resources::SessionResourceError;
use peri_acp_types::thread::ThreadListEntry;
use peri_acp_types::workspace::{ScopedThreadEntry, ScopedThreadPage};
use serde_json::json;

use crate::transport::types::RequestId;

use super::super::harness::{expect_error, Harness, CWD};
use super::*;

fn timestamp(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .expect("test timestamp")
        .with_timezone(&Utc)
}

fn cursor(text: &str, id: &str) -> ThreadListCursor {
    ThreadListCursor {
        updated_at: timestamp(text),
        thread_id: id.to_owned(),
    }
}

fn entry(id: &str, cwd: &str, text: &str) -> ScopedThreadEntry {
    ScopedThreadEntry {
        thread: ThreadListEntry {
            id: id.to_owned(),
            title: Some(format!("title {id}")),
            cwd: cwd.to_owned(),
            message_count: 1,
            updated_at: timestamp(text),
        },
        binding: None,
        effective_cwd: PathBuf::from(cwd),
        workspace_root: None,
    }
}

fn page(entries: Vec<ScopedThreadEntry>, next: Option<ThreadListCursor>) -> ScopedThreadPage {
    ScopedThreadPage {
        entries,
        next_cursor: next,
    }
}

fn session_ids(page: &Page) -> Vec<String> {
    page.sessions
        .iter()
        .map(|session| session.session_id.0.to_string())
        .collect()
}

#[test]
fn cursor_round_trips_and_rejects_every_other_shape() {
    let position = cursor("2026-11-24T10:00:00.123+00:00", "thread-a");
    let encoded = encode_cursor(&position);
    assert!(
        !encoded.contains('=') && !encoded.contains('+') && !encoded.contains('/'),
        "the cursor must be base64url without padding: {encoded}"
    );
    assert_eq!(decode_cursor(&encoded).expect("round trip"), position);

    let encoded_json = |text: &str| URL_SAFE_NO_PAD.encode(text.as_bytes());
    for invalid in [
        String::new(),
        "not a cursor".to_owned(),
        encoded_json("{}"),
        encoded_json(r#"{"u":"not-a-time","i":"thread-a"}"#),
        encoded_json(r#"{"u":"2026-11-24T10:00:00Z","i":""}"#),
        encoded_json(r#"{"u":"2026-11-24T10:00:00Z"}"#),
    ] {
        let error = decode_cursor(&invalid).expect_err("invalid cursor must be refused");
        assert_eq!(error.code, errors::INVALID_PARAMS, "{invalid}");
    }
}

#[tokio::test]
async fn list_requests_forward_a_decoded_cursor_and_a_clamped_limit() {
    let mut harness = Harness::start();
    harness.negotiate().await;

    let first = RequestId::Number(3000);
    harness
        .request(
            first.clone(),
            "session/list",
            json!({"cwd": CWD, "limit": 0}),
        )
        .await;
    let forwarded = harness.expect_forwarded().await;
    assert_eq!(forwarded.method, super::SESSION_LIST);
    assert_eq!(forwarded.params, json!({"cwd": CWD, "limit": 1}));
    harness
        .respond(
            forwarded.id,
            Ok(json!({"sessions": [], "nextCursor": "opaque"})),
        )
        .await;
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, first);
    assert_eq!(result.expect("list reply")["nextCursor"], "opaque");

    let position = cursor("2026-11-24T10:00:00Z", "thread-a");
    let second = RequestId::Number(3001);
    harness
        .request(
            second.clone(),
            "session/list",
            json!({"cursor": encode_cursor(&position), "limit": 600}),
        )
        .await;
    let forwarded = harness.expect_forwarded().await;
    assert_eq!(forwarded.method, super::SESSION_LIST);
    assert_eq!(forwarded.params["limit"], 500);
    assert_eq!(
        forwarded.params["cursor"],
        json!({"updatedAt": "2026-11-24T10:00:00+00:00", "threadId": "thread-a"})
    );
    harness
        .respond(forwarded.id, Ok(json!({"sessions": []})))
        .await;
    let (answered, result) = harness.expect_out().await.response();
    assert_eq!(answered, second);
    assert!(result.is_ok());
}

#[tokio::test]
async fn invalid_list_parameters_never_reach_the_host() {
    let mut harness = Harness::start();
    harness.negotiate().await;
    for (offset, params) in [
        json!({"cursor": "not a cursor"}),
        json!({"cursor": 5}),
        json!({"cwd": "relative/path"}),
        json!({"limit": "lots"}),
    ]
    .into_iter()
    .enumerate()
    {
        let id = RequestId::Number(3100 + offset as i64);
        harness.request(id.clone(), "session/list", params).await;
        expect_error(&harness, id, -32602).await;
    }
    harness.expect_quiet().await;
}

#[tokio::test]
async fn a_filtered_page_reports_the_last_raw_cursor_and_never_skips_entries() {
    let first = page(
        vec![
            entry("a", "/wanted", "2026-11-24T10:00:00Z"),
            entry("b", "/other", "2026-11-24T09:00:00Z"),
        ],
        Some(cursor("2026-11-24T09:00:00Z", "b")),
    );
    let second = page(vec![entry("c", "/wanted", "2026-11-24T08:00:00Z")], None);
    let mut pages = vec![first, second.clone()].into_iter();
    let mut queries = Vec::new();
    let collected = page_through(Some("/wanted"), None, 1, |query| {
        queries.push(query.clone());
        let page = pages.next().expect("one fetch per page");
        async move { Ok::<_, SessionResourceError>(page) }
    })
    .await
    .expect("page through");
    // The raw page was consumed whole; its cursor (after `b`) is the resume
    // position, so `b` cannot be skipped by the next request.
    assert_eq!(session_ids(&collected), vec!["a"]);
    assert_eq!(
        decode_cursor(collected.next_cursor.as_deref().expect("next cursor"))
            .expect("decodable cursor"),
        cursor("2026-11-24T09:00:00Z", "b")
    );
    assert_eq!(queries.len(), 1);
    assert_eq!(queries[0].limit, 1);

    let mut pages = vec![second].into_iter();
    let rest = page_through(None, None, 1, |_query| {
        let page = pages.next().expect("one fetch per page");
        async move { Ok::<_, SessionResourceError>(page) }
    })
    .await
    .expect("page through");
    assert_eq!(session_ids(&rest), vec!["c"]);
    assert!(rest.next_cursor.is_none());
}

#[tokio::test]
async fn a_filter_that_matches_nothing_consumes_every_page_before_ending() {
    let pages = vec![
        page(
            vec![entry("b", "/other", "2026-11-24T10:00:00Z")],
            Some(cursor("2026-11-24T10:00:00Z", "b")),
        ),
        page(vec![entry("c", "/elsewhere", "2026-11-24T09:00:00Z")], None),
    ];
    let mut pages = pages.into_iter();
    let mut reads = 0;
    let collected = page_through(Some("/wanted"), None, 10, |_query| {
        reads += 1;
        let page = pages.next().expect("one fetch per page");
        async move { Ok::<_, SessionResourceError>(page) }
    })
    .await
    .expect("page through");
    assert!(collected.sessions.is_empty());
    assert!(collected.next_cursor.is_none());
    assert_eq!(reads, 2, "both raw pages were exhausted");
}

#[tokio::test]
async fn a_store_failure_keeps_the_resource_error_mapping() {
    let error = page_through(Some("/wanted"), None, 10, |_query| async {
        Err::<ScopedThreadPage, _>(SessionResourceError::new(
            peri_acp_types::session_resources::SessionResourceErrorKind::Unavailable {
                detail: "store offline".to_owned(),
            },
        ))
    })
    .await
    .expect_err("store failure");
    assert_ne!(error.code, 0);
}
