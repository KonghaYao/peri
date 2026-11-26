//! Lifecycle fixtures for a dedicated PostgreSQL integration database.
//! Set PERI_TEST_POSTGRES_URL when explicitly running the ignored database tests.

use std::future::Future;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use super::sql::Value;
use crate::sessions::local_port::LocalExecutionPort;
use peri_acp_types::session_resources::{
    FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionResourceError, SessionResourceResult,
};
use peri_acp_types::store::MessageFlags;
use peri_acp_types::thread::{CancelPolicy, ThreadId};
use peri_acp_types::workspace::{ResolvedWorkspace, SessionBinding};

use super::ledger::{LedgerRow, OperationId, OperationIdentity};
use super::mutation::{MutationOutcome, QualifiedMutation, RemoteStore, StoreAccess};
use super::session_data::RemoteSessionData;
use super::sql::StatementSpec;
use super::{RemoteEndpoint, SessionStoreCredential};

/// 输出缓冲：打印前校验每行不含凭证字面量。
pub(super) struct SafeOut {
    lines: Vec<String>,
    secrets: Vec<String>,
}

impl SafeOut {
    pub(super) fn new(secrets: Vec<String>) -> Self {
        Self {
            lines: Vec::new(),
            secrets,
        }
    }

    pub(super) fn push(&mut self, line: impl Into<String>) {
        let line = line.into();
        for secret in &self.secrets {
            if secret.len() >= 8 && line.contains(secret.as_str()) {
                // 消息本身不含凭证内容。
                panic!("probe output rejected: line would contain a credential literal");
            }
        }
        self.lines.push(line);
    }

    pub(super) fn flush(&self) {
        for line in &self.lines {
            println!("PROBE {line}");
        }
    }
}

pub(super) fn test_url() -> String {
    std::env::var("PERI_TEST_POSTGRES_URL")
        .expect("set PERI_TEST_POSTGRES_URL to a dedicated test database")
}

/// 本轮唯一 run 标签：只用十六进制与短横线，供本轮对象命名（不作为身份判据）。
pub(super) fn unique_run_label(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    format!("{prefix}-{nanos:x}-{}", &nonce[..8])
}

/// 已授权测试库的目标：端点、凭证与脱敏字面量。
///
/// 故意不实现 `Debug`（结构里含 locator）；需要输出时只用 [`Self::out`]。
pub(super) struct CloudTarget {
    endpoint: RemoteEndpoint,
    credential: SessionStoreCredential,
    redactions: Vec<String>,
}

impl CloudTarget {
    /// 载入配置：缺文件、缺选择器、缺键、locator 形状不可用都直接 panic。
    pub(super) fn load() -> Self {
        let raw_url = test_url();
        let credential = SessionStoreCredential::default();
        let endpoint = RemoteEndpoint::parse(&raw_url).unwrap_or_else(|error| {
            panic!("cloud experiment locator rejected: {error}");
        });
        Self {
            endpoint,
            credential,
            redactions: vec![raw_url],
        }
    }

    /// 已解析的远端端点（只在本 crate 内用于装配实验；不打印原文）。
    pub(super) fn endpoint(&self) -> &RemoteEndpoint {
        &self.endpoint
    }

    /// 已解析的凭证值（装配实验直接注入，不经过进程环境）。
    pub(super) fn credential(&self) -> &SessionStoreCredential {
        &self.credential
    }

    pub(super) fn out(&self) -> SafeOut {
        SafeOut::new(self.redactions.clone())
    }

    /// 连接可变路径；连接失败即失败，不降级成「跳过」。
    pub(super) async fn store(&self, access: StoreAccess) -> SessionResourceResult<RemoteStore> {
        RemoteStore::connect(&self.endpoint, &self.credential, access).await
    }

    /// 连接会话数据 adapter（C-03 行为）：打开即读回 store 身份，未初始化时按访问模式
    /// 决定是否创建本任务 schema；失败即失败，不降级成「跳过」。
    ///
    /// 打开事实只有三个：端点、凭证、访问模式。本机不再持有远端操作的日志（v10 撤销了
    /// 「发送前登记、确定终态才结清」的本机记录），adapter 因此不需要任何本机库。
    pub(super) async fn session_data(
        &self,
        access: StoreAccess,
    ) -> SessionResourceResult<Arc<RemoteSessionData>> {
        RemoteSessionData::open(&self.endpoint, &self.credential, access)
            .await
            .map(|(data, _initialization)| Arc::new(data))
    }
}

/// 未预期失败只给类别名（`SessionResourceError` 的 detail 已在该层脱敏）。
pub(super) fn remote_error_class(error: &SessionResourceError) -> String {
    format!("{:?}", error.kind())
}

