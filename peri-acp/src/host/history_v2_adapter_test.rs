//! History V2 own 适配器的真实 SQLite 行为（H3a1）。
//!
//! 复用父模块的 `PageFixture`（真实门面 + 临时目录 SQLite）；用例只驱动
//! `HistoryV2Backend` 端口，不经过 wire，以便直接断言快照 tail、cursor 与 descriptor。

use super::*;
use peri_acp_types::history_v2::{
    DecimalU64, HistoryAnchor, HistoryBodyDescriptor, HistoryBoundary, HistoryCoverage,
    HistoryDirection, HistoryDurabilityState, HistoryEntryAnchor, HistoryOrigin,
    HistoryProgressReason, HistoryRepresentation, HistorySource as V2Source, HistorySubItem,
    HistoryV2Backend, HISTORY_V2_PROJECTION_VERSION,
};
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, TrustedSystemReminderFactory,
};
use peri_acp_types::workspace::ResolvedWorkspace;

use crate::host::history_v2_adapter::HistoryV2BackendAdapter;

fn adapter(fixture: &PageFixture) -> HistoryV2BackendAdapter {
    HistoryV2BackendAdapter::new(
        Arc::clone(&fixture.cfg.session_resources),
        PeriCaps::default(),
    )
}

async fn append_tool(fixture: &PageFixture) {
    fixture
        .cfg
        .session_resources
        .append_history(
            &fixture.sid,
            &[PersistedPayload::Message(BaseMessage::tool_result(
                "call-1",
                "tool output",
            ))],
        )
        .await
        .unwrap();
}

async fn append_payload(fixture: &PageFixture, payload: PersistedPayload) -> MessageId {
    let id = payload.id();
    fixture
        .cfg
        .session_resources
        .append_history(&fixture.sid, &[payload])
        .await
        .unwrap();
    id
}

fn entry_message_id(entry_id: &str) -> MessageId {
    let (_, message) = entry_id.rsplit_once(':').expect("entry id");
    uuid::Uuid::parse_str(message).map(MessageId::from).unwrap()
}

fn all_sub_item_kinds(entry: &peri_acp_types::history_v2::HistoryEntry) -> Vec<&'static str> {
    entry
        .sub_items
        .iter()
        .map(|item| match item {
            HistorySubItem::Text(_) => "text",
            HistorySubItem::Reasoning(_) => "reasoning",
            HistorySubItem::ToolCall(_) => "toolCall",
            HistorySubItem::ToolResult(_) => "toolResult",
            HistorySubItem::Reminder(_) => "reminder",
            HistorySubItem::Summary(_) => "summary",
            HistorySubItem::Attachment(_) => "attachment",
            HistorySubItem::Opaque(_) => "opaque",
        })
        .collect()
}

#[tokio::test]
#[serial]
async fn open_latest_reports_tail_cursors_and_ordered_entries() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    append_tool(&fixture).await;
    let backend = adapter(&fixture);

    let opened = backend
        .open(&fixture.sid, &HistoryAnchor::Latest, 3, 0, 64 * 1024)
        .await
        .expect("open latest");

    assert!(!opened.page.entries.is_empty());
    let tail = opened
        .committed_tail
        .clone()
        .expect("non-empty history tail");
    assert_eq!(opened.history_revision, DecimalU64::new(tail.position));
    assert_eq!(opened.page.history_revision, opened.history_revision);
    assert_eq!(opened.page.committed_tail, opened.committed_tail);
    assert_eq!(opened.projection_version, HISTORY_V2_PROJECTION_VERSION);
    assert!(
        opened.page.before_cursor.is_some(),
        "earlier entries remain"
    );
    assert!(
        opened.page.after_cursor.is_none(),
        "latest page is at the tail"
    );

    let ordinals: Vec<u64> = opened
        .page
        .entries
        .iter()
        .map(|entry| entry.order.ordinal.get())
        .collect();
    let mut sorted = ordinals.clone();
    sorted.sort_unstable();
    assert_eq!(ordinals, sorted, "entries must be in historical order");

    let last = opened.page.entries.last().unwrap();
    assert_eq!(last.durability.state, HistoryDurabilityState::Sealed);
    assert_eq!(last.durability.origin, HistoryOrigin::Live);
    assert_eq!(
        last.durability.final_item_seq,
        Some(last.order.ordinal),
        "own durability must mirror the storage position"
    );
    assert!(
        all_sub_item_kinds(last).contains(&"toolResult"),
        "the tool message projects a tool result: {:?}",
        all_sub_item_kinds(last)
    );

    match &opened.page.coverage {
        HistoryCoverage::Interval(interval) => {
            assert!(matches!(interval.from, HistoryBoundary::Position(_)));
            assert!(matches!(interval.to, HistoryBoundary::Tail));
        }
        other => panic!("expected interval coverage, got {other:?}"),
    }
    assert_eq!(opened.page.progress.reason, HistoryProgressReason::Complete);
    assert_eq!(
        opened.page.progress.consumed_entries,
        opened.page.entries.len() as u64
    );
}

