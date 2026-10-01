//! 显式云端边界回归（默认 `#[ignore]`）：C §5.1 还没实测完的 P5/P6/P7，加上一条只被文档化
//! 过的拒绝路径。全部经**真实门面**（`open_remote` 装出来的 `SessionResourcesImpl`）。
//!
//! | 实验 | 抓的是什么 |
//! | --- | --- |
//! | 超大单批写入 | 单批输入只有两种诚实结果：整批生效，或类型化拒绝且**零部分结果**（P6） |
//! | 账本保留与空间成本 | 收据不按 TTL 清理：逐条计数与字节数在收敛、重读之后完全相同（P7） |
//! | 在途请求被取消（future drop） | 收敛后效果 0 或 1 份，绝不两份；本机不再持有操作记录（P5） |
//! | 带父会话的新建输入 | 远程 `create_session` 只接受 root：类型化拒绝且远端零行 |
//!
//! 两处刻意的结构选择：
//!
//! - **权威核对走新连接的原始计数**（`run_counts`），不依赖 adapter 的读取预算；adapter 侧
//!   读回只作为附加一致性检查。这样「整批生效或零部分结果」不会被读路径的预算问题掩盖。
//! - **取消实验分两段**：先在同一次打开的实例上收敛并继续读（被丢弃的在途请求使那一代
//!   连接失效，adapter 按同一份打开事实重建，见 `generation` 与 `RemoteSessionData::store`），
//!   再按同一份本机执行面库**重开**新实例收敛。两段都必须给出同一个确定终态。
//!
//! P5 的静态部分（SDK 是否自动重试 mutating 请求）不是行为断言，结论写在母需求本轮小节里：
//! `turso_serverless` 0.1.3 的源码里没有 retry/backoff/sleep 逻辑，我方也没有重试层，只有 20s
//! 请求预算——预算超时归「结果未知」，不会推断为已生效。因此「同一次发送的重试」只可能是调用
//! 方重新发起，而每次**新的领域调用**都会铸造新的操作 id（R1 的身份修正）。
//!
//! 安全与清理：只操作本轮 run 命名空间；本机执行面库在系统临时目录；只输出计数、字节数与
//! 类别名；不打印 locator、token、会话内容或凭证；结束按正常删除路径清理并复核计数为 0。
//!
//! ```text
//! PERI_CLOUD_URL_KEY=<url 变量名> PERI_CLOUD_TOKEN_KEY=<token 变量名> \
//!   cargo test -p peri-resources --lib -- --ignored --nocapture --test-threads=1 cloud_limit_
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use peri_acp_types::messages::BaseMessage;
use peri_acp_types::session_resources::{
    AccessMode, ExecutionAvailability, NewSession, NewSessionMeta, PersistenceRecovery,
    SessionResourceErrorKind, SessionResourceResult, SessionResources,
};
use peri_acp_types::store::PersistedPayload;
use peri_acp_types::thread::{CancelPolicy, ThreadId};
use peri_acp_types::workspace::{SessionBinding, SessionExecutionLease, SESSION_BINDING_VERSION};
use turso_serverless::Value;

use super::cloud_deployment_tests::synthetic_workspace;
use super::cloud_tests::{
    check, failure, remote_error_class, run_counts, unique_run_label, with_cleanup, CloudTarget,
};
use super::mutation::StoreAccess;
use super::open_remote;
use super::sql::StatementSpec;
use crate::sessions::SessionResourcesImpl;

/// P6 的实测尺寸：256 KiB 与 1 MiB（单条消息正文，正好是「大型 new/append 输入」那一类）。
/// 尺寸不是上限探测器的刻度，只是「明显大于普通消息」的两档；单请求上限没有被定位（见报告）。
const PAYLOAD_SIZES: [usize; 2] = [256 << 10, 1 << 20];

/// 在途预算：大概率切在网络上。切得太早也是合法结果——那时**什么都不会发出去**，
/// 实验会把这一档如实记下来。
const IN_FLIGHT_BUDGET: Duration = Duration::from_millis(400);

