//! H3a2：inherited 冻结前缀接入 History V2 适配器（真实 SQLite）。
//!
//! 覆盖跨区 open/page/seek、ordinal 稳定、有界 body 分块、oversize 可见性与
//! 固定 own tail 的失效语义；不新增表/列，全部复用现有存储原语。

use super::*;
use crate::host::history_v2_adapter::{inherited_prefix_visible, HistoryV2BackendAdapter};
use peri_acp_types::history_v2::{
    DecimalU64, HistoryAnchor, HistoryBackendError, HistoryBoundary, HistoryCoverage,
    HistoryCoverageInterval, HistoryDirection, HistoryDurabilityState, HistoryEntryAnchor,
    HistoryOrigin, HistoryProgressReason, HistorySource as V2Source, HistorySubItem,
    HistoryV2Backend, HISTORY_V2_PROJECTION_VERSION,
};
use peri_acp_types::store::InheritedContext;
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, TrustedSystemReminder, TrustedSystemReminderFactory,
};
use peri_acp_types::thread::ThreadId;
use peri_acp_types::workspace::ResolvedWorkspace;

fn adapter(fixture: &PageFixture) -> HistoryV2BackendAdapter {
    HistoryV2BackendAdapter::new(
        Arc::clone(&fixture.cfg.session_resources),
        PeriCaps::default(),
    )
}