#[tokio::test]
#[serial]
async fn page_walks_before_and_after_without_duplicates() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    append_tool(&fixture).await;
    let backend = adapter(&fixture);
    let opened = backend
        .open(&fixture.sid, &HistoryAnchor::Latest, 2, 0, 64 * 1024)
        .await
        .unwrap();
    let snapshot = snapshot_of(&opened, &fixture.sid);
    let latest_page = opened.page;

    let before_cursor = latest_page.before_cursor.clone().expect("earlier cursor");
    let older = backend
        .page(&snapshot, &before_cursor, HistoryDirection::Before, 2)
        .await
        .expect("before page");
    assert_eq!(older.entries.len(), 2);
    assert!(older.before_cursor.is_none(), "history head reached");
    assert!(older.after_cursor.is_some(), "newer direction remains");
    let older_ids: Vec<&str> = older.entries.iter().map(|e| e.entry_id.as_str()).collect();
    let latest_ids: Vec<&str> = latest_page
        .entries
        .iter()
        .map(|e| e.entry_id.as_str())
        .collect();
    assert!(
        older_ids.iter().all(|id| !latest_ids.contains(id)),
        "the older page must not repeat the latest page"
    );
    assert!(
        older.entries.last().unwrap().order.ordinal < latest_page.entries[0].order.ordinal,
        "the older page stays strictly before the latest page"
    );

    let after_cursor = older.after_cursor.clone().expect("newer cursor");
    let newer = backend
        .page(&snapshot, &after_cursor, HistoryDirection::After, 2)
        .await
        .expect("after page");
    assert_eq!(newer.entries.len(), 2);
    assert!(newer.after_cursor.is_none(), "tail reached");
    let newer_ids: Vec<&str> = newer.entries.iter().map(|e| e.entry_id.as_str()).collect();
    let unique: std::collections::HashSet<&&str> = newer_ids.iter().collect();
    assert_eq!(unique.len(), newer_ids.len(), "no duplicate inside a page");
    assert_eq!(
        newer_ids, latest_ids,
        "after page returns the fixed tail range"
    );
}

#[tokio::test]
#[serial]
async fn seek_places_the_anchor_once_with_both_neighbors() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    append_tool(&fixture).await;
    let backend = adapter(&fixture);
    let opened = backend
        .open(&fixture.sid, &HistoryAnchor::Latest, 4, 0, 64 * 1024)
        .await
        .unwrap();
    let snapshot = snapshot_of(&opened, &fixture.sid);
    let anchor_entry = opened.page.entries[1].clone();
    let anchor = HistoryEntryAnchor {
        entry_id: anchor_entry.entry_id.clone(),
        source: V2Source::Own,
    };

    let sought = backend.seek(&snapshot, &anchor, 1, 1).await.expect("seek");
    assert_eq!(sought.entries.len(), 3);
    let hits = sought
        .entries
        .iter()
        .filter(|entry| entry.entry_id == anchor.entry_id)
        .count();
    assert_eq!(hits, 1, "anchor must appear exactly once");
    assert_eq!(sought.entries[1].entry_id, anchor.entry_id);
    assert!(
        sought.entries[0].order.ordinal < anchor_entry.order.ordinal
            && sought.entries[2].order.ordinal > anchor_entry.order.ordinal
    );
}

