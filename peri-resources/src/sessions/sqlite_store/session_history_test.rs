use super::*;
use peri_acp_types::messages::MessageId;
use peri_acp_types::store::history::{rewind_clear_excluded_ids, rewind_keep_len, RewindFlagEntry};

#[tokio::test]
async fn test_projection_and_compaction_maintain_derived_views_in_the_same_write() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    data.save_new_session(&session("s-compact", &cwd, &workspace, frozen("compact")))
        .await
        .unwrap();
    let history = payloads(3);
    data.append_history(&"s-compact".to_owned(), &history)
        .await
        .unwrap();
    let updated_before = data
        .load_meta(&"s-compact".to_owned())
        .await
        .unwrap()
        .updated_at;

    // 一次性投影变更集：flags 与派生视图同事务生效。
    data.apply_message_projections(
        &"s-compact".to_owned(),
        &[(
            history[0].id(),
            peri_acp_types::store::MessageFlags {
                truncated: true,
                excluded: false,
                projection: None,
            },
        )],
    )
    .await
    .unwrap();
    let flags = data
        .load_snapshot(&"s-compact".to_owned())
        .await
        .unwrap()
        .flags;
    assert!(flags[&history[0].id()].truncated);
    let updated_after = data
        .load_meta(&"s-compact".to_owned())
        .await
        .unwrap()
        .updated_at;
    assert!(updated_after >= updated_before);

    // 指向别会话/不存在的条目：整体失败，不留部分写入。
    let error = data
        .apply_message_projections(
            &"s-compact".to_owned(),
            &[
                (
                    history[1].id(),
                    peri_acp_types::store::MessageFlags {
                        truncated: true,
                        excluded: true,
                        projection: None,
                    },
                ),
                (
                    peri_acp_types::messages::MessageId::new(),
                    peri_acp_types::store::MessageFlags::default(),
                ),
            ],
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        peri_acp_types::session_resources::SessionResourceErrorKind::InvalidInput { .. }
    ));
    let flags = data
        .load_snapshot(&"s-compact".to_owned())
        .await
        .unwrap()
        .flags;
    assert!(
        !flags
            .get(&history[1].id())
            .is_some_and(|flags| flags.truncated),
        "失败批次不得留下部分 flags"
    );

    // compaction：追加摘要消息与 flags 一次生效。
    let summary = BaseMessage::ai("summary");
    let summary_id = summary.id();
    data.apply_compaction(
        &"s-compact".to_owned(),
        &CompactionChange {
            flag_updates: vec![(
                history[2].id(),
                peri_acp_types::store::MessageFlags {
                    truncated: false,
                    excluded: true,
                    projection: None,
                },
            )],
            appended_messages: vec![summary],
        },
    )
    .await
    .unwrap();
    let snapshot = data.load_snapshot(&"s-compact".to_owned()).await.unwrap();
    assert_eq!(snapshot.payloads.len(), 4);
    assert_eq!(snapshot.payloads[3].id(), summary_id);
    assert!(snapshot.flags[&history[2].id()].excluded);
    assert_eq!(snapshot.meta.message_count, 4);

    // 归属校验：别会话的条目不能在本次 compaction 里被改掉。
    let error = data
        .apply_compaction(
            &"s-compact".to_owned(),
            &CompactionChange {
                flag_updates: vec![(
                    peri_acp_types::messages::MessageId::new(),
                    peri_acp_types::store::MessageFlags::default(),
                )],
                appended_messages: vec![],
            },
        )
        .await
        .unwrap_err();
    assert!(!matches!(
        error.kind(),
        peri_acp_types::session_resources::SessionResourceErrorKind::NotFound
    ));
}