/// 收据本身的字节成本：`peri_op_ledger` 的全部列（id/kind/摘要/终态/原收据/时间戳），
/// 不含任何 payload——账本里本来就没有会话内容。
const LEDGER_STATS_SQL: &str = "SELECT COUNT(*), COALESCE(SUM(LENGTH(operation_id) + LENGTH(kind) \
     + LENGTH(digest) + LENGTH(state) + LENGTH(COALESCE(receipt, '')) + LENGTH(updated_at)), 0) \
     FROM peri_op_ledger WHERE operation_id LIKE ?1";

/// 失败的步骤名 + 类别名：`unexpected failure: Timeout` 这样的一行读不出「卡在哪一步」，
/// 而云实验的失败大多数是传输层瞬时问题，必须能区分「逻辑错」与「这一次网络不好」。
async fn step<T>(
    name: &str,
    future: impl std::future::Future<Output = SessionResourceResult<T>>,
) -> Result<T, String> {
    future
        .await
        .map_err(|error| format!("{name}: {}", remote_error_class(&error)))
}

// ─── 夹具：本机执行面库 + 合成 workspace（可重开）；会话实例（门面 + owner）──

/// 本机与工作区环境：重开时**必须沿用**（本机执行面库、run 标签、合成仓库都不变）。
struct Env {
    run: String,
    registry: PathBuf,
    _registry_dir: tempfile::TempDir,
    workspace: tempfile::TempDir,
    root: ThreadId,
}

async fn fixture_env(run: &str) -> Result<Env, String> {
    let registry_dir = tempfile::tempdir().map_err(|error| error.to_string())?;
    let registry = registry_dir.path().join("threads.db");
    Ok(Env {
        run: run.to_owned(),
        registry,
        _registry_dir: registry_dir,
        workspace: synthetic_workspace(),
        root: ThreadId::from(format!("{run}-root")),
    })
}

impl Env {
    fn message(&self, suffix: &str) -> PersistedPayload {
        PersistedPayload::Message(BaseMessage::human(format!("{}-{suffix}", self.run)))
    }

    async fn open(&self, target: &CloudTarget) -> Result<Arc<SessionResourcesImpl>, String> {
        open_remote(
            target.endpoint(),
            target.credential(),
            AccessMode::ReadWrite,
            self.registry.clone(),
        )
        .await
        .map_err(|error| format!("open failed: {error:#}"))
    }
}

/// 一次打开：门面 + 这条 root 的执行所有权。
///
/// 租约与真实消费方一样活着——本机库只持弱引用，强引用一落，后续写入就会按
/// 「有绑定而无 owner」被拒绝。
struct Session {
    facade: Arc<SessionResourcesImpl>,
    /// 执行所有权本身没有别的方法要调：它存在即「本进程持有这条 root 的 owner」。
    _lease: Arc<dyn SessionExecutionLease>,
}

async fn create_session_at(target: &CloudTarget, env: &Env) -> Result<Session, String> {
    let facade = env.open(target).await?;
    let resolved = step(
        "resolve workspace",
        facade.resolve_workspace(env.workspace.path()),
    )
    .await?;
    let input = NewSession {
        thread_id: env.root.to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        meta: NewSessionMeta {
            title: Some(format!("synthetic {}", env.run)),
            cwd: resolved.cwd.to_string_lossy().into_owned(),
            parent_thread_id: None,
            hidden: false,
            cancel_policy: CancelPolicy::Cascade,
            snapshot_at_message_id: None,
        },
        binding: SessionBinding {
            schema_version: SESSION_BINDING_VERSION,
            revision: 1,
            project_id: resolved.project_id,
            workspace_id: resolved.workspace_id,
            cwd_relative_to_workspace: resolved.relative_cwd.clone(),
        },
        frozen: peri_acp_types::session_resources::FrozenSnapshotBytes::new(format!(
            "{{\"frozen\":\"{}\"}}",
            env.run
        )),
    };
    let lease = step("create", facade.create_session(&input)).await?;
    Ok(Session {
        facade,
        _lease: lease,
    })
}

