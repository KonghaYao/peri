//! 显式云端 C-03 生命周期与批内守卫实验（默认 `#[ignore]`）：真引擎上的可观察结果。
//!
//! | 实验 | 断言的事实 |
//! | --- | --- |
//! | 生命周期 | child resume 认领事实由状态派生；远端 legacy 接纳拒绝补造执行证据；删树移除整棵子树、二次删树 `NotFound`；有子会话的撤销被拒绝且一行都不删 |
//! | 批内守卫 | 谓词成立 → 整批回滚（效果一条不落、资格写也不留）；谓词不成立 → **一行都不插**（单行表仍只有身份行）且批照常提交 |
//!
//! 守卫实验用**生产同一条守卫语句**（`session_history::GUARD_MESSAGE_NOT_IN_SESSION_SQL`）驱动
//! 一个托管事务批，不另抄一份 SQL：谓词不成立时「守卫一行都不插」同样关键——若它在健康路径
//! 上插了行，所有正常写入都会失败。
//!
//! 历史行为实验见 [`super::cloud_history_tests`]。
//!
//! 安全与清理：只操作本轮 run 命名空间；结束用正常 mutation 路径删除本轮行并复核计数为 0；
//! 只输出计数/类别/布尔；合成数据由本轮 run 派生，不含真实历史或项目内容。
//!
//! ```text
//! PERI_CLOUD_URL_KEY=<url 变量名> PERI_CLOUD_TOKEN_KEY=<token 变量名> \
//!   cargo test -p peri-resources --lib -- --ignored --nocapture --test-threads=1 cloud_lifecycle_
//! ```

use std::collections::HashMap;
use std::path::PathBuf;

use peri_acp_types::messages::BaseMessage;
use peri_acp_types::session_resources::{
    ChildSnapshot, FrozenSnapshotBytes, FrozenState, SessionResourceErrorKind,
};
use peri_acp_types::store::{InheritedContext, PersistedPayload};
use peri_acp_types::thread::AgentStatus;
use peri_acp_types::workspace::ResolvedWorkspace;
use turso_serverless::Value;

use super::cloud_tests::{
    check, failure, run_counts, session_input, synth_binding, synth_thread, unique_run_label,
    with_cleanup, CloudTarget,
};
use super::ledger::{OperationId, OperationIdentity};
use super::mutation::{MutationOutcome, QualifiedMutation, RemoteStore, StoreAccess};
use super::session_history::{GUARD_MESSAGE_NOT_IN_SESSION_SQL, UPDATE_FLAGS_SQL};
use super::sql::StatementSpec;
use crate::sessions::data::{ChildResumeRecord, SessionDataPort};

const COUNT_META_ROWS_SQL: &str = "SELECT COUNT(*) FROM peri_store_meta WHERE singleton = 0";
const COUNT_RUN_LEDGER_ROWS_SQL: &str =
    "SELECT COUNT(*) FROM peri_op_ledger WHERE operation_id LIKE ?1";
const SELECT_TRUNCATED_SQL: &str =
    "SELECT truncated FROM messages WHERE thread_id = ?1 AND message_id = ?2";

/// 实验一：生命周期行为（child resume / legacy 接纳 / 删树 / 撤销）在真引擎上的往返。
#[tokio::test]
#[ignore = "显式 cloud 实验：需要已授权测试库的 .env，默认不跑"]
async fn cloud_lifecycle_round_trip() {
    let target = CloudTarget::load();
    let run = unique_run_label("peri-life");
    let mut out = target.out();
    out.push("experiment=lifecycle".to_owned());
    out.flush();
    let result = with_cleanup(&target, &run, || async {
        lifecycle_flow(&target, &run).await
    })
    .await;
    match result {
        Ok(()) => {
            let mut out = target.out();
            out.push("lifecycle=ok".to_owned());
            out.flush();
        }
        Err(message) => panic!("{message}"),
    }
}