// ─── 本轮 run 命名空间的共用夹具 ─────────────────────────────────────────────
//
// 所有显式云实验都只操作本轮 `run` 前缀下的行：结束前删除本轮**合成数据**（会话与消息）
// 并**复核计数为 0**（复核走新连接，见 [`cleanup_run`]）。语句是静态 SQL + 全绑定参数，
// `run` 只作为绑定值出现，不进入 SQL 文本。
//
// **收据不删**：`peri_op_ledger` 的每一行都是某次操作的封闭证据（终态 + 原收据），删除它
// 等于删掉「这件事发生过」——迟到的同 id 请求在证据不存在时无法被判为重复，而 P7 的口径
// 正是「收据不按 TTL 清理」。因此清理器里**没有**针对该表的删除/覆盖语句，收据是有意保留的
// 空间成本，复核时报告保留条数（[`check_cleanup_kept_receipts`]）。

pub(super) const DELETE_RUN_MESSAGES_SQL: &str = "DELETE FROM messages WHERE thread_id LIKE $1";
pub(super) const DELETE_RUN_BINDINGS_SQL: &str =
    "DELETE FROM session_bindings WHERE thread_id LIKE $1";
pub(super) const DELETE_RUN_SESSIONS_SQL: &str = "DELETE FROM threads WHERE id LIKE $1";
pub(super) const COUNT_RUN_SESSIONS_SQL: &str = "SELECT COUNT(*) FROM threads WHERE id LIKE $1";
pub(super) const COUNT_RUN_MESSAGES_SQL: &str =
    "SELECT COUNT(*) FROM messages WHERE thread_id LIKE $1";
pub(super) const COUNT_RUN_BINDINGS_SQL: &str =
    "SELECT COUNT(*) FROM session_bindings WHERE thread_id LIKE $1";
pub(super) const COUNT_RUN_LEDGER_SQL: &str =
    "SELECT COUNT(*) FROM peri_op_ledger WHERE operation_id LIKE $1";

/// 清理器的效果语句：只删本轮合成数据，**不碰收据**。
///
/// 单独成函数是为了让「清理器不删收据」这条契约能在离线测试里被断言（见本文件的
/// `test_cleanup_never_deletes_ledger_receipts`），而不是只靠运行时的计数观察。
pub(super) fn cleanup_effects(run: &str) -> Vec<StatementSpec> {
    let prefix = Value::Text(format!("{run}%"));
    vec![
        StatementSpec::new(DELETE_RUN_MESSAGES_SQL, vec![prefix.clone()]),
        StatementSpec::new(DELETE_RUN_BINDINGS_SQL, vec![prefix.clone()]),
        StatementSpec::new(DELETE_RUN_SESSIONS_SQL, vec![prefix]),
    ]
}

/// 清理后的复核（纯函数，离线可测）。
///
/// 两件事都必须成立：本轮**合成**数据为 0；本轮**收据**只增不减（更新后的计数不小于清理前
/// 的计数，且清理自身那次操作也会留下一条）。返回有意保留的收据条数，由调用方报告。
pub(super) fn check_cleanup_kept_receipts(
    before: &RunCounts,
    after: &RunCounts,
) -> Result<i64, String> {
    check(
        after.sessions == 0 && after.messages == 0 && after.bindings == 0,
        "run namespace still has rows after cleanup",
    )?;
    check(
        before.ledger >= 0 && after.ledger >= before.ledger,
        "cleanup must not delete operation receipts",
    )?;
    Ok(after.ledger)
}

pub(super) fn check(condition: bool, message: &str) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(message.to_owned())
    }
}

pub(super) fn failure(error: SessionResourceError) -> String {
    format!("unexpected failure: {}", remote_error_class(&error))
}

/// Isolated guard fixtures only compare binding identity, without creating a workspace.
pub(super) fn synth_binding() -> SessionBinding {
    SessionBinding {
        schema_version: peri_acp_types::workspace::SESSION_BINDING_VERSION,
        revision: 1,
        project_id: peri_acp_types::workspace::ProjectId::new(),
        workspace_id: peri_acp_types::workspace::WorkspaceId::new(),
        cwd_relative_to_workspace: "sub".into(),
    }
}

/// Run the same workspace observation used by a native deployment. Keep the
/// directory alive for the duration of the test so its evidence stays valid.
pub(super) async fn workspace(
    data: &Arc<RemoteSessionData>,
) -> Result<(tempfile::TempDir, ResolvedWorkspace), String> {
    let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
    let execution = super::execution::RemoteExecution::new(Arc::clone(data), false);
    let workspace = execution
        .resolve_workspace(directory.path())
        .await
        .map_err(|e| e.to_string())?;
    Ok((directory, workspace))
}