async fn anchor_setup(
    fixture: &PageFixture,
) -> (
    HistoryV2BackendAdapter,
    peri_acp_types::history_v2::HistorySnapshotRef,
    HistoryEntryAnchor,
    u64,
) {
    append_tool(fixture).await;
    let backend = adapter(fixture);
    let opened = backend
        .open(&fixture.sid, &HistoryAnchor::Latest, 4, 0, 64 * 1024)
        .await
        .unwrap();
    let anchor_entry = opened.page.entries[1].clone();
    let snapshot = snapshot_of(&opened, &fixture.sid);
    let anchor = HistoryEntryAnchor {
        entry_id: anchor_entry.entry_id.clone(),
        source: V2Source::Own,
    };
    (backend, snapshot, anchor, anchor_entry.order.ordinal.get())
}

fn assert_anchor_window(
    entries: &[peri_acp_types::history_v2::HistoryEntry],
    anchor: &str,
    ordinal: u64,
) {
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry.entry_id == anchor)
            .count(),
        1,
        "anchor must appear exactly once"
    );
    let hits: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.entry_id == anchor)
        .map(|(index, _)| index)
        .collect();
    assert_eq!(hits.len(), 1);
    if hits[0] > 0 {
        assert!(entries[hits[0] - 1].order.ordinal.get() < ordinal);
    }
    if hits[0] + 1 < entries.len() {
        assert!(entries[hits[0] + 1].order.ordinal.get() > ordinal);
    }
}

#[tokio::test]
#[serial]
async fn open_entry_before_one_after_one_returns_both_neighbors() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let (backend, _snapshot, anchor, ordinal) = anchor_setup(&fixture).await;

    let opened = backend
        .open(
            &fixture.sid,
            &HistoryAnchor::Entry(anchor.clone()),
            1,
            1,
            64 * 1024,
        )
        .await
        .expect("open entry");
    assert_eq!(opened.page.entries.len(), 3);
    assert_anchor_window(&opened.page.entries, &anchor.entry_id, ordinal);
}

#[tokio::test]
#[serial]
async fn open_entry_before_zero_after_one_returns_anchor_and_next() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let (backend, _snapshot, anchor, ordinal) = anchor_setup(&fixture).await;

    let opened = backend
        .open(
            &fixture.sid,
            &HistoryAnchor::Entry(anchor.clone()),
            0,
            1,
            64 * 1024,
        )
        .await
        .expect("open entry");
    assert_eq!(opened.page.entries.len(), 2);
    assert_anchor_window(&opened.page.entries, &anchor.entry_id, ordinal);
    assert_eq!(opened.page.entries[0].entry_id, anchor.entry_id);
}

#[tokio::test]
#[serial]
async fn open_entry_before_one_after_zero_returns_previous_and_anchor() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let (backend, _snapshot, anchor, ordinal) = anchor_setup(&fixture).await;

    let opened = backend
        .open(
            &fixture.sid,
            &HistoryAnchor::Entry(anchor.clone()),
            1,
            0,
            64 * 1024,
        )
        .await
        .expect("open entry");
    assert_eq!(opened.page.entries.len(), 2);
    assert_anchor_window(&opened.page.entries, &anchor.entry_id, ordinal);
    assert_eq!(opened.page.entries[1].entry_id, anchor.entry_id);
}

#[tokio::test]
#[serial]
async fn seek_before_one_after_one_returns_both_neighbors() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let (backend, snapshot, anchor, ordinal) = anchor_setup(&fixture).await;

    let sought = backend.seek(&snapshot, &anchor, 1, 1).await.expect("seek");
    assert_eq!(sought.entries.len(), 3);
    assert_anchor_window(&sought.entries, &anchor.entry_id, ordinal);
}