#[tokio::test]
async fn test_rewind_boundaries_are_distinct_and_unknown_cutoffs_change_nothing() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    data.save_new_session(&session("s-rewind", &cwd, &workspace, frozen("rewind")))
        .await
        .unwrap();
    let history = payloads(4);
    data.append_history(&"s-rewind".to_owned(), &history)
        .await
        .unwrap();

    // 未知截止点：无变更（保留现有 rewind 语义）；本用例只验边界语义，不涉及 flags 重建。
    data.rewind_history(
        &"s-rewind".to_owned(),
        RewindBoundary::RemoveFrom(peri_acp_types::messages::MessageId::new()),
        &[],
    )
    .await
    .unwrap();
    assert_eq!(
        data.load_snapshot(&"s-rewind".to_owned())
            .await
            .unwrap()
            .payloads
            .len(),
        4
    );

    // 保留到目标：目标本身保留。
    data.rewind_history(
        &"s-rewind".to_owned(),
        RewindBoundary::KeepThrough(history[1].id()),
        &[],
    )
    .await
    .unwrap();
    let snapshot = data.load_snapshot(&"s-rewind".to_owned()).await.unwrap();
    assert_eq!(
        snapshot
            .payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>(),
        vec![history[0].id(), history[1].id()]
    );
    assert_eq!(snapshot.meta.message_count, 2);

    // 从目标开始移除：目标及之后的条目都不再存在。
    data.rewind_history(
        &"s-rewind".to_owned(),
        RewindBoundary::RemoveFrom(history[1].id()),
        &[],
    )
    .await
    .unwrap();
    let snapshot = data.load_snapshot(&"s-rewind".to_owned()).await.unwrap();
    assert_eq!(
        snapshot
            .payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>(),
        vec![history[0].id()]
    );
    assert_eq!(snapshot.meta.message_count, 1);
}

#[tokio::test]
async fn test_remove_history_entries_is_exact_and_idempotent() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    data.save_new_session(&session("s-remove", &cwd, &workspace, frozen("remove")))
        .await
        .unwrap();
    data.save_new_session(&session("s-other", &cwd, &workspace, frozen("other")))
        .await
        .unwrap();
    let history = payloads(2);
    let other = payloads(1);
    data.append_history(&"s-remove".to_owned(), &history)
        .await
        .unwrap();
    data.append_history(&"s-other".to_owned(), &other)
        .await
        .unwrap();

    // 别会话的条目不得被本次移除命中。
    let error = data
        .remove_history_entries(&"s-remove".to_owned(), &[other[0].id()])
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        peri_acp_types::session_resources::SessionResourceErrorKind::InvalidInput { .. }
    ));
    assert_eq!(
        data.load_snapshot(&"s-other".to_owned())
            .await
            .unwrap()
            .payloads
            .len(),
        1
    );

    data.remove_history_entries(&"s-remove".to_owned(), &[history[0].id()])
        .await
        .unwrap();
    assert_eq!(
        data.load_snapshot(&"s-remove".to_owned())
            .await
            .unwrap()
            .payloads
            .len(),
        1
    );
    // 已经不存在的条目：幂等，不报错。
    data.remove_history_entries(&"s-remove".to_owned(), &[history[0].id()])
        .await
        .unwrap();
}

// ─── metadata、resume、登记 ───────────────────────────────────────────────────