/// 合成一组 flags（实验里只关心 truncated/excluded）。
pub(super) fn synth_flags(truncated: bool, excluded: bool) -> MessageFlags {
    MessageFlags {
        truncated,
        excluded,
        projection: None,
    }
}

pub(super) fn synth_thread(label: &str) -> ThreadId {
    ThreadId::from(label)
}

/// 合成一条会话输入：全部内容由本轮 run 派生，不含真实历史或项目数据。
pub(super) fn session_input(
    thread_id: &str,
    created_at: &str,
    workspace: &ResolvedWorkspace,
    frozen: &str,
    parent: Option<&str>,
) -> NewSession {
    NewSession {
        thread_id: thread_id.to_owned(),
        created_at: created_at.to_owned(),
        meta: NewSessionMeta {
            title: Some(format!(
                "title-{}",
                &thread_id[thread_id.len().saturating_sub(4)..]
            )),
            cwd: workspace.cwd.to_string_lossy().into_owned(),
            parent_thread_id: parent.map(str::to_owned),
            hidden: false,
            cancel_policy: CancelPolicy::Cascade,
            snapshot_at_message_id: None,
        },
        binding: SessionBinding::from_workspace(workspace),
        frozen: FrozenSnapshotBytes::new(frozen),
    }
}

/// 跑实验体、无论成败都清理本轮对象，最后一起判定。
pub(super) async fn with_cleanup<F, Fut>(
    target: &CloudTarget,
    run: &str,
    body: F,
) -> Result<(), String>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    let outcome = body().await;
    let cleanup = cleanup_run(target, run).await;
    match (outcome, cleanup) {
        (Err(message), _) => Err(message),
        (Ok(()), Err(message)) => Err(message),
        (Ok(()), Ok(())) => Ok(()),
    }
}

/// 删除本轮**合成**数据（会话与消息）并复核；**收据一律保留**。
///
/// 不能只凭「删除返回成功」就宣称清理完成，也不能把收据算进「本轮行」：收据是封闭证据，
/// 删除它会破坏「迟到请求不得被当成新操作」的判据（见本文件顶部与 P7）。
///
/// 复核走**新连接**（见 [`counts_after_cleanup`]）：收据「还在」只有在一次全新打开上仍可
/// 读回才算事实，同一连接上的读回可能只是本地视图。
pub(super) async fn cleanup_run(target: &CloudTarget, run: &str) -> Result<(), String> {
    let store = target
        .store(StoreAccess::ReadWrite)
        .await
        .map_err(failure)?;
    let before = run_counts(&store, run).await?;
    let identity = OperationIdentity::new(
        OperationId::scoped(run, "session_cleanup"),
        "session_cleanup",
        &[run],
    );
    let outcome = store
        .apply_qualified(&QualifiedMutation {
            identity: identity.clone(),
            effects: cleanup_effects(run),
        })
        .await
        .map_err(failure)?;
    if !matches!(outcome, MutationOutcome::Applied { .. }) {
        return Err(format!("cleanup did not apply: {outcome:?}"));
    }
    store.close().await.map_err(failure)?;
    let after = counts_after_cleanup(target, run, &identity).await?;
    let retained = check_cleanup_kept_receipts(&before, &after)?;
    // 有意保留的收据条数（安全输出：只有数字）。
    let mut out = target.out();
    out.push(format!("retained_receipts={retained}"));
    out.flush();
    Ok(())
}

/// 清理之后在**新连接**上复核：本轮合成数据为 0，本轮收据仍在，且清理自身那张收据
/// 仍能经 adapter 读回（`Applied`）。
///
/// 这条路径同时覆盖两个方向：合成数据被真的删掉（不是只在本连接上消失），以及收据**没有**
/// 被清理语句顺手带走——收据只增不减，它的空间成本由 P7 口径承担。
async fn counts_after_cleanup(
    target: &CloudTarget,
    run: &str,
    cleanup: &OperationIdentity,
) -> Result<RunCounts, String> {
    let store = target.store(StoreAccess::ReadOnly).await.map_err(failure)?;
    let counts = run_counts(&store, run).await?;
    let receipt = store
        .resolve_operation(&cleanup.operation_id)
        .await
        .map_err(failure)?;
    store.close().await.map_err(failure)?;
    match receipt {
        LedgerRow::Applied { .. } => Ok(counts),
        other => Err(format!(
            "cleanup receipt must stay readable on a new connection: {other:?} (counts: sessions={} messages={} bindings={} ledger={})",
            counts.sessions, counts.messages, counts.bindings, counts.ledger
        )),
    }
}