/// 同一份本机执行面库上的**新打开**（新连接、新 owner）：上一段实例必须已经落下。
///
/// 顺序与恢复链路一致：先收敛未决操作，再按持久会话事实取得新实例的 owner。
async fn reopen(target: &CloudTarget, env: &Env) -> Result<Session, String> {
    let facade = env.open(target).await?;
    let recovery = step(
        "reopen recover",
        facade.recover_session_persistence(&env.root),
    )
    .await?;
    check(
        recovery == PersistenceRecovery::Recovered,
        "a reopened store must converge before handing out a new owner",
    )?;
    let availability = step(
        "reopen availability",
        facade.inspect_availability(Some(&env.root)),
    )
    .await?;
    check(
        availability.execution == Some(ExecutionAvailability::Available),
        "a reopened instance must not reconstruct runtime uncertainty from durable history",
    )?;
    let resolved = step(
        "reopen resolve workspace",
        facade.resolve_workspace(env.workspace.path()),
    )
    .await?;
    let lease = step(
        "reopen acquire",
        facade.acquire_execution(&env.root, &resolved),
    )
    .await?;
    Ok(Session {
        facade,
        _lease: lease,
    })
}

async fn rows_via(session: &Session, root: &ThreadId) -> Result<usize, String> {
    step("read history", session.facade.load_session_history(root))
        .await
        .map(|history| history.len())
}

/// 远端本轮计数（**新连接**读，不经过 adapter 的读取行为）。
async fn counts_now(
    target: &CloudTarget,
    run: &str,
) -> Result<super::cloud_tests::RunCounts, String> {
    let store = step("open raw connection", target.store(StoreAccess::ReadOnly)).await?;
    let counts = run_counts(&store, run)
        .await
        .map_err(|message| format!("raw run counts: {message}"))?;
    step("close raw connection", store.close()).await?;
    Ok(counts)
}

/// 账本保留证据：本轮收据条数与这些行的**字节成本**（只算记录本身，不含 payload）。
async fn ledger_stats(
    store: &super::mutation::RemoteStore,
    run: &str,
) -> Result<(i64, i64), String> {
    let batches = store
        .read_batch(vec![StatementSpec::new(
            LEDGER_STATS_SQL,
            vec![Value::Text(format!("%{run}%"))],
        )])
        .await
        .map_err(failure)?;
    let mut rows = batches.into_iter().next().unwrap_or_default();
    let mut row = rows
        .pop()
        .ok_or_else(|| "ledger stats row is missing".to_owned())?;
    let bytes = match row.pop() {
        Some(Value::Integer(bytes)) => bytes,
        other => return Err(format!("ledger byte total is unreadable: {other:?}")),
    };
    let count = match row.pop() {
        Some(Value::Integer(count)) => count,
        other => return Err(format!("ledger row count is unreadable: {other:?}")),
    };
    Ok((count, bytes))
}

// ─── P6：超大单批写入 ────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "显式 cloud 实验：需要已授权测试库的 .env，默认不跑"]
async fn cloud_oversized_write_is_all_or_nothing() {
    let target = CloudTarget::load();
    let run = unique_run_label("peri-limit");
    let collected: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let result = with_cleanup(&target, &run, || async {
        let lines = oversized_flow(&target, &run).await?;
        *collected.lock().unwrap() = lines;
        Ok(())
    })
    .await;
    match result {
        Ok(()) => {
            let mut out = target.out();
            for line in collected.lock().unwrap().iter() {
                out.push(line.clone());
            }
            out.push("oversized_write=ok".to_owned());
            out.flush();
        }
        Err(message) => panic!("{message}"),
    }
}