async fn lifecycle_flow(target: &CloudTarget, run: &str) -> Result<(), String> {
    let writer = target
        .session_data(StoreAccess::ReadWrite)
        .await
        .map_err(failure)?;
    let created_at = chrono::Utc::now().to_rfc3339();
    let frozen = format!("{{\"frozen\":\"{run}\"}}");
    let binding = synth_binding();
    let root = synth_thread(&format!("{run}-lc-root"));
    let child = synth_thread(&format!("{run}-lc-child"));
    let parent = synth_thread(&format!("{run}-lc-parent"));
    let nested = synth_thread(&format!("{run}-lc-nested"));

    for session in [&root, &parent] {
        writer
            .save_new_session(&session_input(
                session.as_str(),
                &created_at,
                &binding,
                &frozen,
                None,
            ))
            .await
            .map_err(failure)?;
    }
    writer
        .save_child(&ChildSnapshot {
            target: session_input(
                child.as_str(),
                &created_at,
                &binding,
                &frozen,
                Some(root.as_str()),
            ),
            parent_id: root.clone(),
            root_id: root.clone(),
            inherited: InheritedContext {
                payloads: vec![PersistedPayload::Message(BaseMessage::human("inherited"))],
                flags: HashMap::new(),
            },
        })
        .await
        .map_err(failure)?;
    writer
        .save_child(&ChildSnapshot {
            target: session_input(
                nested.as_str(),
                &created_at,
                &binding,
                &frozen,
                Some(parent.as_str()),
            ),
            parent_id: parent.clone(),
            root_id: parent.clone(),
            inherited: InheritedContext {
                payloads: Vec::new(),
                flags: HashMap::new(),
            },
        })
        .await
        .map_err(failure)?;

    // child resume 认领事实：由状态派生，不是独立字段。
    writer
        .store_child_resume_record(
            &child,
            &ChildResumeRecord {
                status: AgentStatus::Active,
                claimed: true,
            },
        )
        .await
        .map_err(failure)?;
    let claimed = writer
        .load_child_resume_record(&child)
        .await
        .map_err(failure)?;
    check(
        claimed.status == AgentStatus::Active && claimed.claimed,
        "active status must read back as claimed",
    )?;
    writer
        .store_child_resume_record(
            &child,
            &ChildResumeRecord {
                status: AgentStatus::Done,
                claimed: false,
            },
        )
        .await
        .map_err(failure)?;
    let released = writer
        .load_child_resume_record(&child)
        .await
        .map_err(failure)?;
    check(
        released.status == AgentStatus::Done && !released.claimed,
        "terminal status must read back as not claimed",
    )?;

    // Remote legacy adoption cannot mint present-day evidence for old history.
    let workspace = ResolvedWorkspace {
        project_id: binding.project_id,
        workspace_id: binding.workspace_id,
        cwd: PathBuf::from("/tmp/peri-cloud-synth"),
        root: PathBuf::from("/tmp/peri-cloud-synth"),
        relative_cwd: PathBuf::from("sub"),
        discovery_snapshot: None,
    };
    let rejected = writer
        .adopt_legacy_session(
            &root,
            "/tmp/peri-cloud-synth",
            &workspace,
            &FrozenSnapshotBytes::new(format!("{{\"adopted\":\"{run}\"}}")),
        )
        .await
        .expect_err("remote legacy adoption must remain read-only");
    check(
        matches!(rejected.kind(), SessionResourceErrorKind::Unsupported),
        "remote legacy adoption must be unsupported",
    )?;
    let preserved = writer.load_snapshot(&root).await.map_err(failure)?;
    check(
        matches!(&preserved.frozen, FrozenState::Present(bytes) if bytes.as_str() == frozen),
        "rejected adoption must preserve the existing frozen snapshot",
    )?;

    // 删树：整棵子树（含根）消失；根不存在时是 NotFound，不是「成功但没删」。
    writer.delete_tree(&root).await.map_err(failure)?;
    for gone in [&root, &child] {
        let missing = writer
            .load_meta(gone)
            .await
            .expect_err("deleted session must be gone");
        check(
            matches!(missing.kind(), SessionResourceErrorKind::NotFound),
            "deleted session must read back as NotFound",
        )?;
    }
    let second_delete = writer
        .delete_tree(&root)
        .await
        .expect_err("deleting an absent session must fail");
    check(
        matches!(second_delete.kind(), SessionResourceErrorKind::NotFound),
        "deleting an absent session must be NotFound, not a silent success",
    )?;

    // 撤销：有子会话 → 拒绝且一行都不删；子会话清掉后 → 成功。
    let published = writer
        .revoke_unpublished_session(&parent)
        .await
        .expect_err("revoking a session with published children must fail");
    check(
        matches!(
            published.kind(),
            SessionResourceErrorKind::InvalidInput { .. }
        ),
        "revoking a session with children must be InvalidInput",
    )?;
    writer.load_meta(&parent).await.map_err(failure)?;
    writer.load_meta(&nested).await.map_err(failure)?;
    writer.delete_tree(&nested).await.map_err(failure)?;
    writer
        .revoke_unpublished_session(&parent)
        .await
        .map_err(failure)?;
    let revoked = writer
        .load_meta(&parent)
        .await
        .expect_err("revoked session must be gone");
    check(
        matches!(revoked.kind(), SessionResourceErrorKind::NotFound),
        "revoked session must read back as NotFound",
    )?;

    writer.close().await.map_err(failure)
}