pub(super) struct RunCounts {
    pub(super) sessions: i64,
    pub(super) messages: i64,
    pub(super) bindings: i64,
    pub(super) ledger: i64,
}

/// 本轮命名空间的计数（通过只读 SQL 复核，不经过 adapter 的读取行为）。
pub(super) async fn run_counts(store: &RemoteStore, run: &str) -> Result<RunCounts, String> {
    let batches = store
        .read_batch(vec![
            StatementSpec::new(COUNT_RUN_SESSIONS_SQL, vec![Value::Text(format!("{run}%"))]),
            StatementSpec::new(COUNT_RUN_MESSAGES_SQL, vec![Value::Text(format!("{run}%"))]),
            StatementSpec::new(COUNT_RUN_BINDINGS_SQL, vec![Value::Text(format!("{run}%"))]),
            StatementSpec::new(COUNT_RUN_LEDGER_SQL, vec![Value::Text(format!("%{run}%"))]),
        ])
        .await
        .map_err(failure)?;
    let mut counts = batches.into_iter().map(|mut rows| {
        rows.pop()
            .and_then(|mut row| row.pop())
            .and_then(|value| match value {
                Value::Integer(number) => Some(number),
                _ => None,
            })
            .unwrap_or(-1)
    });
    let (sessions, messages, bindings, ledger) = (
        counts.next().unwrap_or(-1),
        counts.next().unwrap_or(-1),
        counts.next().unwrap_or(-1),
        counts.next().unwrap_or(-1),
    );
    check(
        sessions >= 0 && messages >= 0 && bindings >= 0 && ledger >= 0,
        "run namespace counts are unreadable",
    )?;
    Ok(RunCounts {
        sessions,
        messages,
        bindings,
        ledger,
    })
}

// ─── 清理器的离线契约测试（不联网、不需要凭证）────────────────────────────────

/// 清理器只删本轮**合成**数据：效果语句里不得出现收据表，也不得把 run 前缀编进 SQL 文本。
#[test]
fn test_cleanup_never_deletes_ledger_receipts() {
    let run = "peri-run-0123456789";
    let effects = cleanup_effects(run);
    assert_eq!(
        effects.len(),
        3,
        "清理本轮合成数据的三条语句（子行先于父行）"
    );
    for spec in &effects {
        assert!(
            !spec.sql.contains("peri_op_ledger"),
            "清理器不得删除收据行：{}",
            spec.sql
        );
        assert!(
            spec.sql.starts_with("DELETE FROM threads")
                || spec.sql.starts_with("DELETE FROM messages")
                || spec.sql.starts_with("DELETE FROM session_bindings"),
            "清理器只删本轮合成的会话与消息：{}",
            spec.sql
        );
        assert!(
            !spec.sql.contains(run),
            "run 只作为绑定值出现，不进入 SQL 文本：{}",
            spec.sql
        );
        assert!(
            spec.params
                .iter()
                .any(|value| matches!(value, Value::Text(text) if text == &format!("{run}%"))),
            "本轮前缀必须是绑定参数"
        );
    }
}

/// 清理后的复核：合成数据必须为 0，收据只增不减，返回值是**有意保留**的条数。
#[test]
fn test_cleanup_retention_check_reports_retained_receipts() {
    let before = RunCounts {
        sessions: 1,
        messages: 3,
        bindings: 1,
        ledger: 2,
    };
    // 清理自身这次的收据也会落在账本里：保留条数因此可以增加，但绝不减少。
    let after = RunCounts {
        sessions: 0,
        messages: 0,
        bindings: 0,
        ledger: 3,
    };
    assert_eq!(check_cleanup_kept_receipts(&before, &after).unwrap(), 3);

    let lost = RunCounts {
        sessions: 0,
        messages: 0,
        bindings: 0,
        ledger: 1,
    };
    assert!(
        check_cleanup_kept_receipts(&before, &lost).is_err(),
        "收据被删掉必须失败，不静默通过"
    );

    let dirty = RunCounts {
        sessions: 1,
        messages: 0,
        bindings: 0,
        ledger: 3,
    };
    assert!(
        check_cleanup_kept_receipts(&before, &dirty).is_err(),
        "合成数据没删干净必须失败"
    );

    let orphan_binding = RunCounts {
        sessions: 0,
        messages: 0,
        bindings: 1,
        ledger: 3,
    };
    assert!(
        check_cleanup_kept_receipts(&before, &orphan_binding).is_err(),
        "绑定行没删干净必须失败"
    );
}