#[tokio::test]
#[serial]
async fn seek_before_zero_after_one_returns_anchor_and_next() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let (backend, snapshot, anchor, ordinal) = anchor_setup(&fixture).await;

    let sought = backend.seek(&snapshot, &anchor, 0, 1).await.expect("seek");
    assert_eq!(sought.entries.len(), 2);
    assert_anchor_window(&sought.entries, &anchor.entry_id, ordinal);
    assert_eq!(sought.entries[0].entry_id, anchor.entry_id);
}

#[tokio::test]
#[serial]
async fn seek_before_one_after_zero_returns_previous_and_anchor() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let (backend, snapshot, anchor, ordinal) = anchor_setup(&fixture).await;

    let sought = backend.seek(&snapshot, &anchor, 1, 0).await.expect("seek");
    assert_eq!(sought.entries.len(), 2);
    assert_anchor_window(&sought.entries, &anchor.entry_id, ordinal);
    assert_eq!(sought.entries[1].entry_id, anchor.entry_id);
}

#[tokio::test]
#[serial]
async fn oversized_entry_is_opaque_and_body_returns_projected_text() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let text = "x".repeat(70 * 1024);
    let huge_id = append_payload(
        &fixture,
        PersistedPayload::Message(BaseMessage::ai(text.clone())),
    )
    .await;
    let backend = adapter(&fixture);

    let opened = backend
        .open(&fixture.sid, &HistoryAnchor::Latest, 2, 0, 64 * 1024)
        .await
        .expect("open latest with an oversized entry");
    assert!(
        backend.visibility_read_max_bytes().is_empty(),
        "ordinary assistant rows are visible from the role column without a prefix read"
    );
    let entry = opened
        .page
        .entries
        .iter()
        .find(|entry| entry.entry_id.ends_with(&huge_id.as_uuid().to_string()))
        .expect("oversized entry present");
    assert_eq!(all_sub_item_kinds(entry), ["opaque"]);
    let HistorySubItem::Opaque(opaque) = &entry.sub_items[0] else {
        panic!("expected opaque subitem");
    };
    let descriptor: &HistoryBodyDescriptor = &opaque.body;
    assert_eq!(
        descriptor.representation,
        HistoryRepresentation::DisplayText
    );
    assert_eq!(descriptor.media_type, "text/plain; charset=utf-8");
    assert!(descriptor.total_bytes.get() > 64 * 1024);
    assert_eq!(descriptor.chunk_bytes, 64 * 1024);

    let canonical = expected_payload_bytes(&fixture, huge_id).await;
    assert_eq!(
        descriptor.total_bytes.get(),
        canonical.len() as u64,
        "page descriptor still carries the canonical length as an estimate"
    );

    let snapshot = snapshot_of(&opened, &fixture.sid);
    let first = backend
        .body(&snapshot, &descriptor.body_ref, 0, 64 * 1024)
        .await
        .expect("first body chunk");
    assert_eq!(first.representation, HistoryRepresentation::DisplayText);
    assert_eq!(first.media_type, "text/plain; charset=utf-8");
    assert_eq!(
        first.encoding,
        peri_acp_types::history_v2::HistoryEncoding::Base64
    );
    assert_eq!(first.offset, DecimalU64::ZERO);
    assert_eq!(
        first.total_bytes,
        DecimalU64::new(text.len() as u64),
        "body totalBytes is the projected display length"
    );
    assert!(!first.complete);
    assert_eq!(first.next_offset, Some(DecimalU64::new(64 * 1024)));
    assert_eq!(first.content_version.len(), 32);
    assert_ne!(first.content_version, "1");
    let first_bytes = decode_base64(&first.data);
    assert_eq!(first_bytes, text.as_bytes()[..64 * 1024]);

    let second = backend
        .body(&snapshot, &descriptor.body_ref, 64 * 1024, 64 * 1024)
        .await
        .expect("second body chunk");
    assert!(second.complete);
    assert!(second.next_offset.is_none());
    assert_eq!(second.total_bytes, first.total_bytes);
    assert_eq!(
        second.content_version, first.content_version,
        "same canonical content must keep one stable version"
    );
    let second_bytes = decode_base64(&second.data);
    assert_eq!(second_bytes, text.as_bytes()[64 * 1024..]);

    let mut joined = first_bytes;
    joined.extend_from_slice(&second_bytes);
    assert_eq!(joined, text.as_bytes(), "分块拼回必须等于投影文本");
    let joined = String::from_utf8(joined).unwrap();
    assert!(!joined.contains("\"role\":\"assistant\""));
    assert!(!joined.contains("\"version\":"));
    assert!(!joined.contains("\"type\":\"message\""));
    assert!(
        !joined.contains("canonical"),
        "body must not leak the canonical envelope"
    );
}