/// 实验二：批内守卫在真引擎上的两条分支。
///
/// 用**生产同一条守卫语句**（`GUARD_MESSAGE_NOT_IN_SESSION_SQL`）驱动一个托管事务批：
///
/// - 谓词成立（目标不在本会话）→ 单行表主键冲突 → **整批回滚**：效果与资格写都不留；
/// - 谓词不成立（目标确实在）→ 守卫**一行都不插**，批照常提交、效果落地。
///
/// 第二条分支同样重要：守卫若在健康路径上插入了行，所有正常写入都会失败。
#[tokio::test]
#[ignore = "显式 cloud 实验：需要已授权测试库的 .env，默认不跑"]
async fn cloud_batch_guard_holds_on_real_engine() {
    let target = CloudTarget::load();
    let run = unique_run_label("peri-guard");
    let mut out = target.out();
    out.push("experiment=batch_guard".to_owned());
    out.flush();
    let result = with_cleanup(&target, &run, || async { guard_flow(&target, &run).await }).await;
    match result {
        Ok(()) => {
            let mut out = target.out();
            out.push("batch_guard=ok".to_owned());
            out.flush();
        }
        Err(message) => panic!("{message}"),
    }
}

async fn guard_flow(target: &CloudTarget, run: &str) -> Result<(), String> {
    let root = synth_thread(&format!("{run}-guard"));
    let created_at = chrono::Utc::now().to_rfc3339();
    let frozen = format!("{{\"frozen\":\"{run}\"}}");

    // 前置事实走 adapter 的正常写路径（不用裸 SQL 造数据）。
    let writer = target
        .session_data(StoreAccess::ReadWrite)
        .await
        .map_err(failure)?;
    writer
        .save_new_session(&session_input(
            root.as_str(),
            &created_at,
            &synth_binding(),
            &frozen,
            None,
        ))
        .await
        .map_err(failure)?;
    let entry = PersistedPayload::Message(BaseMessage::human("guarded entry"));
    writer
        .append_history(&root, std::slice::from_ref(&entry))
        .await
        .map_err(failure)?;
    writer.close().await.map_err(failure)?;

    let store = target
        .store(StoreAccess::ReadWrite)
        .await
        .map_err(failure)?;
    let root_id = Value::Text(root.as_str().to_owned());
    let entry_id = Value::Text(entry.id().as_uuid().to_string());
    let absent_id = Value::Text(uuid::Uuid::nil().to_string());
    // 基线：建会话与追加各自写了自己的资格行（它们的 operation id 里也带 run 标签，
    // 因为线程名由 run 派生），所以这里只能断言**增量**。
    let ledger_baseline = ledger_rows(&store, run).await?;

    // 健康分支：守卫谓词不成立（条目确实在本会话）→ 不插行、批提交、效果落地。
    let healthy = guard_batch(&store, run, "guard_healthy", &root_id, &entry_id, true).await?;
    check(
        matches!(healthy, MutationOutcome::Applied { .. }),
        "a healthy guard batch must commit",
    )?;
    check(
        scalar(&store, COUNT_META_ROWS_SQL, Vec::new()).await? == 1,
        "the guard inserted a row on the healthy branch",
    )?;
    let healthy_ledger = ledger_rows(&store, run).await?;
    check(
        healthy_ledger == ledger_baseline + 1,
        &format!(
            "the healthy guard batch changed the ledger row count from {ledger_baseline} to {healthy_ledger}"
        ),
    )?;
    let applied = scalar(
        &store,
        SELECT_TRUNCATED_SQL,
        vec![root_id.clone(), entry_id.clone()],
    )
    .await?;
    check(applied == 1, "the healthy batch effect did not land")?;

    // 守卫分支：谓词成立（目标不在本会话）→ 主键冲突中止整批。
    let fired = guard_batch(&store, run, "guard_fired", &root_id, &absent_id, false).await?;
    check(
        matches!(fired, MutationOutcome::NotApplied { .. }),
        "a fired guard must be reported as not applied, never as success",
    )?;
    check(
        scalar(&store, COUNT_META_ROWS_SQL, Vec::new()).await? == 1,
        "a fired guard left a row in the single-row table",
    )?;
    let fired_ledger = ledger_rows(&store, run).await?;
    check(
        fired_ledger == healthy_ledger,
        &format!(
            "a rolled-back batch changed the ledger row count from {healthy_ledger} to {fired_ledger}"
        ),
    )?;
    let rolled_back = scalar(
        &store,
        SELECT_TRUNCATED_SQL,
        vec![root_id.clone(), entry_id.clone()],
    )
    .await?;
    check(
        rolled_back == 1,
        "a rolled-back batch still changed the effect row",
    )?;

    // 重连复核：中止的批不得在半状态上留下任何痕迹。
    store.close().await.map_err(failure)?;
    let reader = target
        .session_data(StoreAccess::ReadOnly)
        .await
        .map_err(failure)?;
    let snapshot = reader.load_snapshot(&root).await.map_err(failure)?;
    check(
        snapshot.payloads.len() == 1 && snapshot.meta.message_count == 1,
        "the aborted batch changed the visible session state",
    )?;
    reader.close().await.map_err(failure)?;

    let store = target
        .store(StoreAccess::ReadWrite)
        .await
        .map_err(failure)?;
    let counts = run_counts(&store, run).await?;
    store.close().await.map_err(failure)?;
    check(
        counts.sessions == 1 && counts.messages == 1 && counts.ledger == ledger_baseline + 1,
        &format!(
            "guard probe left {} sessions / {} messages / {} ledger rows",
            counts.sessions, counts.messages, counts.ledger
        ),
    )
}

