//! FIX-02：正文定位不依赖页面预算拆分（真实 SQLite canonical 字节）。
//!
//! 构造「before 吃掉大部分预算、after 条目变成 opaque」的 around seek：旧实现会用完整
//! `target_bytes` 重读同一锚点，把页面判定为 `Oversized` 的条目重判成 `Loaded` 后返回
//! `not_found`。这里只驱动 `HistoryV2Backend`，并断言 body 能完整读回。
//!
//! 从 `history_v2_adapter_test.rs` 挂载，`use super::*` 复用真实 SQLite 夹具与 helper。

use super::*;
use peri_acp_types::history_v2::HistoryOpenedSnapshot;

/// 512 KiB 页预算（`HISTORY_WINDOW_DEFAULT_BYTE_BUDGET`）。
const PAGE_BUDGET: u64 = 512 * 1024;
/// 大条目正文规模：canonical 编码后约 200 KiB。
const BIG_TEXT: usize = 200 * 1024;

async fn append_big(fixture: &PageFixture, id_role: bool) -> (MessageId, usize) {
    let text = if id_role {
        format!("assistant:{}", "a".repeat(BIG_TEXT))
    } else {
        format!("user:{}", "u".repeat(BIG_TEXT))
    };
    let payload = if id_role {
        PersistedPayload::Message(BaseMessage::ai(text))
    } else {
        PersistedPayload::Message(BaseMessage::human(text))
    };
    let id = append_payload(fixture, payload).await;
    let bytes = expected_payload_bytes(fixture, id).await.len();
    (id, bytes)
}

async fn append_anchor(fixture: &PageFixture) -> (MessageId, usize) {
    let id = append_payload(
        fixture,
        PersistedPayload::Message(BaseMessage::human("around-seek anchor")),
    )
    .await;
    let bytes = expected_payload_bytes(fixture, id).await.len();
    (id, bytes)
}

async fn append_after(fixture: &PageFixture) -> (MessageId, String, usize) {
    let text = format!("after:{}", "z".repeat(BIG_TEXT));
    let id = append_payload(
        fixture,
        PersistedPayload::Message(BaseMessage::ai(text.clone())),
    )
    .await;
    let bytes = expected_payload_bytes(fixture, id).await.len();
    (id, text, bytes)
}

fn snapshot_with_bytes(
    opened: &HistoryOpenedSnapshot,
    session_id: &str,
    bytes: u64,
) -> peri_acp_types::history_v2::HistorySnapshotRef {
    let mut snapshot = snapshot_of(opened, session_id);
    snapshot.target_bytes = bytes;
    snapshot
}

fn opaque_descriptor(
    opened: &HistoryOpenedSnapshot,
    message_id: MessageId,
) -> HistoryBodyDescriptor {
    let suffix = message_id.as_uuid().to_string();
    let entry = opened
        .page
        .entries
        .iter()
        .find(|entry| entry.entry_id.ends_with(&suffix))
        .unwrap_or_else(|| {
            panic!(
                "opaque entry {suffix} missing; entries={:?}",
                opened
                    .page
                    .entries
                    .iter()
                    .map(|entry| (entry.entry_id.as_str(), all_sub_item_kinds(entry)))
                    .collect::<Vec<_>>()
            )
        });
    assert_eq!(
        all_sub_item_kinds(entry),
        ["opaque"],
        "budget-split entry must stay opaque instead of being reclassified"
    );
    match &entry.sub_items[0] {
        HistorySubItem::Opaque(opaque) => opaque.body.clone(),
        other => panic!("expected opaque subitem, got {other:?}"),
    }
}

async fn read_body_text(
    backend: &HistoryV2BackendAdapter,
    snapshot: &peri_acp_types::history_v2::HistorySnapshotRef,
    descriptor: &HistoryBodyDescriptor,
) -> String {
    let mut bytes = Vec::new();
    let mut offset = 0u64;
    loop {
        let slice = backend
            .body(snapshot, &descriptor.body_ref, offset, 64 * 1024)
            .await
            .expect("body slice");
        assert_eq!(
            slice.representation, descriptor.representation,
            "descriptor/body representation must agree"
        );
        assert_eq!(
            slice.media_type, descriptor.media_type,
            "descriptor/body mediaType must agree"
        );
        let chunk = decode_base64(&slice.data);
        offset += chunk.len() as u64;
        bytes.extend_from_slice(&chunk);
        if slice.complete {
            assert_eq!(offset, slice.total_bytes.get());
            break;
        }
    }
    String::from_utf8(bytes).expect("display text is UTF-8")
}

