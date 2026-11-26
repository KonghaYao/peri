//! 显式云端操作身份回归（默认 `#[ignore]`）：**同值往返与同边界复用必须真的生效**。
//!
//! 抓的是内容派生操作 id 的真实缺陷：id 由「store + 标签 + 内容摘要」决定时，第三次
//! 同内容调用会撞上前一次的 id，被当成历史重放**静默丢掉效果**。三条断言覆盖三个面：
//!
//! | 断言 | 抓的是什么 |
//! | --- | --- |
//! | 状态 A→B→A→B 落到 B | 第三次同内容更新必须生效，不是重放 |
//! | 标题 x→y→x→y 落到 y | 同上，命中定向 metadata 更新路径 |
//! | 同边界 rewind 后再追加再 rewind 只留一条 | 同边界复用必须真的裁剪，不是重放 |
//! | 每次调用都有自己的账本行 | 「一次领域调用 = 一次远端操作」在账本上可数 |
//! | `recover_persistence` 返回 `Recovered` | 已结清的 root 必须给出确定结论 |
//!
//! 安全与清理：只操作本轮 run 命名空间（操作 id 以 run 前缀的会话开头，故清理按 run
//! 前缀即命中本轮全部账本行）；结束用正常 mutation 路径删除本轮行并复核计数为 0；
//! 只输出计数/布尔；本机执行面库写在系统临时目录，不进仓库、不含真实历史或凭证。
//!
//! ```text
//! PERI_TEST_POSTGRES_URL=<test-database-url> \
//!   cargo test -p peri-resources --lib -- --ignored --nocapture --test-threads=1 cloud_identity_
//! ```

use super::sql::Value;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::session_resources::{RewindBoundary, SessionMetaPatch};
use peri_acp_types::store::PersistedPayload;
use peri_acp_types::thread::AgentStatus;

use super::cloud_tests::CloudTarget;
use super::mutation::StoreAccess;
use super::sql::StatementSpec;
use crate::sessions::data::SessionDataPort;

/// 本会话在远端账本上的行数（操作 id 以会话 id 开头，清理与核对共用这一约定）。
const COUNT_THREAD_LEDGER_SQL: &str =
    "SELECT COUNT(*) FROM peri_op_ledger WHERE operation_id LIKE $1";

/// 实验：同值往返与同边界复用在真引擎上的可观察结果。
#[tokio::test]
#[ignore = "requires PERI_TEST_POSTGRES_URL pointing to a dedicated PostgreSQL database"]
async fn cloud_operation_identity_survives_repeat_values() {
    let target = CloudTarget::load();
    let run = super::cloud_tests::unique_run_label("peri-id");
    let mut out = target.out();
    out.push("experiment=operation_identity".to_owned());
    out.push(format!("run_prefix_len={}", run.len()));
    out.flush();
    let result = super::cloud_tests::with_cleanup(&target, &run, || async {
        identity_flow(&target, &run).await
    })
    .await;
    match result {
        Ok(()) => {
            let mut out = target.out();
            out.push("operation_identity=ok".to_owned());
            out.flush();
        }
        Err(message) => panic!("{message}"),
    }
}

