use super::*;

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

    // 未知截止点：无变更（保留现有 rewind 语义）。
    data.rewind_history(
        &"s-rewind".to_owned(),
        RewindBoundary::RemoveFrom(peri_acp_types::messages::MessageId::new()),
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
