//! 显式云端 C-03 历史行为实验（默认 `#[ignore]`）：追加/投影/compact/rewind/移除在真引擎上的可观察结果。
//!
//! | 断言 | 抓的是什么 |
//! | --- | --- |
//! | 顺序与计数重数、`title IS NULL` 时补自动标题 | 追加不是「写进去就算」：顺序、计数、标题都要在读回时成立 |
//! | 批内重复 id、撞已有行主键都被拒绝 | 冲突不得静默忽略，也不得部分落行 |
//! | 撞主键的批**一条都没落**（重连后同读不到） | 批内约束失败必须整批回滚 |
//! | 投影/flags 定向生效、compact 追加 + 重数 | flags 是派生视图，读取要按同一套规则还原 |
//! | rewind 两个显式边界、未知边界无变更、精确移除幂等、跨会话拒绝 | 边界语义与本机一致，跨会话不得命中 |
//!
//! 断言用的读取走新连接（`StoreAccess::ReadOnly`）。生命周期与批内守卫实验见
//! [`super::cloud_lifecycle_tests`]。
//!
//! 安全与清理：只操作本轮 run 命名空间；结束用正常 mutation 路径删除本轮行并复核计数为 0；
//! 只输出计数/类别/布尔；合成数据由本轮 run 派生，不含真实历史或项目内容。
//!
//! ```text
//! PERI_CLOUD_URL_KEY=<url 变量名> PERI_CLOUD_TOKEN_KEY=<token 变量名> \
//!   cargo test -p peri-resources --lib -- --ignored --nocapture --test-threads=1 cloud_history_
//! ```

use peri_acp_types::messages::BaseMessage;
use peri_acp_types::session_resources::{RewindBoundary, SessionResourceErrorKind};
use peri_acp_types::store::{CompactionChange, PersistedPayload};

use super::cloud_tests::{
    check, failure, session_input, synth_binding, synth_flags, synth_thread, unique_run_label,
    with_cleanup, CloudTarget,
};
use super::mutation::StoreAccess;
use crate::sessions::data::SessionDataPort;

/// 实验：历史行为（追加/投影/compact/rewind/移除）在真引擎上的往返。
#[tokio::test]
#[ignore = "显式 cloud 实验：需要已授权测试库的 .env，默认不跑"]
async fn cloud_history_behaviors_round_trip() {
    let target = CloudTarget::load();
    let run = unique_run_label("peri-hist");
    let mut out = target.out();
    out.push("experiment=history".to_owned());
    out.push(format!("run_prefix_len={}", run.len()));
    out.flush();
    let result = with_cleanup(&target, &run, || async {
        history_flow(&target, &run).await
    })
    .await;
    match result {
        Ok(()) => {
            let mut out = target.out();
            out.push("history=ok".to_owned());
            out.flush();
        }
        Err(message) => panic!("{message}"),
    }
}