async fn identity_flow(target: &CloudTarget, run: &str) -> Result<(), String> {
    let writer = target
        .session_data(StoreAccess::ReadWrite)
        .await
        .map_err(super::cloud_tests::failure)?;
    let root = format!("{run}-id");
    let root_id = peri_acp_types::thread::ThreadId::from(root.as_str());
    let created_at = chrono::Utc::now().to_rfc3339();
    let frozen = format!("{{\"frozen\":\"{run}\"}}");
    let (_directory, workspace) = super::cloud_tests::workspace(&writer).await?;

    // 标题先给一个显式值：后面用「x → y → x → y」把同内容复用走到头。
    let mut new_session =
        super::cloud_tests::session_input(&root, &created_at, &workspace, &frozen, None);
    new_session.meta.title = Some("x".to_owned());
    writer
        .save_new_session_in_workspace(&new_session, &workspace)
        .await
        .map_err(super::cloud_tests::failure)?;

    // 状态 A(创建默认 Active) → Done → Active → Done：第三次与第一次**内容完全相同**。
    for status in [AgentStatus::Done, AgentStatus::Active, AgentStatus::Done] {
        writer
            .update_meta(
                &root_id,
                &SessionMetaPatch {
                    title: None,
                    status: Some(status),
                    cancel_policy: None,
                    config: None,
                },
            )
            .await
            .map_err(super::cloud_tests::failure)?;
    }
    // 标题 x → y → x → y：同样第 3 次与第 1 次内容相同。
    for title in ["y", "x", "y"] {
        writer
            .update_meta(
                &root_id,
                &SessionMetaPatch {
                    title: Some(Some(title.to_owned())),
                    status: None,
                    cancel_policy: None,
                    config: None,
                },
            )
            .await
            .map_err(super::cloud_tests::failure)?;
    }

    // 同边界复用：rewind 到 m1 之后追加 m3，再 rewind 到**同一边界**必须真的裁掉 m3。
    let first = PersistedPayload::Message(BaseMessage::human("identity first"));
    let second = PersistedPayload::Message(BaseMessage::ai("identity second"));
    writer
        .append_history(&root_id, &[first.clone(), second.clone()])
        .await
        .map_err(super::cloud_tests::failure)?;
    let boundary = RewindBoundary::KeepThrough(first.id());
    writer
        .rewind_history(&root_id, boundary, &[])
        .await
        .map_err(super::cloud_tests::failure)?;
    let third = PersistedPayload::Message(BaseMessage::human("identity third"));
    writer
        .append_history(&root_id, std::slice::from_ref(&third))
        .await
        .map_err(super::cloud_tests::failure)?;
    writer
        .rewind_history(&root_id, boundary, &[])
        .await
        .map_err(super::cloud_tests::failure)?;

    // 断言走新连接（只读打开），不读本连接的缓存。
    let reader = target
        .session_data(StoreAccess::ReadOnly)
        .await
        .map_err(super::cloud_tests::failure)?;
    let meta = reader
        .load_meta(&root_id)
        .await
        .map_err(super::cloud_tests::failure)?;
    super::cloud_tests::check(
        meta.title.as_deref() == Some("y"),
        "title must be the value of the last call, not a replayed earlier one",
    )?;
    let claim = reader
        .load_child_resume_record(&root_id)
        .await
        .map_err(super::cloud_tests::failure)?;
    super::cloud_tests::check(
        claim.status == AgentStatus::Done,
        "status must be the value of the last call, not a replayed earlier one",
    )?;
    let history = reader
        .load_session_history(&root_id)
        .await
        .map_err(super::cloud_tests::failure)?;
    super::cloud_tests::check(
        history.len() == 1 && history[0].id() == first.id(),
        "rewind to the same boundary must trim the appended entry, not be dropped as a replay",
    )?;

    // 全部结清、可恢复。
    let recovery = writer
        .recover_persistence(&root_id)
        .await
        .map_err(super::cloud_tests::failure)?;
    super::cloud_tests::check(
        matches!(
            recovery,
            peri_acp_types::session_resources::PersistenceRecovery::Recovered
        ),
        "a settled root must report Recovered",
    )?;

    // 账本行数：创建 1 + 状态 3 + 标题 3 + 追加 2 + rewind 2 = 11。
    let expected_operations = 11i64;
    let store = target
        .store(StoreAccess::ReadOnly)
        .await
        .map_err(super::cloud_tests::failure)?;
    let batches = store
        .read_batch(vec![StatementSpec::new(
            COUNT_THREAD_LEDGER_SQL,
            vec![Value::Text(format!("{root}.%"))],
        )])
        .await
        .map_err(super::cloud_tests::failure)?;
    let counted = batches
        .into_iter()
        .next()
        .and_then(|mut rows| rows.pop())
        .and_then(|mut row| row.pop())
        .and_then(|value| match value {
            Value::Integer(number) => Some(number),
            _ => None,
        })
        .unwrap_or(-1);
    store.close().await.map_err(super::cloud_tests::failure)?;
    super::cloud_tests::check(
        counted == expected_operations,
        "each domain call must leave exactly one ledger row of its own",
    )?;

    let mut out = target.out();
    out.push(format!("operations={counted}"));
    out.push("identity=ok".to_owned());
    out.flush();
    writer.close().await.map_err(super::cloud_tests::failure)?;
    reader.close().await.map_err(super::cloud_tests::failure)?;
    Ok(())
}