fn wire_entry_id(fixture: &PageFixture, message_id: MessageId) -> String {
    format!("{}:{}", fixture.sid, message_id.as_uuid())
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

async fn set_inherited(
    tmp: &tempfile::TempDir,
    sid: &str,
    payloads: &[PersistedPayload],
    ancestor: &str,
) {
    let context = InheritedContext {
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
    sqlx::query("UPDATE threads SET inherited_context = ?1, parent_thread_id = ?2 WHERE id = ?3")
        .bind(context)
        .bind(ancestor)
        .bind(sid)
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}

async fn inherited_snapshot(
    fixture: &PageFixture,
) -> peri_acp_types::history_window::HistoryInheritedSnapshot {
    fixture
        .cfg
        .session_resources
        .history_window_bounds(&ThreadId::from(fixture.sid.clone()))
        .await
        .unwrap()
        .inherited
        .snapshot()
        .cloned()
        .expect("inherited snapshot")
}

fn snapshot_of(
    opened: &peri_acp_types::history_v2::HistoryOpenedSnapshot,
    session_id: &str,
) -> peri_acp_types::history_v2::HistorySnapshotRef {
    snapshot_with_bytes(opened, session_id, 64 * 1024)
}

fn snapshot_with_bytes(
    opened: &peri_acp_types::history_v2::HistoryOpenedSnapshot,
    session_id: &str,
    target_bytes: u64,
) -> peri_acp_types::history_v2::HistorySnapshotRef {
    peri_acp_types::history_v2::HistorySnapshotRef {
        session_id: session_id.to_owned(),
        committed_tail: opened.committed_tail.clone(),
        history_revision: opened.history_revision,
        projection_version: opened.projection_version,
        target_bytes,
    }
}

fn inherited_source(snapshot_id: &str) -> V2Source {
    V2Source::Inherited(peri_acp_types::history_v2::HistoryInheritedSource {
        snapshot_id: snapshot_id.to_owned(),
        ancestor_session_id: "parent-session".to_owned(),
    })
}

fn decode_base64(data: &str) -> Vec<u8> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(data)
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

fn reminder_with_body(body: &str, audiences: Vec<ReminderAudience>) -> TrustedSystemReminder {
    let reminder = SystemReminder {
        version: 1,
        category: ReminderCategory::Guidance,
        source: ReminderSource("history_v2_inherited_test".to_owned()),
        kind: "guidance".to_owned(),
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

#[tokio::test]
#[serial]
async fn open_latest_orders_inherited_before_own_and_keeps_own_tail() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let inherited = vec![
        PersistedPayload::Message(BaseMessage::human("祖先一")),
        PersistedPayload::Message(BaseMessage::ai("祖先二")),
    ];
    let inherited_ids: Vec<MessageId> = inherited.iter().map(PersistedPayload::id).collect();
    set_inherited(&tmp, &fixture.sid, &inherited, "parent-session").await;
    let meta = inherited_snapshot(&fixture).await;
    let own_ids = fixture.own_ids().await;
    let backend = adapter(&fixture);

    let opened = backend
        .open(&fixture.sid, &HistoryAnchor::Latest, 10, 0, 64 * 1024)
        .await
        .expect("open latest");
    assert_eq!(opened.page.entries.len(), 5);
    assert_eq!(opened.projection_version, HISTORY_V2_PROJECTION_VERSION);

    for (entry, message_id) in opened.page.entries[..2].iter().zip(&inherited_ids) {
        assert_eq!(entry.entry_id, wire_entry_id(&fixture, *message_id));
        let V2Source::Inherited(source) = &entry.source else {
            panic!("inherited prefix must stay inherited");
        };
        assert_eq!(source.snapshot_id, meta.snapshot_id);
        assert_eq!(source.ancestor_session_id, "parent-session");
        assert_eq!(entry.durability.state, HistoryDurabilityState::Sealed);
        assert_eq!(entry.durability.origin, HistoryOrigin::Imported);
        assert_eq!(entry.durability.final_item_seq, None);
        assert_eq!(entry.order.region, "inherited");
    }
    let inherited_ordinals: Vec<u64> = opened.page.entries[..2]
        .iter()
        .map(|entry| entry.order.ordinal.get())
        .collect();
    assert_eq!(inherited_ordinals, vec![0, 1]);

    for (entry, message_id) in opened.page.entries[2..].iter().zip(&own_ids) {
        assert_eq!(entry.entry_id, wire_entry_id(&fixture, *message_id));
        assert!(matches!(entry.source, V2Source::Own));
        assert_eq!(entry.durability.origin, HistoryOrigin::Live);
        assert_eq!(entry.order.region, "own");
    }
    let own_ordinals: Vec<u64> = opened.page.entries[2..]
        .iter()
        .map(|entry| entry.order.ordinal.get())
        .collect();
    assert_eq!(own_ordinals.len(), 3);
    assert!(
        own_ordinals.windows(2).all(|pair| pair[0] < pair[1]),
        "own positions stay in storage order"
    );

    let tail = opened.committed_tail.as_ref().expect("own tail");
    assert_eq!(tail.entry_id, wire_entry_id(&fixture, own_ids[2]));
    assert_eq!(opened.history_revision, DecimalU64::new(tail.position));
    assert_eq!(opened.page.history_revision, opened.history_revision);
    assert_eq!(
        opened.page.coverage,
        HistoryCoverage::Interval(HistoryCoverageInterval {
            from: HistoryBoundary::Head,
            to: HistoryBoundary::Tail,
        })
    );
}

#[tokio::test]
#[serial]
async fn before_pages_cross_regions_without_duplicates() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let inherited = vec![
        PersistedPayload::Message(BaseMessage::human("祖先一")),
        PersistedPayload::Message(BaseMessage::ai("祖先二")),
    ];
    let inherited_ids: Vec<MessageId> = inherited.iter().map(PersistedPayload::id).collect();
    set_inherited(&tmp, &fixture.sid, &inherited, "parent-session").await;
    let own_ids = fixture.own_ids().await;
    let backend = adapter(&fixture);

    let opened = backend
        .open(&fixture.sid, &HistoryAnchor::Latest, 2, 0, 64 * 1024)
        .await
        .expect("open latest");
    let snapshot = snapshot_of(&opened, &fixture.sid);
    assert_eq!(
        opened
            .page
            .entries
            .iter()
            .map(|entry| (entry.order.region.as_str(), entry.entry_id.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("own", wire_entry_id(&fixture, own_ids[1]).as_str()),
            ("own", wire_entry_id(&fixture, own_ids[2]).as_str()),
        ]
    );

    let first = backend
        .page(
            &snapshot,
            opened.page.before_cursor.as_deref().expect("before cursor"),
            HistoryDirection::Before,
            2,
        )
        .await
        .expect("cross-region page");
    assert_eq!(
        first
            .entries
            .iter()
            .map(|entry| (entry.order.region.as_str(), entry.entry_id.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (
                "inherited",
                wire_entry_id(&fixture, inherited_ids[1]).as_str()
            ),
            ("own", wire_entry_id(&fixture, own_ids[0]).as_str()),
        ],
        "inherited stays before own and no consumed entry repeats"
    );

    let second = backend
        .page(
            &snapshot,
            first.before_cursor.as_deref().expect("inherited cursor"),
            HistoryDirection::Before,
            2,
        )
        .await
        .expect("inherited head page");
    assert_eq!(
        second.entries.len(),
        1,
        "only the frozen inherited head remains"
    );
    assert_eq!(second.entries[0].order.region, "inherited");
    assert_eq!(second.entries[0].order.ordinal, DecimalU64::new(0));
    assert!(second.before_cursor.is_none(), "history head reached");

    let ids: Vec<String> = opened
        .page
        .entries
        .iter()
        .chain(first.entries.iter())
        .chain(second.entries.iter())
        .map(|entry| entry.entry_id.clone())
        .collect();
    let unique: std::collections::HashSet<&String> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len(), "no duplicate across pages");
    assert_eq!(
        ids,
        vec![
            wire_entry_id(&fixture, own_ids[1]),
            wire_entry_id(&fixture, own_ids[2]),
            wire_entry_id(&fixture, inherited_ids[1]),
            wire_entry_id(&fixture, own_ids[0]),
            wire_entry_id(&fixture, inherited_ids[0]),
        ],
        "logical order is inherited prefix then own tail"
    );
}

#[tokio::test]
#[serial]
async fn after_page_from_inherited_anchor_continues_into_own_without_skipping() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let inherited = vec![
        PersistedPayload::Message(BaseMessage::human("祖先一")),
        PersistedPayload::Message(BaseMessage::ai("祖先二")),
    ];
    set_inherited(&tmp, &fixture.sid, &inherited, "parent-session").await;
    let meta = inherited_snapshot(&fixture).await;
    let backend = adapter(&fixture);

    let opened = backend
        .open(
            &fixture.sid,
            &HistoryAnchor::Entry(HistoryEntryAnchor {
                entry_id: wire_entry_id(&fixture, inherited[0].id()),
                source: inherited_source(&meta.snapshot_id),
            }),
            0,
            1,
            64 * 1024,
        )
        .await
        .expect("open inherited entry");
    let snapshot = snapshot_of(&opened, &fixture.sid);
    assert_eq!(opened.page.entries.len(), 2);
    assert_eq!(opened.page.entries[0].order.ordinal, DecimalU64::new(0));
    assert_eq!(opened.page.entries[1].order.ordinal, DecimalU64::new(1));

    let next = backend
        .page(
            &snapshot,
            opened.page.after_cursor.as_deref().expect("after cursor"),
            HistoryDirection::After,
            10,
        )
        .await
        .expect("continue into own");
    assert_eq!(next.entries.len(), 3);
    assert!(
        next.entries
            .iter()
            .all(|entry| matches!(entry.source, V2Source::Own)),
        "after inherited tail the page continues with own entries"
    );
    assert_eq!(
        next.entries
            .iter()
            .map(|entry| entry.entry_id.clone())
            .collect::<Vec<_>>(),
        fixture
            .own_ids()
            .await
            .iter()
            .map(|id| wire_entry_id(&fixture, *id))
            .collect::<Vec<_>>()
    );
    assert!(next.after_cursor.is_none(), "own tail reached");

    let back = backend
        .page(
            &snapshot,
            next.before_cursor.as_deref().expect("back cursor"),
            HistoryDirection::Before,
            10,
        )
        .await
        .expect("back into inherited");
    assert_eq!(
        back.entries
            .iter()
            .map(|entry| entry.order.region.as_str())
            .collect::<Vec<_>>(),
        vec!["inherited", "inherited"]
    );
    assert_eq!(back.entries[0].order.ordinal, DecimalU64::new(0));
    assert_eq!(back.entries[1].order.ordinal, DecimalU64::new(1));
    assert!(back.before_cursor.is_none(), "inherited head reached");
}

#[tokio::test]
#[serial]
async fn seek_inherited_returns_anchor_once_with_stable_ordinals() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let inherited = vec![
        PersistedPayload::Message(BaseMessage::human("祖先一")),
        PersistedPayload::Message(BaseMessage::ai("祖先二")),
        PersistedPayload::Message(BaseMessage::human("祖先三")),
        PersistedPayload::Message(BaseMessage::ai("祖先四")),
    ];
    set_inherited(&tmp, &fixture.sid, &inherited, "parent-session").await;
    let meta = inherited_snapshot(&fixture).await;
    let backend = adapter(&fixture);
    let anchor = HistoryEntryAnchor {
        entry_id: wire_entry_id(&fixture, inherited[2].id()),
        source: inherited_source(&meta.snapshot_id),
    };

    let opened = backend
        .open(
            &fixture.sid,
            &HistoryAnchor::Entry(anchor.clone()),
            1,
            1,
            64 * 1024,
        )
        .await
        .expect("open inherited around");
    let snapshot = snapshot_of(&opened, &fixture.sid);
    let ordinals: Vec<u64> = opened
        .page
        .entries
        .iter()
        .map(|entry| entry.order.ordinal.get())
        .collect();
    assert_eq!(ordinals, vec![1, 2, 3]);
    assert_eq!(
        opened
            .page
            .entries
            .iter()
            .filter(|entry| entry.order.ordinal == DecimalU64::new(2))
            .count(),
        1,
        "anchor appears exactly once"
    );
    assert!(opened
        .page
        .entries
        .iter()
        .all(|entry| entry.durability.origin == HistoryOrigin::Imported));

    let sought = backend
        .seek(&snapshot, &anchor, 1, 1)
        .await
        .expect("seek inherited");
    assert_eq!(
        sought
            .entries
            .iter()
            .map(|entry| entry.order.ordinal.get())
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(sought.progress.reason, HistoryProgressReason::Complete);
}

#[tokio::test]
#[serial]
async fn inherited_body_chunks_match_projected_text() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let sid = "inherited-body-v2";
    create_empty_session(&fixture.cfg, tmp.path(), sid).await;
    let payload = PersistedPayload::Message(BaseMessage::human("b".repeat(4096)));
    let canonical = peri_acp_types::store::serialize_persisted_payload(&payload)
        .unwrap()
        .into_bytes();
    // 展示投影只保留原文，不包含 canonical 信封。
    let expected = "b".repeat(4096).into_bytes();
    set_inherited(&tmp, sid, &[payload], "parent-session").await;
    let backend = adapter(&fixture);

    let opened = backend
        .open(sid, &HistoryAnchor::Latest, 10, 0, 64)
        .await
        .expect("oversized inherited open");
    assert_eq!(opened.page.entries.len(), 1);
    let body = match &opened.page.entries[0].sub_items[0] {
        HistorySubItem::Opaque(opaque) => opaque.body.clone(),
        other => panic!("expected opaque descriptor, got {other:?}"),
    };
    assert_eq!(
        body.representation,
        peri_acp_types::history_v2::HistoryRepresentation::DisplayText
    );
    assert_eq!(body.total_bytes, DecimalU64::new(canonical.len() as u64));
    let snapshot = snapshot_with_bytes(&opened, sid, 64);

    let mut collected = Vec::new();
    let mut offset = 0u64;
    loop {
        let slice = backend
            .body(&snapshot, &body.body_ref, offset, 512)
            .await
            .expect("inherited body slice");
        let chunk = decode_base64(&slice.data);
        assert_eq!(
            chunk,
            expected[offset as usize..offset as usize + chunk.len()]
        );
        collected.extend_from_slice(&chunk);
        if slice.complete {
            assert_eq!(slice.next_offset, None);
            break;
        }
        offset = slice.next_offset.expect("next offset").get();
        assert_eq!(offset, collected.len() as u64);
    }
    assert_eq!(collected, expected, "chunks are byte-identical to storage");
    assert!(backend
        .visibility_read_max_bytes()
        .iter()
        .all(|bytes| *bytes <= 1024));
}

#[tokio::test]
#[serial]
async fn inherited_oversized_visibility_uses_bounded_prefix() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let backend = adapter(&fixture);

    let model_sid = "inherited-model-only-v2";
    create_empty_session(&fixture.cfg, tmp.path(), model_sid).await;
    let model_id = MessageId::new();
    let model_only = PersistedPayload::SystemReminder {
        id: model_id,
        reminder: reminder_with_body(&"m".repeat(64 * 1024), vec![ReminderAudience::Model]),
    };
    set_inherited(&tmp, model_sid, &[model_only], "parent-session").await;
    let opened = backend
        .open(model_sid, &HistoryAnchor::Latest, 10, 0, 64)
        .await
        .expect("model-only inherited open");
    assert!(
        opened.page.entries.is_empty(),
        "model-only reminder must not be projected"
    );
    assert_eq!(
        backend.visibility_read_max_bytes(),
        vec![1024],
        "visibility prefix read is bounded to 1024 bytes"
    );
    // 即使猜到 inherited 条目身份，body 也必须按前缀受众再次拒绝。
    use base64::Engine as _;
    let bounds = fixture
        .cfg
        .session_resources
        .history_window_bounds(&peri_acp_types::thread::ThreadId::from(model_sid))
        .await
        .expect("inherited bounds");
    let frozen = bounds
        .inherited
        .snapshot()
        .expect("frozen inherited snapshot");
    let inherited_ref = format!(
        "i:{}:{}:{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(frozen.snapshot_id.as_bytes()),
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            frozen
                .ancestor_thread_id
                .as_ref()
                .expect("ancestor")
                .as_str()
                .as_bytes()
        ),
        model_id.as_uuid()
    );
    let model_snapshot = snapshot_with_bytes(&opened, model_sid, 64 * 1024);
    let error = backend
        .body(&model_snapshot, &inherited_ref, 0, 1024)
        .await
        .expect_err("model-only inherited body must stay hidden");
    assert_eq!(
        error,
        peri_acp_types::history_v2::HistoryBackendError::NotFound
    );
    backend.reset_visibility_reads();

    let tui_sid = "inherited-tui-v2";
    create_empty_session(&fixture.cfg, tmp.path(), tui_sid).await;
    let tui_reminder = PersistedPayload::SystemReminder {
        id: MessageId::new(),
        reminder: reminder_with_body(&"t".repeat(64 * 1024), vec![ReminderAudience::Tui]),
    };
    let tui_id = tui_reminder.id();
    let tui_bytes = peri_acp_types::store::serialize_persisted_payload(&tui_reminder)
        .unwrap()
        .into_bytes();
    set_inherited(&tmp, tui_sid, &[tui_reminder], "parent-session").await;
    let opened = backend
        .open(tui_sid, &HistoryAnchor::Latest, 10, 0, 64)
        .await
        .expect("tui inherited open");
    assert_eq!(opened.page.entries.len(), 1);
    let entry = &opened.page.entries[0];
    assert_eq!(entry.entry_id, format!("{tui_sid}:{}", tui_id.as_uuid()));
    let body = match &entry.sub_items[0] {
        HistorySubItem::Opaque(opaque) => opaque.body.clone(),
        other => panic!("expected opaque descriptor, got {other:?}"),
    };
    assert_eq!(body.total_bytes, DecimalU64::new(tui_bytes.len() as u64));
    assert_eq!(
        backend.visibility_read_max_bytes(),
        vec![1024],
        "one bounded read per oversized entry"
    );

    let snapshot = snapshot_with_bytes(&opened, tui_sid, 64);
    let slice = backend
        .body(&snapshot, &body.body_ref, 0, 1024)
        .await
        .expect("tui reminder body");
    let prefix = decode_base64(&slice.data);
    let prefix = String::from_utf8(prefix).expect("display text");
    assert!(prefix.starts_with("提醒："));
    assert!(prefix.contains('t'), "reminder body must be visible");
    assert!(!prefix.contains("system_reminder"));
    let expected_display = format!("提醒：guidance\n{}", "t".repeat(64 * 1024));
    assert_eq!(slice.total_bytes.get(), expected_display.len() as u64);
}