#[tokio::test]
#[serial]
async fn oversized_system_message_is_not_projected() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let system_id = append_payload(
        &fixture,
        PersistedPayload::Message(BaseMessage::system("s".repeat(70 * 1024))),
    )
    .await;
    let backend = adapter(&fixture);

    let opened = backend
        .open(&fixture.sid, &HistoryAnchor::Latest, 2, 0, 64 * 1024)
        .await
        .unwrap();
    assert!(
        opened.page.entries.is_empty(),
        "oversized system rows must not be projected"
    );
    assert!(opened
        .page
        .entries
        .iter()
        .all(|entry| !entry.entry_id.ends_with(&system_id.as_uuid().to_string())));
    assert!(
        backend.visibility_read_max_bytes().is_empty(),
        "role=system is rejected without reading content"
    );
}

#[tokio::test]
#[serial]
async fn oversized_model_only_reminder_is_not_projected() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    append_payload(
        &fixture,
        PersistedPayload::SystemReminder {
            id: MessageId::new(),
            reminder: large_reminder(vec![ReminderAudience::Model]),
        },
    )
    .await;
    let backend = adapter(&fixture);

    let opened = backend
        .open(&fixture.sid, &HistoryAnchor::Latest, 2, 0, 64 * 1024)
        .await
        .unwrap();
    assert!(
        opened.page.entries.is_empty(),
        "model-only reminders must not issue an opaque descriptor"
    );
    assert_eq!(
        backend.visibility_read_max_bytes(),
        vec![1024],
        "visibility prefix read is bounded to 1024 bytes"
    );
}

#[tokio::test]
#[serial]
async fn oversized_tui_reminder_is_opaque_and_body_readable() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    append_payload(
        &fixture,
        PersistedPayload::SystemReminder {
            id: MessageId::new(),
            reminder: large_reminder(vec![ReminderAudience::Tui]),
        },
    )
    .await;
    let backend = adapter(&fixture);

    let opened = backend
        .open(&fixture.sid, &HistoryAnchor::Latest, 2, 0, 64 * 1024)
        .await
        .unwrap();
    let entry = opened
        .page
        .entries
        .iter()
        .find(|entry| all_sub_item_kinds(entry) == ["opaque"])
        .expect("tui reminder must remain visible");
    let HistorySubItem::Opaque(opaque) = &entry.sub_items[0] else {
        panic!("expected opaque");
    };
    assert_eq!(
        backend.visibility_read_max_bytes(),
        vec![1024],
        "visibility prefix read is bounded to 1024 bytes"
    );

    let snapshot = snapshot_of(&opened, &fixture.sid);
    let first = backend
        .body(&snapshot, &opaque.body.body_ref, 0, 1024)
        .await
        .expect("reminder body prefix");
    assert!(!first.complete);
    assert_eq!(first.next_offset, Some(DecimalU64::new(1024)));
    assert_eq!(
        first.representation,
        HistoryRepresentation::DisplayText,
        "body must be projected display text"
    );
    let first_text = String::from_utf8(decode_base64(&first.data)).expect("utf8");
    assert!(first_text.contains("提醒："));
    assert!(!first_text.contains("\"type\":\"system_reminder\""));
    assert!(!first_text.contains("\"audiences\""));

    let rest = backend
        .body(&snapshot, &opaque.body.body_ref, 1024, 64 * 1024)
        .await
        .expect("reminder body rest");
    assert!(rest.complete);
    let second_text = String::from_utf8(decode_base64(&rest.data)).expect("utf8");
    let joined = format!("{first_text}{second_text}");
    assert!(joined.contains(&"r".repeat(1024)));
    assert_eq!(
        first.total_bytes.get(),
        joined.len() as u64,
        "totalBytes is the projected text length"
    );
    assert_eq!(rest.total_bytes, first.total_bytes);
}