#[tokio::test]
#[serial]
async fn around_seek_after_entry_is_opaque_and_body_reads_fully() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let (_, before_a) = append_big(&fixture, false).await;
    let (_, before_b) = append_big(&fixture, true).await;
    let (anchor, anchor_bytes) = append_anchor(&fixture).await;
    let (after, after_text, after_bytes) = append_after(&fixture).await;

    // 页预算 512 KiB：before 两条约 400 KiB + 锚点，after 约 200 KiB 落在剩余预算之外，
    // 但小于整页预算（旧 descriptor 重读会把它判成 Loaded）。
    assert!(
        before_a + before_b + anchor_bytes < PAGE_BUDGET as usize,
        "before rows {before_a}+{before_b}+{anchor_bytes} must fit the page budget"
    );
    assert!(
        after_bytes > PAGE_BUDGET as usize - (before_a + before_b + anchor_bytes),
        "after entry must exceed the remaining budget"
    );
    assert!(
        after_bytes <= PAGE_BUDGET as usize,
        "after entry must fit the full page budget, otherwise the old re-read also saw Oversized"
    );

    let backend = adapter(&fixture);
    let opened = backend
        .open(
            &fixture.sid,
            &HistoryAnchor::Entry(HistoryEntryAnchor {
                source: V2Source::Own,
                entry_id: anchor.as_uuid().to_string(),
            }),
            2,
            1,
            PAGE_BUDGET,
        )
        .await
        .expect("around seek");

    let descriptor = opaque_descriptor(&opened, after);
    let snapshot = snapshot_with_bytes(&opened, &fixture.sid, PAGE_BUDGET);
    let text = read_body_text(&backend, &snapshot, &descriptor).await;
    assert_eq!(
        text, after_text,
        "opaque body must be the full display text"
    );
    assert_eq!(
        descriptor.total_bytes.get(),
        expected_payload_bytes(&fixture, after).await.len() as u64,
        "page descriptor advertises the canonical byte estimate"
    );
    assert_eq!(
        text.len() as u64,
        read_body_total(&backend, &snapshot, &descriptor).await
    );
}

async fn read_body_total(
    backend: &HistoryV2BackendAdapter,
    snapshot: &peri_acp_types::history_v2::HistorySnapshotRef,
    descriptor: &HistoryBodyDescriptor,
) -> u64 {
    backend
        .body(snapshot, &descriptor.body_ref, 0, 1)
        .await
        .expect("body head")
        .total_bytes
        .get()
}

async fn set_inherited(
    tmp: &tempfile::TempDir,
    sid: &str,
    payloads: &[PersistedPayload],
    ancestor: &str,
) {
    let context = peri_acp_types::store::InheritedContext {
        payloads: payloads.to_vec(),
        flags: Default::default(),
    }
    .to_json()
    .unwrap();
    let pool = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new().filename(tmp.path().join("threads.db")),
    )
    .await
    .unwrap();
    let updated = sqlx::query(
        "UPDATE threads SET inherited_context = ?1, parent_thread_id = ?2 WHERE id = ?3",
    )
    .bind(context)
    .bind(ancestor)
    .bind(sid)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        updated.rows_affected(),
        1,
        "session {sid} must exist before seeding inherited"
    );
    pool.close().await;
}

async fn inherited_snapshot(
    fixture: &PageFixture,
    sid: &str,
) -> peri_acp_types::history_common::HistoryInheritedSnapshot {
    fixture
        .cfg
        .session_resources
        .history_window_bounds(&peri_acp_types::thread::ThreadId::from(sid.to_owned()))
        .await
        .unwrap()
        .inherited
        .snapshot()
        .cloned()
        .expect("inherited snapshot")
}

fn inherited_source(snapshot_id: &str) -> V2Source {
    V2Source::Inherited(peri_acp_types::history_v2::HistoryInheritedSource {
        snapshot_id: snapshot_id.to_owned(),
        ancestor_session_id: "parent-session".to_owned(),
    })
}