#[tokio::test]
#[serial]
async fn own_append_does_not_change_the_frozen_prefix_and_tail_delete_invalidates() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    let inherited = vec![
        PersistedPayload::Message(BaseMessage::human("祖先一")),
        PersistedPayload::Message(BaseMessage::ai("祖先二")),
    ];
    let inherited_ids: Vec<MessageId> = inherited.iter().map(PersistedPayload::id).collect();
    set_inherited(&tmp, &fixture.sid, &inherited, "parent-session").await;
    let meta = inherited_snapshot(&fixture).await;
    let own_ids = fixture.own_ids().await;
    let backend = adapter(&fixture);

    let opened = backend
        .open(
            &fixture.sid,
            &HistoryAnchor::Entry(HistoryEntryAnchor {
                entry_id: wire_entry_id(&fixture, inherited[1].id()),
                source: inherited_source(&meta.snapshot_id),
            }),
            1,
            0,
            64 * 1024,
        )
        .await
        .expect("open inherited");
    let snapshot = snapshot_of(&opened, &fixture.sid);
    let prefix_before: Vec<String> = opened
        .page
        .entries
        .iter()
        .map(|entry| entry.entry_id.clone())
        .collect();
    assert_eq!(
        prefix_before,
        vec![
            wire_entry_id(&fixture, inherited_ids[0]),
            wire_entry_id(&fixture, inherited_ids[1]),
        ]
    );

    append_tool(&fixture).await;
    let page = backend
        .page(
            &snapshot,
            opened.page.after_cursor.as_deref().expect("after cursor"),
            HistoryDirection::After,
            10,
        )
        .await
        .expect("own append stays outside the fixed tail");
    assert_eq!(
        page.entries
            .iter()
            .map(|entry| entry.entry_id.clone())
            .collect::<Vec<_>>(),
        own_ids
            .iter()
            .map(|id| wire_entry_id(&fixture, *id))
            .collect::<Vec<_>>(),
        "the fixed window stops at the snapshot tail"
    );
    assert_eq!(
        page.entries.last().unwrap().order.ordinal,
        DecimalU64::new(opened.history_revision.get()),
        "the last own entry is the fixed committed tail"
    );

    let back = backend
        .page(
            &snapshot,
            page.before_cursor.as_deref().expect("back cursor"),
            HistoryDirection::Before,
            10,
        )
        .await
        .expect("inherited prefix after own append");
    assert_eq!(
        back.entries
            .iter()
            .map(|entry| entry.entry_id.clone())
            .collect::<Vec<_>>(),
        prefix_before,
        "own appends never rewrite the frozen prefix"
    );

    fixture
        .cfg
        .session_resources
        .remove_history_entries(&fixture.sid, &[own_ids[2]])
        .await
        .unwrap();
    let error = backend
        .page(
            &snapshot,
            opened.page.after_cursor.as_deref().expect("after cursor"),
            HistoryDirection::After,
            1,
        )
        .await
        .expect_err("fixed tail was removed");
    assert_eq!(error, HistoryBackendError::SnapshotInvalidated);
}