#[tokio::test]
async fn test_update_meta_applies_only_requested_fields() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    data.save_new_session(&session("s-meta", &cwd, &workspace, frozen("meta")))
        .await
        .unwrap();
    let before = data.load_meta(&"s-meta".to_owned()).await.unwrap();

    // 空 patch：不改任何字段，也不刷新时间戳。
    data.update_meta(&"s-meta".to_owned(), &SessionMetaPatch::default())
        .await
        .unwrap();
    let after = data.load_meta(&"s-meta".to_owned()).await.unwrap();
    assert_eq!(after.updated_at, before.updated_at);

    data.update_meta(
        &"s-meta".to_owned(),
        &SessionMetaPatch {
            title: Some(Some("renamed".to_owned())),
            status: Some(AgentStatus::Done),
            cancel_policy: Some(peri_acp_types::thread::CancelPolicy::Independent),
            config: Some(Some("{\"k\":1}".to_owned())),
        },
    )
    .await
    .unwrap();
    let updated = data.load_meta(&"s-meta".to_owned()).await.unwrap();
    assert_eq!(updated.title.as_deref(), Some("renamed"));
    assert_eq!(updated.agent_status, AgentStatus::Done);
    assert_eq!(
        updated.cancel_policy,
        peri_acp_types::thread::CancelPolicy::Independent
    );
    assert_eq!(updated.config.as_deref(), Some("{\"k\":1}"));
    // cwd/parent/计数不是定向更新能改的字段。
    assert_eq!(updated.cwd, before.cwd);
    assert_eq!(updated.parent_thread_id, before.parent_thread_id);
    assert_eq!(updated.created_at, before.created_at);

    data.update_meta(
        &"s-meta".to_owned(),
        &SessionMetaPatch {
            title: Some(None),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(data
        .load_meta(&"s-meta".to_owned())
        .await
        .unwrap()
        .title
        .is_none());

    let error = data
        .update_meta(
            &"absent".to_owned(),
            &SessionMetaPatch {
                status: Some(AgentStatus::Done),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        peri_acp_types::session_resources::SessionResourceErrorKind::NotFound
    ));
}

#[tokio::test]
async fn test_child_resume_record_round_trip() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    data.save_new_session(&session("s-resume", &cwd, &workspace, frozen("resume")))
        .await
        .unwrap();
    let mut child = session("s-resume-child", &cwd, &workspace, frozen("resume"));
    child.meta.parent_thread_id = Some("s-resume".to_owned());
    data.save_child(&ChildSnapshot {
        target: child,
        parent_id: "s-resume".to_owned(),
        root_id: "s-resume".to_owned(),
        inherited: Default::default(),
    })
    .await
    .unwrap();

    let record = data
        .load_child_resume_record(&"s-resume-child".to_owned())
        .await
        .unwrap();
    assert_eq!(record.status, AgentStatus::Active);
    assert!(record.claimed, "active 的 child 视为已被认领");

    data.store_child_resume_record(
        &"s-resume-child".to_owned(),
        &crate::sessions::data::ChildResumeRecord {
            status: AgentStatus::Done,
            claimed: false,
        },
    )
    .await
    .unwrap();
    let record = data
        .load_child_resume_record(&"s-resume-child".to_owned())
        .await
        .unwrap();
    assert_eq!(record.status, AgentStatus::Done);
    assert!(!record.claimed);
}

// ─── 只读与关闭 ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_read_only_store_serves_reads_and_refuses_every_mutation() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    data.save_new_session(&session("s-ro", &cwd, &workspace, frozen("ro")))
        .await
        .unwrap();
    let path = directory.path().join("threads.db");
    store.close().await;
    let reader = SqliteThreadStore::open_existing_read_only(&path)
        .await
        .unwrap();
    let read_only = SqliteSessionData::new(Arc::clone(&reader.database));

    assert!(read_only.load_snapshot(&"s-ro".to_owned()).await.is_ok());
    let error = read_only
        .save_new_session(&session("s-ro-2", &cwd, &workspace, frozen("ro")))
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        peri_acp_types::session_resources::SessionResourceErrorKind::ReadOnlyStore
    ));
    let error = read_only
        .append_history(&"s-ro".to_owned(), &payloads(1))
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        peri_acp_types::session_resources::SessionResourceErrorKind::ReadOnlyStore
    ));
    let error = read_only.delete_tree(&"s-ro".to_owned()).await.unwrap_err();
    assert!(matches!(
        error.kind(),
        peri_acp_types::session_resources::SessionResourceErrorKind::ReadOnlyStore
    ));
    // 收敛检查在只读下不写库：没有未决锚点时报告可重载。
    assert_eq!(
        read_only
            .recover_persistence(&"s-ro".to_owned())
            .await
            .unwrap(),
        PersistenceRecovery::Recovered
    );
}