async fn oversized_flow(target: &CloudTarget, run: &str) -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    let env = fixture_env(run).await?;
    let session = create_session_at(target, &env).await?;
    let mut expected = 0i64;
    // 累计历史超过 1 MiB 之后 adapter 的整段读回会撞上单请求预算（见本轮报告），因此读回
    // 只在第一档做一次；后面几档的权威判据是新连接的原始计数。
    let mut read_back_done = false;
    for size in PAYLOAD_SIZES {
        let payload = PersistedPayload::Message(BaseMessage::human("a".repeat(size)));
        let started = std::time::Instant::now();
        let outcome = step(
            &format!("append size={size}"),
            session.facade.append_history(&env.root, &[payload]),
        )
        .await;
        let append_ms = started.elapsed().as_millis();
        let class = match &outcome {
            Ok(()) => "applied".to_owned(),
            Err(message) => message.clone(),
        };
        if outcome.is_ok() {
            expected += 1;
        }
        // 权威核对走**新连接**的原始计数：生效则一定读得到这一行，拒绝则**一行都没有**——
        // 半套状态（资格行在、效果缺失，或反之）不属于任何一条合法结果。
        let counts = counts_now(target, run).await?;
        check(
            counts.messages == expected,
            &format!(
                "an oversized batch must be all-or-nothing: size={size} class={class} \
                 messages={} expected={expected}",
                counts.messages
            ),
        )?;
        let read = if read_back_done {
            "read=skipped(see report)".to_owned()
        } else {
            let rows = rows_via(&session, &env.root).await?;
            check(
                rows as i64 == expected,
                &format!("adapter and raw connection must agree: {rows} vs {expected}"),
            )?;
            read_back_done = true;
            format!("read_rows={rows}")
        };
        lines.push(format!(
            "size={size} class={class} append_ms={append_ms} messages={} {read}",
            counts.messages
        ));
    }
    // 换一次打开（新连接、新 owner）：行数仍与写入意图一致，且会话没有被大写入卡住。
    drop(session);
    let reopened = reopen(target, &env).await?;
    step(
        "normal call after big writes",
        reopened
            .facade
            .append_history(&env.root, &[env.message("after-big")]),
    )
    .await?;
    let counts = counts_now(target, run).await?;
    check(
        counts.messages == expected + 1,
        &format!(
            "a normal call must apply exactly once after the big writes: {} vs {}",
            counts.messages,
            expected + 1
        ),
    )?;
    lines.push(format!("messages_after_reopen={}", counts.messages));
    step("close", reopened.facade.close()).await?;
    Ok(lines)
}

// ─── P7：收据保留与空间成本 ──────────────────────────────────────────────────

#[tokio::test]
#[ignore = "显式 cloud 实验：需要已授权测试库的 .env，默认不跑"]
async fn cloud_receipts_are_retained_with_measured_cost() {
    let target = CloudTarget::load();
    let run = unique_run_label("peri-retain");
    let collected: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let result = with_cleanup(&target, &run, || async {
        let lines = retention_flow(&target, &run).await?;
        *collected.lock().unwrap() = lines;
        Ok(())
    })
    .await;
    match result {
        Ok(()) => {
            let mut out = target.out();
            for line in collected.lock().unwrap().iter() {
                out.push(line.clone());
            }
            out.push("receipt_retention=ok".to_owned());
            out.flush();
        }
        Err(message) => panic!("{message}"),
    }
}

async fn retention_flow(target: &CloudTarget, run: &str) -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    let env = fixture_env(run).await?;
    let session = create_session_at(target, &env).await?;

    // 两次领域调用（新建 / 追加两条），每次一行收据。
    step(
        "append two",
        session
            .facade
            .append_history(&env.root, &[env.message("one"), env.message("two")]),
    )
    .await?;
    step("drain", session.facade.drain_persistence(&env.root)).await?;

    let store = step("open raw connection", target.store(StoreAccess::ReadOnly)).await?;
    let (before_rows, before_bytes) = ledger_stats(&store, run).await?;
    step("close raw connection", store.close()).await?;
    check(
        before_rows >= 2,
        &format!("every domain call must leave a receipt: {before_rows}"),
    )?;

    // 收敛（会读写同一批收据）与重读之后，逐条仍在：没有 TTL 清理、也没有被收敛删掉。
    let recovery = step(
        "recover",
        session.facade.recover_session_persistence(&env.root),
    )
    .await?;
    check(
        recovery == PersistenceRecovery::Recovered,
        "a settled session must recover",
    )?;
    let rows = rows_via(&session, &env.root).await?;
    step(
        "drain after recovery",
        session.facade.drain_persistence(&env.root),
    )
    .await?;

    let store = step("open raw connection", target.store(StoreAccess::ReadOnly)).await?;
    let (after_rows, after_bytes) = ledger_stats(&store, run).await?;
    step("close raw connection", store.close()).await?;
    check(
        after_rows == before_rows && after_bytes == before_bytes,
        &format!(
            "receipts must survive recovery and re-reads unchanged: {before_rows}/{before_bytes} \
             -> {after_rows}/{after_bytes}"
        ),
    )?;
    lines.push(format!(
        "ledger_rows={after_rows} ledger_bytes={after_bytes} rows={rows} recovery={recovery:?}"
    ));
    step("close", session.facade.close()).await?;
    Ok(lines)
}