/// 一个托管事务批：生产守卫语句 + 一条效果语句（flags 改写）。
async fn guard_batch(
    store: &RemoteStore,
    run: &str,
    kind: &str,
    thread_id: &Value,
    message_id: &Value,
    truncated: bool,
) -> Result<MutationOutcome, String> {
    let identity = OperationIdentity::new(OperationId::scoped(run, kind), kind, &[run]);
    store
        .apply_qualified(&QualifiedMutation {
            identity,
            effects: vec![
                StatementSpec::new(
                    GUARD_MESSAGE_NOT_IN_SESSION_SQL,
                    vec![thread_id.clone(), message_id.clone()],
                ),
                StatementSpec::new(
                    UPDATE_FLAGS_SQL,
                    vec![
                        Value::Integer(i64::from(truncated)),
                        Value::Integer(0),
                        Value::Null,
                        thread_id.clone(),
                        message_id.clone(),
                    ],
                ),
            ],
        })
        .await
        .map_err(failure)
}

/// 本轮命名空间里的账本行数（含适配器自己的写入资格行）。
async fn ledger_rows(store: &RemoteStore, run: &str) -> Result<i64, String> {
    scalar(
        store,
        COUNT_RUN_LEDGER_ROWS_SQL,
        vec![Value::Text(format!("%{run}%"))],
    )
    .await
}

/// 只读单值读取（计数/布尔），读不出来即失败，不返回默认值。
async fn scalar(store: &RemoteStore, sql: &'static str, params: Vec<Value>) -> Result<i64, String> {
    let batches = store
        .read_batch(vec![StatementSpec::new(sql, params)])
        .await
        .map_err(failure)?;
    let mut batches = batches.into_iter();
    let mut rows = batches
        .next()
        .ok_or_else(|| "guard probe read returned no result set".to_owned())?;
    let mut row = rows
        .pop()
        .ok_or_else(|| "guard probe read returned no row".to_owned())?;
    match row.pop() {
        Some(Value::Integer(number)) => Ok(number),
        _ => Err("guard probe read returned no integer".to_owned()),
    }
}