#[tokio::test]
async fn test_close_stops_writes_and_keeps_history_readable() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    data.save_new_session(&session("s-close", &cwd, &workspace, frozen("close")))
        .await
        .unwrap();

    data.close().await.unwrap();
    let error = data
        .append_history(&"s-close".to_owned(), &payloads(1))
        .await
        .unwrap_err();
    assert!(
        matches!(
            error.kind(),
            peri_acp_types::session_resources::SessionResourceErrorKind::Unavailable { .. }
        ),
        "{error}"
    );
    assert!(data.load_snapshot(&"s-close".to_owned()).await.is_ok());
}

#[tokio::test]
async fn test_restart_preserves_canonical_snapshot_and_history() {
    let (_store, data, directory) = database().await;
    let store = SqliteThreadStore::new(directory.path().join("threads.db"))
        .await
        .unwrap();
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    data.save_new_session(&session("s-dirty", &cwd, &workspace, frozen("dirty")))
        .await
        .unwrap();
    store
        .append_message(
            &"s-dirty".to_owned(),
            BaseMessage::human("saved before restart"),
        )
        .await
        .unwrap();
    let before = data.load_snapshot(&"s-dirty".to_owned()).await.unwrap();
    drop(store);
    assert!(data.load_snapshot(&"s-dirty".to_owned()).await.is_ok());

    let reopened = SqliteThreadStore::new(directory.path().join("threads.db"))
        .await
        .unwrap();
    let after = data.load_snapshot(&"s-dirty".to_owned()).await.unwrap();
    assert_eq!(after.binding, before.binding);
    assert_eq!(after.frozen, before.frozen);
    assert_eq!(
        payload_bytes(&after.payloads),
        payload_bytes(&before.payloads)
    );
    reopened
        .append_message(
            &"s-dirty".to_owned(),
            BaseMessage::human("continued after restart"),
        )
        .await
        .unwrap();
    assert_eq!(
        reopened
            .load_messages(&"s-dirty".to_owned())
            .await
            .unwrap()
            .len(),
        2
    );
}

// ─── 有效视图与有界 flags ─────────────────────────────────────────────────────

#[tokio::test]
async fn test_active_snapshot_reads_only_unexcluded_entries() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    data.save_new_session(&session("s-active", &cwd, &workspace, frozen("active")))
        .await
        .unwrap();
    let history = payloads(3);
    data.append_history(&"s-active".to_owned(), &history)
        .await
        .unwrap();

    // 一条被排除、一条被截断：排除条目既不进有效 payload，也不进有效 flags；
    // 截断条目仍然有效（投影视图照常返回标记）。
    let summary = BaseMessage::ai("summary");
    let summary_id = summary.id();
    data.apply_compaction(
        &"s-active".to_owned(),
        &CompactionChange {
            flag_updates: vec![
                (
                    history[1].id(),
                    peri_acp_types::store::MessageFlags {
                        truncated: false,
                        excluded: true,
                        projection: None,
                    },
                ),
                (
                    history[2].id(),
                    peri_acp_types::store::MessageFlags {
                        truncated: true,
                        excluded: false,
                        projection: None,
                    },
                ),
            ],
            appended_messages: vec![summary],
        },
    )
    .await
    .unwrap();

    let active = data
        .load_active_snapshot(&"s-active".to_owned())
        .await
        .unwrap();
    assert_eq!(
        active
            .payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>(),
        vec![history[0].id(), history[2].id(), summary_id]
    );
    assert!(
        !active.flags.contains_key(&history[1].id()),
        "已排除条目的标记不出现在有效视图里"
    );
    assert!(active.flags[&history[2].id()].truncated);
    assert_eq!(active.meta.message_count, 4);

    // 完整读取仍然给出归档区：排除只影响有效视图，不改变存储事实。
    let full = data.load_snapshot(&"s-active".to_owned()).await.unwrap();
    assert_eq!(full.payloads.len(), 4);
    assert!(full.flags[&history[1].id()].excluded);
}