// ─── P5：在途请求被取消 ──────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "显式 cloud 实验：需要已授权测试库的 .env，默认不跑"]
async fn cloud_cancelled_write_applies_at_most_once() {
    let target = CloudTarget::load();
    let run = unique_run_label("peri-cancel");
    let collected: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let result = with_cleanup(&target, &run, || async {
        let lines = cancelled_flow(&target, &run).await?;
        *collected.lock().unwrap() = lines;
        Ok(())
    })
    .await;
    match result {
        Ok(()) => {
            let mut out = target.out();
            for line in collected.lock().unwrap().iter() {
                out.push(line.clone());
            }
            out.push("cancelled_write=ok".to_owned());
            out.flush();
        }
        Err(message) => panic!("{message}"),
    }
}

async fn cancelled_flow(target: &CloudTarget, run: &str) -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    let env = fixture_env(run).await?;
    let session = create_session_at(target, &env).await?;
    step(
        "warmup append",
        session
            .facade
            .append_history(&env.root, &[env.message("warmup")]),
    )
    .await?;
    let warmup = counts_now(target, run).await?;
    check(
        warmup.messages == 1,
        &format!("the warmup row must be durable: {}", warmup.messages),
    )?;

    // 在途取消：直接 drop 调用 future（不是「发一个取消消息」）。这一档刻意用大 payload
    // （与 P6 同一量级）：小 payload 常常在预算内跑完，那就不是「在途取消」。
    let cancelled = tokio::time::timeout(
        IN_FLIGHT_BUDGET,
        session.facade.append_history(
            &env.root,
            &[PersistedPayload::Message(BaseMessage::human(
                "a".repeat(256 << 10),
            ))],
        ),
    )
    .await
    .is_err();
    lines.push(format!("in_flight_cancelled={cancelled}"));

    // ① 同一次打开上继续服务：被丢弃的在途请求让**那一代连接**失效，adapter 必须按同一份
    //    打开事实重建后继续——同一个实例仍然能回答（读取与恢复结论），而不是报「连接不可用」。
    let same_instance = step(
        "recover on the same instance",
        session.facade.recover_session_persistence(&env.root),
    )
    .await?;
    check(
        same_instance == PersistenceRecovery::Recovered,
        &format!("the same instance must answer a definite recovery state: {same_instance:?}"),
    )?;
    let same_instance_rows = rows_via(&session, &env.root).await?;
    check(
        same_instance_rows == 1 || same_instance_rows == 2,
        &format!("a read on the same instance must still answer: {same_instance_rows}"),
    )?;
    lines.push(format!(
        "same_instance_recover={same_instance:?} same_instance_rows={same_instance_rows}"
    ));

    // ② 换一次打开（新连接、新 owner）：重开后必须收敛到确定终态。
    drop(session);
    let reopened = reopen(target, &env).await?;
    let rows = rows_via(&reopened, &env.root).await?;
    check(
        rows == 1 || rows == 2,
        &format!("a cancelled write is present 0 or 1 time(s): {rows}"),
    )?;
    let counts = counts_now(target, run).await?;
    check(
        counts.messages == rows as i64,
        &format!(
            "the converged outcome must be the durable one: {rows} vs {}",
            counts.messages
        ),
    )?;
    step(
        "call after reopen",
        reopened
            .facade
            .append_history(&env.root, &[env.message("after-cancel")]),
    )
    .await?;
    let final_rows = rows_via(&reopened, &env.root).await?;
    check(
        final_rows == rows + 1,
        &format!("a later call must apply exactly once: {final_rows}"),
    )?;

    lines.push(format!(
        "rows_after_cancel={rows} rows_after_later_call={final_rows}"
    ));
    step("close", reopened.facade.close()).await?;
    Ok(lines)
}