#[tokio::test]
#[serial]
async fn inherited_around_seek_after_entry_is_opaque_and_body_reads_fully() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = "fix02-inherited-around";
    create_empty_session(&fixture.cfg, tmp.path(), sid).await;

    let big = |prefix: &str, fill: char| {
        PersistedPayload::Message(BaseMessage::ai(format!(
            "{prefix}:{}",
            fill.to_string().repeat(BIG_TEXT)
        )))
    };
    let before_a = big("first", 'a');
    let before_b = big("second", 'b');
    let anchor = PersistedPayload::Message(BaseMessage::human("anchor"));
    let after_text = format!("after:{}", "z".repeat(BIG_TEXT));
    let after = PersistedPayload::Message(BaseMessage::ai(after_text.clone()));
    let after_id = after.id();
    let after_bytes = peri_acp_types::store::serialize_persisted_payload(&after)
        .unwrap()
        .len();
    let before_bytes: usize = [&before_a, &before_b]
        .iter()
        .map(|payload| {
            peri_acp_types::store::serialize_persisted_payload(payload)
                .unwrap()
                .len()
        })
        .sum();
    let anchor_bytes = peri_acp_types::store::serialize_persisted_payload(&anchor)
        .unwrap()
        .len();
    assert!(
        before_bytes + anchor_bytes < PAGE_BUDGET as usize,
        "before rows must fit the page budget"
    );
    assert!(
        after_bytes > PAGE_BUDGET as usize - (before_bytes + anchor_bytes),
        "after entry must exceed the remaining budget"
    );
    assert!(after_bytes <= PAGE_BUDGET as usize);

    set_inherited(
        &tmp,
        sid,
        &[before_a, before_b, anchor.clone(), after],
        "parent-session",
    )
    .await;
    let snapshot = inherited_snapshot(&fixture, sid).await;
    let backend = adapter(&fixture);
    let opened = backend
        .open(
            sid,
            &HistoryAnchor::Entry(HistoryEntryAnchor {
                source: inherited_source(&snapshot.snapshot_id),
                entry_id: anchor.id().as_uuid().to_string(),
            }),
            2,
            1,
            PAGE_BUDGET,
        )
        .await
        .expect("inherited around seek");

    let descriptor = opaque_descriptor(&opened, after_id);
    let body_snapshot = snapshot_with_bytes(&opened, sid, PAGE_BUDGET);
    let text = read_body_text(&backend, &body_snapshot, &descriptor).await;
    assert_eq!(text, after_text);
}

#[tokio::test]
#[serial]
async fn cross_region_open_keeps_inherited_oversized_body_readable() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let (_, before_a) = append_big(&fixture, false).await;
    let (_, before_b) = append_big(&fixture, true).await;
    assert!(
        before_a + before_b < PAGE_BUDGET as usize,
        "own rows must fit the page budget and leave a small inherited budget"
    );

    let inherited_text = format!("inherited:{}", "i".repeat(BIG_TEXT));
    let inherited = PersistedPayload::Message(BaseMessage::ai(inherited_text.clone()));
    let inherited_id = inherited.id();
    let inherited_bytes = peri_acp_types::store::serialize_persisted_payload(&inherited)
        .unwrap()
        .len();
    let remaining = PAGE_BUDGET as usize - (before_a + before_b);
    assert!(
        inherited_bytes > remaining,
        "inherited entry must exceed the budget left by own rows"
    );
    assert!(inherited_bytes <= PAGE_BUDGET as usize);
    set_inherited(&tmp, &fixture.sid, &[inherited], "parent-session").await;

    let backend = adapter(&fixture);
    let opened = backend
        .open(&fixture.sid, &HistoryAnchor::Latest, 10, 0, PAGE_BUDGET)
        .await
        .expect("cross-region open");
    let descriptor = opaque_descriptor(&opened, inherited_id);
    let snapshot = snapshot_with_bytes(&opened, &fixture.sid, PAGE_BUDGET);
    let text = read_body_text(&backend, &snapshot, &descriptor).await;
    assert_eq!(text, inherited_text);
}

#[tokio::test]
#[serial]
async fn same_entry_inline_and_opaque_yields_identical_display_text() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let text = format!("consistency:{}", "c".repeat(8 * 1024));
    let id = append_payload(
        &fixture,
        PersistedPayload::Message(BaseMessage::ai(text.clone())),
    )
    .await;
    let backend = adapter(&fixture);

    // 大预算：条目内联，展示文本直接来自页投影。
    let inline = backend
        .open(&fixture.sid, &HistoryAnchor::Latest, 1, 0, 64 * 1024)
        .await
        .expect("inline open");
    let inline_entry = inline
        .page
        .entries
        .iter()
        .find(|entry| entry.entry_id == format!("{}:{}", fixture.sid, id.as_uuid()))
        .expect("inline entry present");
    let inline_text = inline_entry
        .sub_items
        .iter()
        .filter_map(|sub_item| match sub_item {
            HistorySubItem::Text(text) => text.text.clone(),
            _ => None,
        })
        .collect::<String>();
    assert_eq!(inline_text, text);

    // 1 字节预算：同一条目外置，正文必须与内联展示一致。
    let opaque = backend
        .open(&fixture.sid, &HistoryAnchor::Latest, 1, 0, 1)
        .await
        .expect("opaque open");
    let descriptor = opaque_descriptor(&opaque, id);
    let snapshot = snapshot_with_bytes(&opaque, &fixture.sid, 1);
    let body_text = read_body_text(&backend, &snapshot, &descriptor).await;
    assert_eq!(
        body_text, inline_text,
        "同一内容在页内联与 body 外置必须得到相同展示文本"
    );
}