async fn history_flow(target: &CloudTarget, run: &str) -> Result<(), String> {
    let writer = target
        .session_data(StoreAccess::ReadWrite)
        .await
        .map_err(failure)?;
    let root = synth_thread(&format!("{run}-hist"));
    let created_at = chrono::Utc::now().to_rfc3339();
    let frozen = format!("{{\"frozen\":\"{run}\"}}");
    let binding = synth_binding();

    // 标题故意留空：自动标题规则只在 `title IS NULL` 时补一次。
    let mut new_session = session_input(root.as_str(), &created_at, &binding, &frozen, None);
    new_session.meta.title = None;
    writer
        .save_new_session(&new_session)
        .await
        .map_err(failure)?;

    let first = PersistedPayload::Message(BaseMessage::human("first question"));
    let second = PersistedPayload::Message(BaseMessage::ai("second answer"));
    writer
        .append_history(&root, &[first.clone(), second.clone()])
        .await
        .map_err(failure)?;

    // 批内重复 id：发请求前就拒绝（同一文案），不落任何行。
    let repeated = PersistedPayload::Message(BaseMessage::human("repeated"));
    let duplicate = writer
        .append_history(&root, &[repeated.clone(), repeated.clone()])
        .await
        .expect_err("a batch that repeats a message id must fail");
    check(
        matches!(
            duplicate.kind(),
            SessionResourceErrorKind::InvalidInput { .. }
        ),
        "in-batch duplicate id must be InvalidInput",
    )?;

    // 撞已有行的全局主键：**批内**约束失败 → 整批回滚，新条目一条都不落。
    let must_not_land = PersistedPayload::Message(BaseMessage::human("must not land"));
    let conflict = writer
        .append_history(&root, &[must_not_land.clone(), first.clone()])
        .await
        .expect_err("a batch that conflicts with an existing primary key must fail");
    check(
        matches!(
            conflict.kind(),
            SessionResourceErrorKind::InvalidInput { .. }
        ),
        "primary key conflict must be InvalidInput",
    )?;

    // 同内容、不同消息 id 的追加：两批都是新条目，必须都落地。
    // （操作身份只按内容摘要时，第二批会撞上第一批的操作 id，被当成重放静默跳过。）
    let repeat_one = PersistedPayload::Message(BaseMessage::human("identical content"));
    let repeat_two = PersistedPayload::Message(BaseMessage::human("identical content"));
    writer
        .append_history(&root, &[repeat_one.clone(), repeat_two.clone()])
        .await
        .map_err(failure)?;

    // 重连（只读）复核：四条、顺序稳定、计数重数、自动标题已补。
    let reader = target
        .session_data(StoreAccess::ReadOnly)
        .await
        .map_err(failure)?;
    let snapshot = reader.load_snapshot(&root).await.map_err(failure)?;
    check(
        snapshot
            .payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>()
            == vec![first.id(), second.id(), repeat_one.id(), repeat_two.id()],
        "appended payloads are missing, reordered, or partially landed",
    )?;
    check(
        !snapshot
            .payloads
            .iter()
            .any(|payload| payload.id() == must_not_land.id()),
        "a rolled-back batch left a row behind",
    )?;
    check(
        snapshot.meta.message_count == 4,
        "message_count is not the recount of the stored rows",
    )?;
    check(
        snapshot.meta.title.as_deref() == Some("first question"),
        "automatic title was not filled from the first human message",
    )?;
    check(
        snapshot.flags.is_empty(),
        "default flags must not be reported as a derived view",
    )?;

    // 投影：定向 flags。
    writer
        .apply_message_projections(&root, &[(first.id(), synth_flags(true, false))])
        .await
        .map_err(failure)?;
    // 目标不属于本会话：拒绝，且不改动已经生效的那一条。
    let foreign = PersistedPayload::Message(BaseMessage::human("foreign"));
    let missing = writer
        .apply_message_projections(&root, &[(foreign.id(), synth_flags(true, true))])
        .await
        .expect_err("a projection target outside this session must fail");
    check(
        matches!(
            missing.kind(),
            SessionResourceErrorKind::InvalidInput { .. }
        ),
        "projection target outside the session must be InvalidInput",
    )?;

    // compact：flag 更新 + 追加摘要，一次批内全部生效。
    let summary = BaseMessage::ai("summarized");
    let repeated_summary = BaseMessage::ai("summarized");
    writer
        .apply_compaction(
            &root,
            &CompactionChange {
                flag_updates: vec![(second.id(), synth_flags(false, true))],
                appended_messages: vec![summary.clone()],
            },
        )
        .await
        .map_err(failure)?;
    // 第二次 compact：摘要正文相同但消息 id 不同，同样必须落地（不能被当成重放跳过）。
    writer
        .apply_compaction(
            &root,
            &CompactionChange {
                flag_updates: vec![(second.id(), synth_flags(false, true))],
                appended_messages: vec![repeated_summary.clone()],
            },
        )
        .await
        .map_err(failure)?;
    let compacted = reader.load_snapshot(&root).await.map_err(failure)?;
    check(
        compacted
            .flags
            .get(&first.id())
            .is_some_and(|f| f.truncated),
        "projection flag is not visible after reconnect",
    )?;
    check(
        compacted
            .flags
            .get(&second.id())
            .is_some_and(|f| f.excluded),
        "compaction flag update is not visible after reconnect",
    )?;
    check(
        compacted
            .payloads
            .iter()
            .any(|payload| payload.id() == repeated_summary.id()),
        "a repeated compaction summary was dropped as a replay",
    )?;
    check(
        compacted.payloads.len() == 6 && compacted.meta.message_count == 6,
        "compaction appended message or recount did not land",
    )?;

    // rewind：保留到第二条（删除 ordinal 更大的条目）。
    writer
        .rewind_history(&root, RewindBoundary::KeepThrough(second.id()))
        .await
        .map_err(failure)?;
    let kept = reader.load_snapshot(&root).await.map_err(failure)?;
    check(
        kept.payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>()
            == vec![first.id(), second.id()]
            && kept.meta.message_count == 2,
        "rewind keep-through did not leave exactly the prefix",
    )?;

    // 未知边界：保持无变更（仍是与本机同一语义）。
    writer
        .rewind_history(&root, RewindBoundary::KeepThrough(foreign.id()))
        .await
        .map_err(failure)?;
    let unchanged = reader.load_snapshot(&root).await.map_err(failure)?;
    check(
        unchanged.payloads.len() == 2 && unchanged.meta.message_count == 2,
        "rewind with an unknown boundary must not change anything",
    )?;

    // RemoveFrom：从目标开始移除。
    writer
        .rewind_history(&root, RewindBoundary::RemoveFrom(second.id()))
        .await
        .map_err(failure)?;
    let removed = reader.load_snapshot(&root).await.map_err(failure)?;
    check(
        removed
            .payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>()
            == vec![first.id()],
        "rewind remove-from did not drop the boundary entry and its successors",
    )?;

    // 精确移除：一次性删除 + 重复删除幂等。
    writer
        .remove_history_entries(&root, &[first.id()])
        .await
        .map_err(failure)?;
    writer
        .remove_history_entries(&root, &[first.id()])
        .await
        .map_err(failure)?;
    let empty = reader.load_snapshot(&root).await.map_err(failure)?;
    check(
        empty.payloads.is_empty() && empty.meta.message_count == 0,
        "exact removal did not clear the history and its count",
    )?;

    // 跨会话条目：拒绝，且不删别的会话的行。
    let other = synth_thread(&format!("{run}-other"));
    writer
        .save_new_session(&session_input(
            other.as_str(),
            &created_at,
            &binding,
            &frozen,
            None,
        ))
        .await
        .map_err(failure)?;
    let other_message = PersistedPayload::Message(BaseMessage::human("other session entry"));
    writer
        .append_history(&other, std::slice::from_ref(&other_message))
        .await
        .map_err(failure)?;
    let cross = writer
        .remove_history_entries(&root, &[other_message.id()])
        .await
        .expect_err("removing another session's entry must fail");
    check(
        matches!(cross.kind(), SessionResourceErrorKind::InvalidInput { .. }),
        "cross-session removal must be InvalidInput",
    )?;
    let other_kept = reader.load_snapshot(&other).await.map_err(failure)?;
    check(
        other_kept.payloads.len() == 1,
        "cross-session removal touched the other session",
    )?;

    writer.close().await.map_err(failure)
}