#[tokio::test]
async fn test_message_flags_read_is_bounded_to_the_given_ids() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    data.save_new_session(&session("s-flags", &cwd, &workspace, frozen("flags")))
        .await
        .unwrap();
    data.save_new_session(&session(
        "s-flags-other",
        &cwd,
        &workspace,
        frozen("flags-other"),
    ))
    .await
    .unwrap();
    let history = payloads(3);
    let other = payloads(1);
    data.append_history(&"s-flags".to_owned(), &history)
        .await
        .unwrap();
    data.append_history(&"s-flags-other".to_owned(), &other)
        .await
        .unwrap();
    data.apply_message_projections(
        &"s-flags".to_owned(),
        &[
            (
                history[0].id(),
                peri_acp_types::store::MessageFlags {
                    truncated: true,
                    excluded: false,
                    projection: None,
                },
            ),
            (
                history[1].id(),
                peri_acp_types::store::MessageFlags {
                    truncated: false,
                    excluded: true,
                    projection: None,
                },
            ),
        ],
    )
    .await
    .unwrap();

    // 只问其中一条：别的条目的标记不返回（范围有界，不扫全历史）。
    let flags = data
        .load_message_flags(&"s-flags".to_owned(), &[history[1].id()])
        .await
        .unwrap();
    assert_eq!(flags.len(), 1);
    assert!(flags[&history[1].id()].excluded);

    // 空集合：不查库，直接是空结果。
    assert!(data
        .load_message_flags(&"s-flags".to_owned(), &[])
        .await
        .unwrap()
        .is_empty());
    // 未标记的条目不在派生视图里（默认值不是标记）。
    assert!(data
        .load_message_flags(&"s-flags".to_owned(), &[history[2].id()])
        .await
        .unwrap()
        .is_empty());
    // 别会话的 id 不会命中：范围包含会话归属。
    assert!(data
        .load_message_flags(&"s-flags".to_owned(), &[other[0].id()])
        .await
        .unwrap()
        .is_empty());
}

// ─── rewind 有效视图重建（A19 矩阵） ──────────────────────────────────────────

/// 两次 Full 之后的存储状态：A–D 由第一次覆盖，S1 与 E、F 由第二次覆盖，S2 是第二次的摘要。
///
/// 顺序：`A B C D | S1 | E F | S2 | G H`（Full#2 排除当时所有未排除的 own 非 System 条目，
/// 因此 `S1` 也被它排除）。排除集合在 own 非 System 条目里恒为前缀。
struct TwoFullState {
    id: ThreadId,
    covered_by_first: Vec<MessageId>,
    first_summary: MessageId,
    between_fulls: Vec<MessageId>,
    second_summary: MessageId,
    after_second: Vec<MessageId>,
}

fn excluded_updates(payloads: &[PersistedPayload]) -> Vec<(MessageId, MessageFlags)> {
    payloads
        .iter()
        .map(|payload| {
            (
                payload.id(),
                MessageFlags {
                    excluded: true,
                    ..Default::default()
                },
            )
        })
        .collect()
}