#[tokio::test]
#[serial]
async fn oversized_assistant_message_is_opaque_without_prefix_read() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    append_payload(
        &fixture,
        PersistedPayload::Message(BaseMessage::ai("a".repeat(70 * 1024))),
    )
    .await;
    let backend = adapter(&fixture);

    let opened = backend
        .open(&fixture.sid, &HistoryAnchor::Latest, 2, 0, 64 * 1024)
        .await
        .unwrap();
    assert!(
        opened
            .page
            .entries
            .iter()
            .any(|entry| all_sub_item_kinds(entry) == ["opaque"]),
        "ordinary assistant rows stay visible"
    );
    assert!(
        backend.visibility_read_max_bytes().is_empty(),
        "role=assistant is visible without reading content"
    );
}

#[tokio::test]
#[serial]
async fn append_stays_outside_the_fixed_tail_and_tail_removal_invalidates() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    append_tool(&fixture).await;
    let backend = adapter(&fixture);
    let opened = backend
        .open(&fixture.sid, &HistoryAnchor::Latest, 2, 0, 64 * 1024)
        .await
        .unwrap();
    let snapshot = snapshot_of(&opened, &fixture.sid);
    let before = opened.page.before_cursor.clone().expect("earlier cursor");
    let appended_id = append_payload(
        &fixture,
        PersistedPayload::Message(BaseMessage::human("append after open")),
    )
    .await;

    // 设计 §9.2：append 不使旧窗口失效；新行不进入固定 tail。
    let page = backend
        .page(&snapshot, &before, HistoryDirection::Before, 2)
        .await
        .expect("append keeps the old snapshot valid");
    assert!(
        page.entries
            .iter()
            .all(|entry| !entry.entry_id.ends_with(&appended_id.as_uuid().to_string())),
        "appended content must not leak into the fixed snapshot"
    );
    assert_eq!(page.committed_tail, snapshot.committed_tail);

    // 删除 tail 行（rewind/精确删除的真实失效路径）后，固定 tail 复核失败。
    let tail_message = entry_message_id(&snapshot.committed_tail.as_ref().expect("tail").entry_id);
    remove_message(&fixture, tail_message).await;
    let error = backend
        .page(&snapshot, &before, HistoryDirection::Before, 2)
        .await
        .expect_err("tail removal must invalidate the snapshot");
    assert_eq!(
        error,
        peri_acp_types::history_v2::HistoryBackendError::SnapshotInvalidated
    );
}

#[tokio::test]
#[serial]
async fn system_and_model_only_entries_are_not_projected() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let system_id = append_payload(
        &fixture,
        PersistedPayload::Message(BaseMessage::system("hidden system prompt")),
    )
    .await;
    append_payload(
        &fixture,
        PersistedPayload::SystemReminder {
            id: MessageId::new(),
            reminder: model_only_reminder(),
        },
    )
    .await;
    let backend = adapter(&fixture);

    let opened = backend
        .open(&fixture.sid, &HistoryAnchor::Latest, 8, 0, 64 * 1024)
        .await
        .expect("open latest");
    assert!(!opened.page.entries.is_empty());
    for entry in &opened.page.entries {
        assert!(
            !entry.entry_id.ends_with(&system_id.as_uuid().to_string()),
            "system entries must not be projected"
        );
        assert!(
            entry
                .sub_items
                .iter()
                .all(|item| !matches!(item, HistorySubItem::Reminder(_))),
            "model-only reminders must not be projected"
        );
    }
}

