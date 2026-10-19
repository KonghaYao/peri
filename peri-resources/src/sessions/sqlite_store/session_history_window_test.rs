//! `peri.historyWindowV1` 的 SQLite 有界读取：边界、checkpoint、双向 keyset、tail 失效、
//! 归档正文与超大条目 descriptor、正文分块。
//!
//! 断言以调用方可观察结果为准：读了哪些条目、返回哪些 cursor、失败分类，以及
//! 「lookahead / 超大正文没有被解码」这类可由损坏行反证的事实。

use super::*;
use peri_acp_types::history_window::{
    HistoryCommittedTail, HistorySource, HistoryWindowDirection, HISTORY_WINDOW_DEFAULT_BYTE_BUDGET,
};
use peri_acp_types::messages::MessageId;
use peri_acp_types::projection::MessageProjectionDirective;
use peri_acp_types::session_resources::{
    HistoryWindowEntry, HistoryWindowPagePlan, HistoryWindowReadIntent,
    HistoryWindowTailExpectation,
};

const WINDOW_SESSION: &str = "window-session";
const BUDGET: usize = HISTORY_WINDOW_DEFAULT_BYTE_BUDGET as usize;

async fn session_with_history(
    history: &[PersistedPayload],
) -> (SqliteThreadStore, SqliteSessionData, TempDir) {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    data.save_new_session(&session(
        WINDOW_SESSION,
        workspace.cwd.to_str().unwrap(),
        &workspace,
        frozen(WINDOW_SESSION),
    ))
    .await
    .unwrap();
    data.append_history(&WINDOW_SESSION.to_owned(), history)
        .await
        .unwrap();
    (store, data, directory)
}

fn loaded_ids(rows: &peri_acp_types::session_resources::HistoryWindowRows) -> Vec<MessageId> {
    rows.entries
        .iter()
        .filter_map(|entry| match entry {
            HistoryWindowEntry::Loaded(item) => Some(item.id),
            HistoryWindowEntry::Oversized { .. } => None,
        })
        .collect()
}

fn all_ids(rows: &peri_acp_types::session_resources::HistoryWindowRows) -> Vec<MessageId> {
    rows.entries
        .iter()
        .map(HistoryWindowEntry::entry_id)
        .collect()
}

#[tokio::test]
async fn bounds_report_range_tail_and_unknown_sessions_fail_closed() {
    let history = payloads(3);
    let (_store, data, _directory) = session_with_history(&history).await;
    let id = WINDOW_SESSION.to_owned();

    let bounds = data.history_window_bounds(&id).await.unwrap();
    assert_eq!(bounds.own_oldest, Some(history[0].id()));
    let tail = bounds.own_latest.expect("non-empty session has a tail");
    assert_eq!(tail.entry_id, history[2].id());
    assert_eq!(tail.position, 3);
    assert!(bounds.checkpoint.is_none());
    assert!(!bounds.inherited.is_present());

    let missing = data
        .history_window_bounds(&"missing".to_owned())
        .await
        .unwrap_err();
    assert!(matches!(
        missing.kind(),
        peri_acp_types::session_resources::SessionResourceErrorKind::NotFound
    ));
}

#[tokio::test]
async fn checkpoint_is_rebuilt_from_the_latest_superseded_by_target() {
    let history = payloads(3);
    let (_store, data, _directory) = session_with_history(&history).await;
    let id = WINDOW_SESSION.to_owned();
    let summary = BaseMessage::ai("compacted summary");
    let summary_id = summary.id();
    let directive = MessageProjectionDirective {
        policy_version: 1,
        entries: Vec::new(),
        superseded_by: Some(summary_id),
    };
    data.apply_compaction(
        &id,
        &CompactionChange {
            flag_updates: history
                .iter()
                .map(|payload| {
                    (
                        payload.id(),
                        MessageFlags {
                            excluded: true,
                            projection: Some(directive.clone()),
                            ..Default::default()
                        },
                    )
                })
                .collect(),
            appended_messages: vec![summary],
        },
    )
    .await
    .unwrap();

    let bounds = data.history_window_bounds(&id).await.unwrap();
    let checkpoint = bounds.checkpoint.expect("summary row is a checkpoint");
    assert_eq!(checkpoint.summary_entry_id, Some(summary_id));
    assert_eq!(checkpoint.position, 4);
    assert_eq!(
        checkpoint.checkpoint_id,
        peri_acp_types::history_window::HistoryEntryId::new(&id, summary_id).encode()
    );
    assert_eq!(
        bounds.own_latest.as_ref().map(|tail| tail.entry_id),
        Some(summary_id)
    );
    // 有效上下文仍排除归档正文；窗口读取不受影响。
    let active = data.load_active_snapshot(&id).await.unwrap();
    assert_eq!(active.payloads.len(), 1);
}