async fn two_full_state(
    store: &SqliteThreadStore,
    data: &SqliteSessionData,
    directory: &TempDir,
    suffix: &str,
) -> TwoFullState {
    let workspace = workspace(store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    let id = format!("s-{suffix}");
    data.save_new_session(&session(&id, &cwd, &workspace, frozen(suffix)))
        .await
        .unwrap();

    let covered_by_first = payloads(4);
    data.append_history(&id, &covered_by_first).await.unwrap();
    let first_summary = BaseMessage::human("summary one");
    data.apply_compaction(
        &id,
        &CompactionChange {
            flag_updates: excluded_updates(&covered_by_first),
            appended_messages: vec![first_summary.clone()],
        },
    )
    .await
    .unwrap();

    let between_fulls = payloads(2);
    data.append_history(&id, &between_fulls).await.unwrap();
    let second_summary = BaseMessage::human("summary two");
    let mut second_batch = between_fulls.clone();
    second_batch.push(PersistedPayload::Message(first_summary.clone()));
    data.apply_compaction(
        &id,
        &CompactionChange {
            flag_updates: excluded_updates(&second_batch),
            appended_messages: vec![second_summary.clone()],
        },
    )
    .await
    .unwrap();

    let after_second = payloads(2);
    data.append_history(&id, &after_second).await.unwrap();

    TwoFullState {
        id,
        covered_by_first: covered_by_first.iter().map(PersistedPayload::id).collect(),
        first_summary: first_summary.id(),
        between_fulls: between_fulls.iter().map(PersistedPayload::id).collect(),
        second_summary: second_summary.id(),
        after_second: after_second.iter().map(PersistedPayload::id).collect(),
    }
}

/// ACP 侧的调用形状：在存储的全量 payload 上定位边界、算保留长度，再算要清零的 id
/// （[`rewind_clear_excluded_ids`] 是唯一规则实现），最后交给存储的同一次原子提交。
async fn rewind_effective_view(
    data: &SqliteSessionData,
    id: &ThreadId,
    boundary: RewindBoundary,
) -> peri_acp_types::session_resources::SessionResourceResult<
    peri_acp_types::session_resources::RewindOutcome,
> {
    let snapshot = data.load_snapshot(id).await?;
    let entries: Vec<RewindFlagEntry> = snapshot
        .payloads
        .iter()
        .map(|payload| {
            (
                payload.id(),
                snapshot
                    .flags
                    .get(&payload.id())
                    .cloned()
                    .unwrap_or_default(),
                matches!(payload.as_message(), Some(BaseMessage::System { .. })),
            )
        })
        .collect();
    let ids: Vec<MessageId> = entries.iter().map(|(id, _, _)| *id).collect();
    let keep_len = rewind_keep_len(&ids, boundary).unwrap_or(0);
    let clear = rewind_clear_excluded_ids(&entries, keep_len);
    data.rewind_history(id, boundary, &clear).await
}

/// 有效视图（冷恢复走的就是它）：未排除条目的 id 序列。
async fn effective_ids(data: &SqliteSessionData, id: &ThreadId) -> Vec<MessageId> {
    data.load_active_snapshot(id)
        .await
        .unwrap()
        .payloads
        .iter()
        .map(PersistedPayload::id)
        .collect()
}

async fn stored_ids(data: &SqliteSessionData, id: &ThreadId) -> Vec<MessageId> {
    data.load_snapshot(id)
        .await
        .unwrap()
        .payloads
        .iter()
        .map(PersistedPayload::id)
        .collect()
}

#[tokio::test]
async fn test_rewind_between_two_fulls_restores_every_retained_body() {
    let (store, data, directory) = database().await;
    let state = two_full_state(&store, &data, &directory, "rewind-between").await;

    // rewind 落在两次 Full 之间：保留到 E 为止（S1 也在保留前缀里，它已被 Full#2 排除）。
    rewind_effective_view(
        &data,
        &state.id,
        RewindBoundary::KeepThrough(state.between_fulls[0]),
    )
    .await
    .unwrap();

    let retained = state
        .covered_by_first
        .iter()
        .copied()
        .chain([state.first_summary, state.between_fulls[0]])
        .collect::<Vec<_>>();
    assert_eq!(stored_ids(&data, &state.id).await, retained);
    assert!(
        !stored_ids(&data, &state.id)
            .await
            .contains(&state.second_summary),
        "边界之后的摘要必须移除"
    );
    // 保留前缀整段失去依据（第二次 Full 的摘要被删）：原文必须重新有效，
    // 而「只删后置摘要」会让它们继续读不回来。
    assert_eq!(effective_ids(&data, &state.id).await, retained);
}

#[tokio::test]
async fn test_rewind_before_the_first_full_restores_every_retained_body() {
    let (store, data, directory) = database().await;
    let state = two_full_state(&store, &data, &directory, "rewind-before").await;

    // rewind 落在第一次 Full 覆盖的区间内：保留 A、B。
    rewind_effective_view(
        &data,
        &state.id,
        RewindBoundary::KeepThrough(state.covered_by_first[1]),
    )
    .await
    .unwrap();

    assert_eq!(
        effective_ids(&data, &state.id).await,
        state.covered_by_first[..2].to_vec(),
        "前缀里剩下的原文都重新有效"
    );
    let stored = stored_ids(&data, &state.id).await;
    assert_eq!(stored, state.covered_by_first[..2].to_vec());
}

#[tokio::test]
async fn test_rewind_after_the_second_full_keeps_the_summary_covered_prefix_excluded() {
    let (store, data, directory) = database().await;
    let state = two_full_state(&store, &data, &directory, "rewind-after").await;

    // rewind 落在第二次 Full 之后：S2 仍是有效摘要，前缀的排除依旧有依据。
    rewind_effective_view(
        &data,
        &state.id,
        RewindBoundary::KeepThrough(state.after_second[0]),
    )
    .await
    .unwrap();

    let stored = stored_ids(&data, &state.id).await;
    assert!(
        !stored.contains(&state.after_second[1]),
        "边界之后的条目必须移除"
    );
    assert!(stored.contains(&state.second_summary));
    // 存活摘要覆盖的前缀保持失效：不能把保留前缀的 excluded 全清。
    let effective = effective_ids(&data, &state.id).await;
    assert_eq!(effective, vec![state.second_summary, state.after_second[0]]);
    let flags = data.load_snapshot(&state.id).await.unwrap().flags;
    assert!(state.covered_by_first.iter().all(|id| flags[id].excluded));
    assert!(state.between_fulls.iter().all(|id| flags[id].excluded));
}

#[tokio::test]
async fn test_rewind_flag_rebuild_failure_leaves_the_store_unchanged() {
    let (store, data, directory) = database().await;
    let state = two_full_state(&store, &data, &directory, "rewind-atomic").await;
    let before_payloads = stored_ids(&data, &state.id).await;
    let before_flags = data.load_snapshot(&state.id).await.unwrap().flags;

    // 清零集合里混进不属于本会话的 id：删除已在同一事务里执行，失败必须整笔回滚。
    let boundary = RewindBoundary::KeepThrough(state.covered_by_first[1]);
    let mut plan = {
        let snapshot = data.load_snapshot(&state.id).await.unwrap();
        let entries: Vec<RewindFlagEntry> = snapshot
            .payloads
            .iter()
            .map(|payload| {
                (
                    payload.id(),
                    snapshot
                        .flags
                        .get(&payload.id())
                        .cloned()
                        .unwrap_or_default(),
                    false,
                )
            })
            .collect();
        let ids: Vec<MessageId> = entries.iter().map(|(id, _, _)| *id).collect();
        let keep_len = rewind_keep_len(&ids, boundary).unwrap();
        rewind_clear_excluded_ids(&entries, keep_len)
    };
    plan.push(MessageId::new());
    let error = data
        .rewind_history(&state.id, boundary, &plan)
        .await
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        peri_acp_types::session_resources::SessionResourceErrorKind::InvalidInput { .. }
    ));

    assert_eq!(
        stored_ids(&data, &state.id).await,
        before_payloads,
        "失败不得留下部分删除"
    );
    assert_eq!(
        data.load_snapshot(&state.id).await.unwrap().flags,
        before_flags,
        "失败不得留下部分 flags"
    );
}