#[tokio::test]
#[serial]
async fn tool_call_and_result_share_invocation_key_across_entries() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    append_payload(
        &fixture,
        PersistedPayload::Message(BaseMessage::ai_with_tool_calls(
            "calling the shell",
            vec![peri_acp_types::messages::ToolCallRequest::new(
                "call-1",
                "shell",
                json!({"cmd": "ls"}),
            )],
        )),
    )
    .await;
    append_payload(
        &fixture,
        PersistedPayload::Message(BaseMessage::tool_result("call-1", "done")),
    )
    .await;
    let backend = adapter(&fixture);

    let opened = backend
        .open(&fixture.sid, &HistoryAnchor::Latest, 8, 0, 64 * 1024)
        .await
        .expect("open latest");
    let mut call_key = None;
    let mut result_key = None;
    for entry in &opened.page.entries {
        for sub_item in &entry.sub_items {
            match sub_item {
                HistorySubItem::ToolCall(item) => {
                    call_key = Some(item.invocation_key.clone());
                }
                HistorySubItem::ToolResult(item) => {
                    result_key = Some(item.invocation_key.clone());
                }
                _ => {}
            }
        }
    }
    let expected = format!("{}:call-1", fixture.sid);
    assert_eq!(call_key.as_deref(), Some(expected.as_str()));
    assert_eq!(
        result_key.as_deref(),
        Some(expected.as_str()),
        "结果必须与调用使用同一跨行键，不能各自绑定 entryId"
    );
}

#[tokio::test]
#[serial]
async fn body_rejects_system_entry_even_with_its_ref() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let system_id = append_payload(
        &fixture,
        PersistedPayload::Message(BaseMessage::system("s".repeat(70 * 1024))),
    )
    .await;
    let backend = adapter(&fixture);
    let opened = backend
        .open(&fixture.sid, &HistoryAnchor::Latest, 2, 0, 64 * 1024)
        .await
        .unwrap();
    let snapshot = snapshot_of(&opened, &fixture.sid);
    let error = backend
        .body(&snapshot, &system_id.as_uuid().to_string(), 0, 1024)
        .await
        .expect_err("system body must stay hidden");
    assert_eq!(
        error,
        peri_acp_types::history_v2::HistoryBackendError::NotFound
    );
}

#[tokio::test]
#[serial]
async fn body_rejects_model_only_reminder_ref() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let reminder_id = MessageId::new();
    append_payload(
        &fixture,
        PersistedPayload::SystemReminder {
            id: reminder_id,
            reminder: large_reminder(vec![ReminderAudience::Model]),
        },
    )
    .await;
    let backend = adapter(&fixture);
    let opened = backend
        .open(&fixture.sid, &HistoryAnchor::Latest, 2, 0, 64 * 1024)
        .await
        .unwrap();
    let snapshot = snapshot_of(&opened, &fixture.sid);
    let error = backend
        .body(&snapshot, &reminder_id.as_uuid().to_string(), 0, 1024)
        .await
        .expect_err("model-only reminder body must stay hidden");
    assert_eq!(
        error,
        peri_acp_types::history_v2::HistoryBackendError::NotFound
    );
}

#[tokio::test]
#[serial]
async fn empty_session_opens_with_empty_snapshot() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = "empty-history-v2-session";
    create_empty_session(&fixture.cfg, tmp.path(), sid).await;
    let backend = adapter(&fixture);

    let opened = backend
        .open(sid, &HistoryAnchor::Latest, 50, 0, 64 * 1024)
        .await
        .expect("empty open");
    assert!(opened.page.entries.is_empty());
    assert_eq!(opened.committed_tail, None);
    assert_eq!(opened.history_revision, DecimalU64::ZERO);
    assert_eq!(opened.page.coverage, HistoryCoverage::EmptySnapshot);
    assert!(opened.page.before_cursor.is_none());
    assert!(opened.page.after_cursor.is_none());
}