#[tokio::test]
async fn malformed_projection_does_not_break_bounds() {
    let history = payloads(2);
    let (store, data, _directory) = session_with_history(&history).await;
    let id = WINDOW_SESSION.to_owned();
    sqlx::query("UPDATE messages SET excluded = 1, projection = ?1 WHERE message_id = ?2")
        .bind("not-json")
        .bind(history[0].id().as_uuid().to_string())
        .execute(&store.database.pool)
        .await
        .unwrap();

    let bounds = data
        .history_window_bounds(&id)
        .await
        .expect("a legacy/corrupt projection must not break bounds");
    assert!(bounds.checkpoint.is_none());
}

#[tokio::test]
async fn open_before_reads_a_bounded_range_and_pages_backwards() {
    let history = payloads(7);
    let (_store, data, _directory) = session_with_history(&history).await;
    let id = WINDOW_SESSION.to_owned();

    let first = data
        .load_history_window_page(
            &id,
            &HistoryWindowPagePlan::open(
                HistorySource::Own,
                None,
                HistoryWindowDirection::Before,
                3,
                0,
                BUDGET,
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        loaded_ids(&first),
        vec![history[4].id(), history[5].id(), history[6].id()]
    );
    assert_eq!(first.before_cursor_anchor, Some(history[4].id()));
    assert_eq!(first.after_cursor_anchor, None);
    let tail = first.committed_tail.clone();

    let second = data
        .load_history_window_page(
            &id,
            &HistoryWindowPagePlan::page(
                HistorySource::Own,
                history[4].id(),
                HistoryWindowDirection::Before,
                2,
                BUDGET,
                tail.clone(),
            ),
        )
        .await
        .unwrap();
    assert_eq!(loaded_ids(&second), vec![history[2].id(), history[3].id()]);
    assert_eq!(second.before_cursor_anchor, Some(history[2].id()));

    let third = data
        .load_history_window_page(
            &id,
            &HistoryWindowPagePlan::page(
                HistorySource::Own,
                history[2].id(),
                HistoryWindowDirection::Before,
                2,
                BUDGET,
                tail,
            ),
        )
        .await
        .unwrap();
    assert_eq!(loaded_ids(&third), vec![history[0].id(), history[1].id()]);
    assert_eq!(third.before_cursor_anchor, None);
}

#[tokio::test]
async fn before_page_exposes_a_later_cursor() {
    let history = payloads(7);
    let (_store, data, _directory) = session_with_history(&history).await;
    let id = WINDOW_SESSION.to_owned();

    let opened = data
        .load_history_window_page(
            &id,
            &HistoryWindowPagePlan::open(
                HistorySource::Own,
                None,
                HistoryWindowDirection::Before,
                3,
                0,
                BUDGET,
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        opened.after_cursor_anchor, None,
        "页已到 durable tail：更晚方向确实没有可读行，不能再铸造游标"
    );
    let tail = opened.committed_tail.clone();

    // 向上翻一页后，本页与 tail 之间仍有可读行：afterCursor 必须指向本页最新行。
    let older = data
        .load_history_window_page(
            &id,
            &HistoryWindowPagePlan::page(
                HistorySource::Own,
                history[4].id(),
                HistoryWindowDirection::Before,
                2,
                BUDGET,
                tail,
            ),
        )
        .await
        .unwrap();
    assert_eq!(loaded_ids(&older), vec![history[2].id(), history[3].id()]);
    assert_eq!(older.before_cursor_anchor, Some(history[2].id()));
    assert_eq!(
        older.after_cursor_anchor,
        Some(history[3].id()),
        "更早页仍必须暴露更晚方向的内侧锚点，否则淘汰后无法向下补回"
    );
}

#[tokio::test]
async fn after_page_exposes_an_earlier_cursor() {
    let history = payloads(7);
    let (_store, data, _directory) = session_with_history(&history).await;
    let id = WINDOW_SESSION.to_owned();
    let opened = data
        .load_history_window_page(
            &id,
            &HistoryWindowPagePlan::open(
                HistorySource::Own,
                None,
                HistoryWindowDirection::Before,
                1,
                0,
                BUDGET,
            ),
        )
        .await
        .unwrap();
    let tail = opened.committed_tail.clone();

    let newer = data
        .load_history_window_page(
            &id,
            &HistoryWindowPagePlan::page(
                HistorySource::Own,
                history[0].id(),
                HistoryWindowDirection::After,
                2,
                BUDGET,
                tail,
            ),
        )
        .await
        .unwrap();
    assert_eq!(loaded_ids(&newer), vec![history[1].id(), history[2].id()]);
    assert_eq!(
        newer.before_cursor_anchor,
        Some(history[1].id()),
        "更晚页也必须暴露更早方向的内侧锚点"
    );
    assert_eq!(newer.after_cursor_anchor, Some(history[2].id()));
}

#[tokio::test]
async fn open_entry_after_includes_the_anchor() {
    let history = payloads(7);
    let (_store, data, _directory) = session_with_history(&history).await;
    let id = WINDOW_SESSION.to_owned();

    let opened = data
        .load_history_window_page(
            &id,
            &HistoryWindowPagePlan::open(
                HistorySource::Own,
                Some(history[3].id()),
                HistoryWindowDirection::After,
                0,
                2,
                BUDGET,
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        loaded_ids(&opened),
        vec![history[3].id(), history[4].id()],
        "after 半段必须包含锚点自身，且只出现一次"
    );
    assert_eq!(opened.before_cursor_anchor, Some(history[3].id()));
    assert_eq!(opened.after_cursor_anchor, Some(history[4].id()));
}

#[tokio::test]
async fn seek_with_zero_before_limit_keeps_the_anchor() {
    let history = payloads(7);
    let (_store, data, _directory) = session_with_history(&history).await;
    let id = WINDOW_SESSION.to_owned();
    let tail = data
        .history_window_bounds(&id)
        .await
        .unwrap()
        .own_latest
        .unwrap();

    let sought = data
        .load_history_window_page(
            &id,
            &HistoryWindowPagePlan::seek(HistorySource::Own, history[3].id(), 0, 2, BUDGET, tail),
        )
        .await
        .unwrap();
    assert_eq!(loaded_ids(&sought), vec![history[3].id(), history[4].id()]);
    assert_eq!(sought.before_cursor_anchor, Some(history[3].id()));
    assert_eq!(sought.after_cursor_anchor, Some(history[4].id()));
}

#[tokio::test]
async fn around_open_includes_the_anchor_once() {
    let history = payloads(7);
    let (_store, data, _directory) = session_with_history(&history).await;
    let id = WINDOW_SESSION.to_owned();

    let opened = data
        .load_history_window_page(
            &id,
            &HistoryWindowPagePlan::open(
                HistorySource::Own,
                Some(history[3].id()),
                HistoryWindowDirection::Around,
                1,
                2,
                BUDGET,
            ),
        )
        .await
        .unwrap();
    let ids = all_ids(&opened);
    assert_eq!(ids, vec![history[3].id(), history[4].id(), history[5].id()]);
    assert_eq!(
        ids.iter().filter(|id| **id == history[3].id()).count(),
        1,
        "锚点只能由 before 半段返回一次"
    );
    assert_eq!(opened.before_cursor_anchor, Some(history[3].id()));
    assert_eq!(opened.after_cursor_anchor, Some(history[5].id()));
}

#[tokio::test]
async fn after_reads_only_later_rows_and_appends_stay_outside_the_old_tail() {
    let history = payloads(7);
    let (_store, data, _directory) = session_with_history(&history).await;
    let id = WINDOW_SESSION.to_owned();

    let opened = data
        .load_history_window_page(
            &id,
            &HistoryWindowPagePlan::open(
                HistorySource::Own,
                None,
                HistoryWindowDirection::Before,
                2,
                0,
                BUDGET,
            ),
        )
        .await
        .unwrap();
    let tail = opened.committed_tail.clone();
    let appended = PersistedPayload::Message(BaseMessage::human("after the snapshot"));
    data.append_history(&id, std::slice::from_ref(&appended))
        .await
        .unwrap();

    let after = data
        .load_history_window_page(
            &id,
            &HistoryWindowPagePlan::page(
                HistorySource::Own,
                history[0].id(),
                HistoryWindowDirection::After,
                3,
                BUDGET,
                tail.clone(),
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        loaded_ids(&after),
        vec![history[1].id(), history[2].id(), history[3].id()]
    );
    assert_eq!(after.after_cursor_anchor, Some(history[3].id()));
    assert_eq!(after.committed_tail, tail);
    assert!(!all_ids(&after).contains(&appended.id()));
}

#[tokio::test]
async fn rewind_past_the_fixed_tail_invalidates_the_old_snapshot() {
    let history = payloads(6);
    let (_store, data, _directory) = session_with_history(&history).await;
    let id = WINDOW_SESSION.to_owned();
    let opened = data
        .load_history_window_page(
            &id,
            &HistoryWindowPagePlan::open(
                HistorySource::Own,
                None,
                HistoryWindowDirection::Before,
                2,
                0,
                BUDGET,
            ),
        )
        .await
        .unwrap();
    let tail = opened.committed_tail.clone();

    data.rewind_history(&id, RewindBoundary::KeepThrough(history[3].id()), &[])
        .await
        .unwrap();

    let stale = data
        .load_history_window_page(
            &id,
            &HistoryWindowPagePlan::page(
                HistorySource::Own,
                history[2].id(),
                HistoryWindowDirection::Before,
                2,
                BUDGET,
                tail.clone(),
            ),
        )
        .await
        .unwrap_err();
    assert_eq!(
        stale.history_window_error().map(|error| error.code()),
        Some(-32012)
    );

    // 旧 tail 行被删后，新 open 必须重新计算尾界。
    let reopened = data
        .load_history_window_page(
            &id,
            &HistoryWindowPagePlan::open(
                HistorySource::Own,
                None,
                HistoryWindowDirection::Before,
                2,
                0,
                BUDGET,
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        reopened.committed_tail.as_ref().map(|tail| tail.entry_id),
        Some(history[3].id())
    );
    assert_ne!(reopened.committed_tail, tail);
}

#[tokio::test]
async fn seek_includes_the_anchor_and_returns_both_sides_without_gaps() {
    let history = payloads(7);
    let (_store, data, _directory) = session_with_history(&history).await;
    let id = WINDOW_SESSION.to_owned();
    let bounds = data.history_window_bounds(&id).await.unwrap();
    let tail = bounds.own_latest.clone().unwrap();

    let rows = data
        .seek_history_window(
            &id,
            HistorySource::Own,
            history[3].id(),
            2,
            2,
            BUDGET,
            &tail,
        )
        .await
        .unwrap();
    assert_eq!(
        loaded_ids(&rows),
        vec![
            history[2].id(),
            history[3].id(),
            history[4].id(),
            history[5].id()
        ]
    );
    assert_eq!(rows.before_cursor_anchor, Some(history[2].id()));
    assert_eq!(rows.after_cursor_anchor, Some(history[5].id()));

    let missing = data
        .seek_history_window(
            &id,
            HistorySource::Own,
            MessageId::new(),
            2,
            2,
            BUDGET,
            &tail,
        )
        .await
        .unwrap_err();
    assert_eq!(
        missing.history_window_error().map(|error| error.code()),
        Some(-32016)
    );
}

#[tokio::test]
async fn excluded_archive_rows_are_readable_but_lookahead_bodies_are_not() {
    let history = payloads(4);
    let (_store, data, _directory) = session_with_history(&history).await;
    let id = WINDOW_SESSION.to_owned();
    data.apply_message_projections(
        &id,
        &[(
            history[1].id(),
            MessageFlags {
                excluded: true,
                ..Default::default()
            },
        )],
    )
    .await
    .unwrap();

    let rows = data
        .load_history_window_page(
            &id,
            &HistoryWindowPagePlan::open(
                HistorySource::Own,
                None,
                HistoryWindowDirection::Before,
                4,
                0,
                BUDGET,
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        loaded_ids(&rows),
        history.iter().map(PersistedPayload::id).collect::<Vec<_>>()
    );

    // 多读的 lookahead 行正文永不解码：把更早一行写成不可解码内容，页仍然成功。
    // 单独建一个会话，让不可解码行成为 lookahead（更早一侧），而不是本页条目。
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let lookahead_id = "window-lookahead".to_owned();
    data.save_new_session(&session(
        &lookahead_id,
        workspace.cwd.to_str().unwrap(),
        &workspace,
        frozen("window-lookahead"),
    ))
    .await
    .unwrap();
    let unreadable = MessageId::new();
    sqlx::query(
        "INSERT INTO messages (message_id, thread_id, role, content) VALUES (?1, ?2, ?3, ?4)",
    )
    .bind(unreadable.as_uuid().to_string())
    .bind(&lookahead_id)
    .bind("user")
    .bind("{\"this is not\": a persisted payload envelope")
    .execute(&store.database.pool)
    .await
    .unwrap();
    let visible = PersistedPayload::Message(BaseMessage::human("only the page is read"));
    data.append_history(&lookahead_id, std::slice::from_ref(&visible))
        .await
        .unwrap();
    let latest = data
        .load_history_window_page(
            &lookahead_id,
            &HistoryWindowPagePlan::open(
                HistorySource::Own,
                None,
                HistoryWindowDirection::Before,
                1,
                0,
                BUDGET,
            ),
        )
        .await
        .expect("lookahead body must not be decoded");
    assert_eq!(loaded_ids(&latest), vec![visible.id()]);
    assert_eq!(latest.before_cursor_anchor, Some(visible.id()));
}

#[tokio::test]
async fn oversized_entries_become_descriptors_without_decoding_the_body() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let id = WINDOW_SESSION.to_owned();
    data.save_new_session(&session(
        WINDOW_SESSION,
        workspace.cwd.to_str().unwrap(),
        &workspace,
        frozen(WINDOW_SESSION),
    ))
    .await
    .unwrap();
    let oversized = MessageId::new();
    sqlx::query(
        "INSERT INTO messages (message_id, thread_id, role, content) VALUES (?1, ?2, ?3, ?4)",
    )
    .bind(oversized.as_uuid().to_string())
    .bind(&id)
    .bind("user")
    .bind("this body is not a decodable history envelope and is larger than one byte")
    .execute(&store.database.pool)
    .await
    .unwrap();

    let rows = data
        .load_history_window_page(
            &id,
            &HistoryWindowPagePlan::open(
                HistorySource::Own,
                None,
                HistoryWindowDirection::Before,
                1,
                0,
                1,
            ),
        )
        .await
        .expect("oversize descriptors must not decode the body");
    assert_eq!(rows.entries.len(), 1);
    match &rows.entries[0] {
        HistoryWindowEntry::Oversized {
            entry_id,
            total_bytes,
            ..
        } => {
            assert_eq!(*entry_id, oversized);
            assert!(*total_bytes > 1);
        }
        HistoryWindowEntry::Loaded(_) => panic!("oversize body must not be loaded"),
    }
}

#[tokio::test]
async fn body_chunks_are_bounded_and_reconstruct_the_exact_bytes() {
    let payload = PersistedPayload::Message(BaseMessage::human("正文".repeat(64)));
    let expected = serialize_persisted_payload(&payload).unwrap();
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let id = WINDOW_SESSION.to_owned();
    data.save_new_session(&session(
        WINDOW_SESSION,
        workspace.cwd.to_str().unwrap(),
        &workspace,
        frozen(WINDOW_SESSION),
    ))
    .await
    .unwrap();
    data.append_history(&id, std::slice::from_ref(&payload))
        .await
        .unwrap();

    let bounds = data.history_window_bounds(&id).await.unwrap();
    let bounds_tail = bounds.own_latest.expect("non-empty session has a tail");
    let mut offset = 0;
    let mut collected = Vec::new();
    loop {
        let chunk = data
            .load_history_body_chunk(
                &id,
                HistorySource::Own,
                payload.id(),
                offset,
                7,
                Some(&bounds_tail),
                &peri_acp_types::history_window::HistoryReadTicket::new("ticket"),
            )
            .await
            .unwrap();
        chunk
            .validate(
                &peri_acp_types::history_window::HistoryEntryId::new(&id, payload.id()).encode(),
                offset,
                7,
            )
            .unwrap();
        collected.extend_from_slice(&chunk.bytes);
        match chunk.next_offset {
            Some(next) => {
                assert!(!chunk.complete);
                assert!(next > offset);
                offset = next;
            }
            None => {
                assert!(chunk.complete);
                break;
            }
        }
    }
    assert_eq!(collected, expected.into_bytes());

    let beyond = data
        .load_history_body_chunk(
            &id,
            HistorySource::Own,
            payload.id(),
            100_000,
            7,
            Some(&bounds_tail),
            &peri_acp_types::history_window::HistoryReadTicket::new("ticket"),
        )
        .await
        .unwrap_err();
    assert_eq!(
        beyond.history_window_error().map(|error| error.code()),
        Some(-32602)
    );
}

#[tokio::test]
async fn an_empty_snapshot_stays_empty_after_later_appends() {
    let (_store, data, _directory) = session_with_history(&[]).await;
    let id = WINDOW_SESSION.to_owned();
    let opened = data
        .load_history_window_page(
            &id,
            &HistoryWindowPagePlan::open(
                HistorySource::Own,
                None,
                HistoryWindowDirection::Before,
                2,
                0,
                BUDGET,
            ),
        )
        .await
        .unwrap();
    assert!(opened.entries.is_empty());
    assert!(opened.committed_tail.is_none());

    data.append_history(&id, &payloads(2)).await.unwrap();
    let stale = data
        .load_history_window_page(
            &id,
            &HistoryWindowPagePlan {
                intent: HistoryWindowReadIntent::Page,
                source: HistorySource::Own,
                anchor: None,
                direction: HistoryWindowDirection::Before,
                include_anchor: false,
                before_limit: 2,
                after_limit: 0,
                byte_budget: BUDGET,
                tail: HistoryWindowTailExpectation::Fixed(None),
            },
        )
        .await
        .unwrap();
    assert!(
        stale.entries.is_empty(),
        "old empty snapshot must not absorb new rows"
    );
    assert!(stale.committed_tail.is_none());
}

#[tokio::test]
async fn a_tail_from_another_session_is_rejected() {
    let history = payloads(2);
    let (_store, data, _directory) = session_with_history(&history).await;
    let id = WINDOW_SESSION.to_owned();
    let foreign = HistoryCommittedTail::new(&"other".to_owned(), history[0].id(), 1);
    let error = data
        .load_history_window_page(
            &id,
            &HistoryWindowPagePlan::page(
                HistorySource::Own,
                history[0].id(),
                HistoryWindowDirection::Before,
                1,
                BUDGET,
                Some(foreign),
            ),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.history_window_error().map(|error| error.code()),
        Some(-32011)
    );
}

#[tokio::test]
async fn body_chunk_is_bounded_by_the_snapshot_tail() {
    let first = PersistedPayload::Message(BaseMessage::human("old snapshot body"));
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let id = WINDOW_SESSION.to_owned();
    data.save_new_session(&session(
        WINDOW_SESSION,
        workspace.cwd.to_str().unwrap(),
        &workspace,
        frozen(WINDOW_SESSION),
    ))
    .await
    .unwrap();
    data.append_history(&id, std::slice::from_ref(&first))
        .await
        .unwrap();
    let old_tail = data
        .history_window_bounds(&id)
        .await
        .unwrap()
        .own_latest
        .unwrap();

    let later = PersistedPayload::Message(BaseMessage::human("appended after snapshot"));
    data.append_history(&id, std::slice::from_ref(&later))
        .await
        .unwrap();

    let error = data
        .load_history_body_chunk(
            &id,
            HistorySource::Own,
            later.id(),
            0,
            16,
            Some(&old_tail),
            &peri_acp_types::history_window::HistoryReadTicket::new("ticket"),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.history_window_error().map(|error| error.code()),
        Some(-32016),
        "body read must not cross the snapshot tail"
    );

    let fresh_tail = data
        .history_window_bounds(&id)
        .await
        .unwrap()
        .own_latest
        .unwrap();
    let chunk = data
        .load_history_body_chunk(
            &id,
            HistorySource::Own,
            later.id(),
            0,
            16,
            Some(&fresh_tail),
            &peri_acp_types::history_window::HistoryReadTicket::new("ticket"),
        )
        .await
        .expect("a fresh snapshot can read the appended entry");
    assert!(!chunk.bytes.is_empty());
}