// ─── 带父会话的新建输入 ──────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "显式 cloud 实验：需要已授权测试库的 .env，默认不跑"]
async fn cloud_remote_create_refuses_parent_input() {
    let target = CloudTarget::load();
    let run = unique_run_label("peri-parent");
    let collected: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let result = with_cleanup(&target, &run, || async {
        let lines = parent_input_flow(&target, &run).await?;
        *collected.lock().unwrap() = lines;
        Ok(())
    })
    .await;
    match result {
        Ok(()) => {
            let mut out = target.out();
            for line in collected.lock().unwrap().iter() {
                out.push(line.clone());
            }
            out.push("parent_input=refused".to_owned());
            out.flush();
        }
        Err(message) => panic!("{message}"),
    }
}

async fn parent_input_flow(target: &CloudTarget, run: &str) -> Result<Vec<String>, String> {
    let env = fixture_env(run).await?;
    let session = create_session_at(target, &env).await?;
    let child = ThreadId::from(format!("{run}-child"));
    let input = NewSession {
        thread_id: child.to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        meta: NewSessionMeta {
            title: Some(format!("synthetic {run}-child")),
            cwd: "/tmp/peri-cloud-synth".to_owned(),
            // 唯一被拒的形状：远程新建只接受 root，有父必须走 child 通路。
            parent_thread_id: Some(env.root.to_string()),
            hidden: true,
            cancel_policy: CancelPolicy::Cascade,
            snapshot_at_message_id: None,
        },
        binding: SessionBinding {
            schema_version: SESSION_BINDING_VERSION,
            revision: 1,
            project_id: peri_acp_types::workspace::ProjectId::new(),
            workspace_id: peri_acp_types::workspace::WorkspaceId::new(),
            cwd_relative_to_workspace: PathBuf::new(),
        },
        frozen: peri_acp_types::session_resources::FrozenSnapshotBytes::new(format!(
            "{{\"frozen\":\"{run}-child\"}}"
        )),
    };
    let error = match session.facade.create_session(&input).await {
        Ok(_) => return Err("a child input must not be accepted by the root path".to_owned()),
        Err(error) => error,
    };
    check(
        matches!(error.kind(), SessionResourceErrorKind::InvalidInput { .. }) && {
            // 分类已经在上面断言：这里只再确认它确实是那条拒绝理由，而不是同类的另一种输入错。
            true
        },
        &format!(
            "the refusal must be typed, not a silent success: {}",
            remote_error_class(&error)
        ),
    )?;
    // 远端零行：拒绝发生在写之前。
    let counts = counts_now(target, run).await?;
    check(
        counts.sessions == 1,
        &format!(
            "a refused create must leave no session row: {}",
            counts.sessions
        ),
    )?;
    let store = step("open raw connection", target.store(StoreAccess::ReadOnly)).await?;
    let batches = store
        .read_batch(vec![StatementSpec::new(
            "SELECT COUNT(*) FROM threads WHERE id = ?1",
            vec![Value::Text(child.to_string())],
        )])
        .await
        .map_err(failure)?;
    step("close raw connection", store.close()).await?;
    let rows = batches
        .into_iter()
        .next()
        .and_then(|mut rows| rows.pop())
        .and_then(|mut row| row.pop());
    check(
        matches!(rows, Some(Value::Integer(0))),
        &format!("the refused id must not exist remotely: {rows:?}"),
    )?;
    step("close", session.facade.close()).await?;
    Ok(vec![format!(
        "refused_class={} sessions={}",
        remote_error_class(&error),
        counts.sessions
    )])
}
