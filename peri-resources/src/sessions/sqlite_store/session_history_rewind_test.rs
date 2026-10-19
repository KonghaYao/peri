use super::*;
use peri_acp_types::messages::MessageId;
use peri_acp_types::projection::MessageProjectionDirective;
use peri_acp_types::store::history::{rewind_clear_excluded_ids, rewind_keep_len, RewindFlagEntry};

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

/// 清零的语义是「原文重新有效」，不是「只改一个 bit」：`projection` 载荷（含只在
/// `excluded` 为真时被读取的 `superseded_by`）必须随清零一起消失，`truncated` 则原样保留。
///
/// 清载荷不丢有效渲染信息：生产里 `excluded: true` 的唯一写入点是 Full Compact
/// （`peri-agent/src/agent/compact_v2/full.rs`），写 flags 时用 `..Default::default()`
/// 整体覆盖，被排除的行本来就不带渲染指令。
#[tokio::test]
async fn test_rewind_clears_the_whole_projection_payload_of_restored_entries() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    let id: ThreadId = "s-rewind-payload".to_owned();
    data.save_new_session(&session(
        "s-rewind-payload",
        &cwd,
        &workspace,
        frozen("rewind-payload"),
    ))
    .await
    .unwrap();

    let covered = payloads(2);
    data.append_history(&id, &covered).await.unwrap();
    let summary = BaseMessage::human("summary");
    let summary_id = summary.id();
    // Full Compact 归档的实际形状：每条被排除的条目写下取代它的摘要 id。`policy_version`
    // 由存储原样搬运（resources 不解释它），测试取任意 u32；其中一条另带 `truncated`，
    // 它来自 Micro compact，不是排除的产物。
    let superseded = |truncated: bool| MessageFlags {
        truncated,
        excluded: true,
        projection: Some(MessageProjectionDirective {
            policy_version: 2,
            entries: Vec::new(),
            superseded_by: Some(summary_id),
        }),
    };
    data.apply_compaction(
        &id,
        &CompactionChange {
            flag_updates: vec![
                (covered[0].id(), superseded(true)),
                (covered[1].id(), superseded(false)),
            ],
            appended_messages: vec![summary.clone()],
        },
    )
    .await
    .unwrap();

    // 边界落在摘要之前的第二条上：摘要被本次 rewind 删掉，两条原文的排除一起失去依据。
    let outcome = rewind_effective_view(&data, &id, RewindBoundary::KeepThrough(covered[1].id()))
        .await
        .unwrap();
    assert_eq!(
        outcome,
        peri_acp_types::session_resources::RewindOutcome::Applied
    );

    let flags = data.load_snapshot(&id).await.unwrap().flags;
    let restored = flags.get(&covered[0].id()).cloned().unwrap_or_default();
    assert!(!restored.excluded, "原文必须重新有效");
    assert!(restored.projection.is_none(), "整条载荷随清零一并清掉");
    assert!(
        restored.superseded_by().is_none(),
        "正文已恢复的条目不得残留取代记录"
    );
    assert!(restored.truncated, "truncated 来自另一条路径，清零不得动它");
    assert_eq!(
        flags.get(&covered[1].id()).cloned().unwrap_or_default(),
        MessageFlags::default(),
        "清零后不再有任何非默认标记（含载荷）"
    );
    assert_eq!(
        stored_ids(&data, &id).await,
        vec![covered[0].id(), covered[1].id()]
    );
}

/// 只有**本次清零**的条目失去载荷：仍被存活摘要覆盖的排除、以及本来就没有标记的条目，
/// 必须逐位不变（含它们的 `superseded_by` 记录）。
#[tokio::test]
async fn test_rewind_leaves_untouched_entries_flags_bit_for_bit() {
    let (store, data, directory) = database().await;
    let workspace = workspace(&store, directory.path()).await;
    let cwd = workspace.cwd.to_string_lossy().into_owned();
    let id: ThreadId = "s-rewind-untouched".to_owned();
    data.save_new_session(&session(
        "s-rewind-untouched",
        &cwd,
        &workspace,
        frozen("rewind-untouched"),
    ))
    .await
    .unwrap();

    // 历史：`A B | S1 | P | E | S2 | C`。第一次 Full 排除 A、B（取代者是 S1），第二次排除 E
    // （取代者是 S2）；P 是两次 Full 之间追加的、从未被排除的普通条目，C 是最后一次追加的。
    let covered = payloads(2);
    data.append_history(&id, &covered).await.unwrap();
    let first_summary = BaseMessage::human("summary one");
    let supersede = |truncated: bool, summary: MessageId| MessageFlags {
        truncated,
        excluded: true,
        projection: Some(MessageProjectionDirective {
            policy_version: 2,
            entries: Vec::new(),
            superseded_by: Some(summary),
        }),
    };
    data.apply_compaction(
        &id,
        &CompactionChange {
            flag_updates: vec![
                (covered[0].id(), supersede(true, first_summary.id())),
                (covered[1].id(), supersede(false, first_summary.id())),
            ],
            appended_messages: vec![first_summary.clone()],
        },
    )
    .await
    .unwrap();
    let plain = payloads(1);
    data.append_history(&id, &plain).await.unwrap();
    let covered_by_second = payloads(1);
    data.append_history(&id, &covered_by_second).await.unwrap();
    let second_summary = BaseMessage::human("summary two");
    data.apply_compaction(
        &id,
        &CompactionChange {
            flag_updates: vec![(
                covered_by_second[0].id(),
                supersede(false, second_summary.id()),
            )],
            appended_messages: vec![second_summary.clone()],
        },
    )
    .await
    .unwrap();
    let tail = payloads(1);
    data.append_history(&id, &tail).await.unwrap();

    let before = data.load_snapshot(&id).await.unwrap().flags;
    // 边界落在 E 上：S2（E 的摘要）与它之后的 C 一起被删 ⇒ 清零集合恰好是 [E]；A、B 的 S1 仍在
    // 保留前缀里，排除依旧有依据，P 本来就没有标记。
    let outcome = rewind_effective_view(
        &data,
        &id,
        RewindBoundary::KeepThrough(covered_by_second[0].id()),
    )
    .await
    .unwrap();
    assert_eq!(
        outcome,
        peri_acp_types::session_resources::RewindOutcome::Applied
    );
    assert_eq!(
        stored_ids(&data, &id).await,
        vec![
            covered[0].id(),
            covered[1].id(),
            first_summary.id(),
            plain[0].id(),
            covered_by_second[0].id(),
        ],
        "边界之后的摘要与尾部条目被删"
    );

    let after = data.load_snapshot(&id).await.unwrap().flags;
    let mut expected = before.clone();
    // E 清零后没有任何非默认标记，派生视图不再列出它；其余条目必须逐位相同。
    expected.remove(&covered_by_second[0].id());
    assert_eq!(
        after, expected,
        "只有本次清零的条目改变，未触及的 entries 连载荷一起原样保留"
    );
    for (position, message) in covered.iter().enumerate() {
        let flags = after.get(&message.id()).cloned().unwrap_or_default();
        assert!(flags.excluded, "存活摘要 S1 覆盖的排除仍有依据");
        assert_eq!(
            flags.superseded_by(),
            Some(first_summary.id()),
            "未被清零的条目保留它的取代记录（第 {position} 条）"
        );
    }
    assert!(
        after
            .get(&covered[0].id())
            .cloned()
            .unwrap_or_default()
            .truncated,
        "A 的 truncated 未被改动"
    );
    assert!(
        !after.contains_key(&plain[0].id()),
        "从未被排除的条目不带任何标记"
    );
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