#[test]
fn inherited_prefix_judgement_is_fail_closed_when_identity_is_unknown() {
    assert!(!inherited_prefix_visible(&[b'{'; 1024]));
    assert!(!inherited_prefix_visible(
        br#"{"type":"system","id":"x","content":"..."}"#
    ));
    assert!(!inherited_prefix_visible(
        br#"{"type":"system_reminder","id":"x","reminder":{"audiences":["model"],"body":"..."}}"#
    ));
    assert!(inherited_prefix_visible(
        br#"{"type":"system_reminder","id":"x","reminder":{"audiences":["tui"],"body":"..."}}"#
    ));
    assert!(inherited_prefix_visible(
        br#"{"role":"user","content":"..."}"#
    ));
}

/// 大 inherited 快照没有派生索引：open/page 都必须是 `index_not_ready`，
/// 不能改走父会话当前态或返回空页。
#[tokio::test]
#[serial]
async fn oversized_inherited_without_index_maps_to_index_not_ready() {
    let tmp = tempfile::TempDir::new().unwrap();
    let fixture = PageFixture::new(&tmp).await;
    // > 8 MiB：超过回退扫描门禁。
    let large = PersistedPayload::Message(BaseMessage::human("x".repeat(9 * 1024 * 1024)));
    let entry_id = large.id();
    set_inherited(&tmp, &fixture.sid, &[large], "parent-session").await;
    let backend = adapter(&fixture);

    let open_error = backend
        .open(&fixture.sid, &HistoryAnchor::Latest, 10, 0, 64 * 1024)
        .await
        .expect_err("large inherited without index must not open");
    assert_eq!(open_error, HistoryBackendError::IndexNotReady);

    // page 先解析 inherited cursor；门禁在冻结快照身份校验之前触发。
    let cursor = inherited_cursor("snapshot-without-index", "parent-session", entry_id);
    let snapshot = peri_acp_types::history_v2::HistorySnapshotRef {
        session_id: fixture.sid.clone(),
        committed_tail: None,
        history_revision: DecimalU64::new(0),
        projection_version: HISTORY_V2_PROJECTION_VERSION,
        target_bytes: 64 * 1024,
    };
    let page_error = backend
        .page(&snapshot, &cursor, HistoryDirection::Before, 10)
        .await
        .expect_err("large inherited without index must not page");
    assert_eq!(page_error, HistoryBackendError::IndexNotReady);
}

fn inherited_cursor(snapshot_id: &str, ancestor: &str, entry_id: MessageId) -> String {
    use base64::Engine as _;
    let encode =
        |value: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.as_bytes());
    format!(
        "i:{}:{}:{}",
        encode(snapshot_id),
        encode(ancestor),
        entry_id.as_uuid()
    )
}