#[tokio::test]
#[serial]
async fn local_sqlite_advertises_history_v2_and_unknown_sessions_fail_closed() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    assert!(fixture.cfg.session_resources.supports_history_v2());
    let backend = adapter(&fixture);
    let error = backend
        .open("missing-session", &HistoryAnchor::Latest, 50, 0, 64 * 1024)
        .await
        .expect_err("unknown session");
    assert_eq!(
        error,
        peri_acp_types::history_v2::HistoryBackendError::NotFound
    );
}

fn snapshot_of(
    opened: &peri_acp_types::history_v2::HistoryOpenedSnapshot,
    session_id: &str,
) -> peri_acp_types::history_v2::HistorySnapshotRef {
    peri_acp_types::history_v2::HistorySnapshotRef {
        session_id: session_id.to_owned(),
        committed_tail: opened.committed_tail.clone(),
        history_revision: opened.history_revision,
        projection_version: opened.projection_version,
        target_bytes: 64 * 1024,
    }
}

fn decode_base64(data: &str) -> Vec<u8> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .unwrap()
}

async fn expected_payload_bytes(fixture: &PageFixture, id: MessageId) -> Vec<u8> {
    let payload = fixture
        .cfg
        .session_resources
        .load_session_snapshot(&fixture.sid)
        .await
        .unwrap()
        .payloads
        .into_iter()
        .find(|payload| payload.id() == id)
        .expect("stored payload");
    peri_acp_types::store::serialize_persisted_payload(&payload)
        .unwrap()
        .into_bytes()
}

async fn remove_message(fixture: &PageFixture, id: MessageId) {
    // 真实失效路径：精确删除只影响存储行；不新增表/列。
    fixture
        .cfg
        .session_resources
        .remove_history_entries(&fixture.sid, &[id])
        .await
        .expect("remove tail row");
}

fn model_only_reminder() -> peri_acp_types::system_reminder::TrustedSystemReminder {
    reminder_with_body("model only", vec![ReminderAudience::Model])
}

fn large_reminder(
    audiences: Vec<ReminderAudience>,
) -> peri_acp_types::system_reminder::TrustedSystemReminder {
    reminder_with_body(&"r".repeat(64 * 1024), audiences)
}

fn reminder_with_body(
    body: &str,
    audiences: Vec<ReminderAudience>,
) -> peri_acp_types::system_reminder::TrustedSystemReminder {
    let reminder = SystemReminder {
        version: 1,
        category: ReminderCategory::Guidance,
        source: ReminderSource("history_v2_test".to_owned()),
        kind: "model_only".to_owned(),
        severity: ReminderSeverity::Info,
        delivery: ReminderDelivery::Configurable,
        audiences: ReminderAudiences(audiences),
        body: body.to_owned(),
        summary: None,
        metadata: serde_json::json!({}),
    };
    TrustedSystemReminderFactory::for_producer()
        .construct(reminder)
        .unwrap()
}

async fn create_empty_session(cfg: &AcpServerConfig, root: &std::path::Path, sid: &str) {
    let workspace: ResolvedWorkspace = cfg.session_resources.resolve_workspace(root).await.unwrap();
    let frozen = cfg
        .session_manager
        .build_frozen_data(workspace.cwd.to_str().unwrap());
    let encoded = crate::session::frozen_snapshot::encode_frozen_snapshot(&frozen).unwrap();
    let input = super::super::bound_input(sid, &workspace, encoded);
    cfg.session_resources.create_session(&input).await.unwrap();
}

// 正文投影与有界读取：单独文件挂载（STD-SIZE-001）。
#[path = "history_v2_adapter_body_test.rs"]
mod body_tests;
