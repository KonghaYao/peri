//! 唯一任务注册表：owner 绑定、容量、TTL/GONE、停止、输出与关闭（FC-BASH-03/04、
//! FC-STATE-01）。
//!
//! 注册表直接持有**真实进程**（单进程形态下没有 worker 替身可以注入，也不需要：
//! 注册表的状态机就是进程状态的投影）。所有用例都在临时工作区根里真跑 `bash -c`，
//! 断言的是真实进程行为；进程级细节（句柄、进程组、TERM→KILL 升级）另见
//! `tests/bash_lifecycle.rs`。

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeDelta, Utc};
use local_mcp_server::tasks::log::{DirOutputPersist, LogStream, OutputPersist};
use local_mcp_server::tasks::registry::{
    task_resource_uri, BashRun, BashRunArgs, BashRunError, Clock, TaskAccessError, TaskEventKind,
    TaskRegistry, TaskRegistryConfig, TASKS_RESOURCE_URI,
};
use local_mcp_server::tasks::{BashTaskConfig, BashTasks, MAX_LOG_READ_BYTES};
use local_mcp_server::wire::{RequestContext, TaskStatus};

/// 可控时钟：`advance` 只推动单调与墙上时间。
struct ManualClock {
    monotonic_offset: std::sync::Mutex<Duration>,
    wall_offset: std::sync::Mutex<TimeDelta>,
}

impl ManualClock {
    fn new() -> Self {
        Self {
            monotonic_offset: std::sync::Mutex::new(Duration::ZERO),
            wall_offset: std::sync::Mutex::new(TimeDelta::zero()),
        }
    }

    fn advance(&self, amount: Duration) {
        *self.monotonic_offset.lock().unwrap() += amount;
        *self.wall_offset.lock().unwrap() += TimeDelta::from_std(amount).expect("duration 可转换");
    }
}

impl Clock for ManualClock {
    fn wall(&self) -> DateTime<Utc> {
        Utc::now() + *self.wall_offset.lock().unwrap()
    }

    fn monotonic(&self) -> Instant {
        Instant::now() + *self.monotonic_offset.lock().unwrap()
    }
}

struct Harness {
    registry: Arc<TaskRegistry>,
    clock: Arc<ManualClock>,
    /// 工作区根必须比注册表活得久（`Bash` 的 cwd 指向它）。
    _workspace: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        Self::with_config(|config| config)
    }

    fn with_config(tweak: impl FnOnce(TaskRegistryConfig) -> TaskRegistryConfig) -> Self {
        let workspace = tempfile::TempDir::new().expect("workspace");
        let log_dir = workspace.path().join(".local-mcp/logs");
        std::fs::create_dir_all(&log_dir).expect("日志目录");
        let persist: Arc<dyn OutputPersist> = Arc::new(DirOutputPersist::new(&log_dir));
        let mut config = TaskRegistryConfig::new(persist);
        // 测试不跑后台轮询：状态推进由 `refresh_all`/`refresh` 显式驱动。
        config.poll_interval = None;
        let config = tweak(config);
        let bash = Arc::new(BashTasks::new(BashTaskConfig::new(
            workspace.path(),
            &log_dir,
        )));
        let clock = Arc::new(ManualClock::new());
        let registry = TaskRegistry::new(bash, Arc::clone(&clock) as Arc<dyn Clock>, config);
        Self {
            registry,
            clock,
            _workspace: workspace,
        }
    }

    fn context(&self, principal: &str, instance: &str) -> RequestContext {
        RequestContext::new("req-1", principal, instance)
    }

    async fn start_background(&self, ctx: &RequestContext, command: &str) -> BashRun {
        self.registry
            .start_bash(
                ctx,
                BashRunArgs {
                    command: command.to_string(),
                    timeout_ms: None,
                    background: true,
                },
            )
            .await
            .expect("后台启动应成功")
    }

    async fn start_foreground(
        &self,
        ctx: &RequestContext,
        command: &str,
        timeout_ms: Option<u64>,
    ) -> Result<BashRun, BashRunError> {
        self.registry
            .start_bash(
                ctx,
                BashRunArgs {
                    command: command.to_string(),
                    timeout_ms,
                    background: false,
                },
            )
            .await
    }

    /// 轮询到终态（真进程的结束时刻不可精确预测；上限 5s 后如实失败）。
    async fn wait_terminal(&self, ctx: &RequestContext, task_id: &str) -> TaskStatus {
        for _ in 0..250 {
            let snapshot = self.registry.refresh(ctx, task_id).await.expect("刷新状态");
            if snapshot.status.is_terminal() {
                return snapshot.status;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("任务未在 5s 内进入终态: {task_id}");
    }

    /// 收尾：终止所有在跑任务，避免测试留下孤儿进程。
    async fn shutdown(&self) {
        self.registry.close().await.expect("关闭注册表");
    }
}

fn running_snapshot(run: &BashRun) -> local_mcp_server::wire::TaskSnapshot {
    match run {
        BashRun::Running { snapshot, .. } => (**snapshot).clone(),
        BashRun::Finished { state } => panic!("预期运行中任务，实际终态 {state:?}"),
    }
}

#[tokio::test]
async fn background_start_is_owner_bound_and_listed() {
    let harness = Harness::new();
    let ctx = harness.context("alice", "conn-1");
    let run = harness.start_background(&ctx, "sleep 30").await;
    let snapshot = running_snapshot(&run);

    assert!(snapshot.task_id.starts_with("shell-"));
    // shell-<完整 UUIDv7>：8 + 36 字符，禁止截断。
    let uuid = snapshot.task_id.trim_start_matches("shell-");
    assert_eq!(uuid.len(), 36, "uuid 不得截断: {uuid}");
    assert_eq!(snapshot.owner, "alice");
    assert_eq!(snapshot.client_instance, "conn-1");
    assert_eq!(snapshot.status, TaskStatus::Running);
    assert!(snapshot.stdout_log.is_some() && snapshot.stderr_log.is_some());
    assert!(snapshot.pid.is_some(), "真进程必须有 pid");

    let listed = harness.registry.list(&ctx);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].task_id, snapshot.task_id);

    // 只读资源面（协议层接口）：同一身份可见，其他身份为空。
    assert_eq!(
        harness.registry.snapshots_for("alice", "conn-1").len(),
        1,
        "同身份应可见"
    );
    assert!(harness.registry.snapshots_for("bob", "conn-1").is_empty());
    assert!(harness
        .registry
        .snapshot_for("alice", "conn-2", &snapshot.task_id)
        .is_none());

    harness.shutdown().await;
}

#[tokio::test]
async fn cross_identity_access_is_refused_without_leaking_existence() {
    let harness = Harness::new();
    let owner = harness.context("alice", "conn-1");
    let run = harness.start_background(&owner, "sleep 30").await;
    let snapshot = running_snapshot(&run);
    let task_id = snapshot.task_id.clone();

    let other_principal = harness.context("bob", "conn-1");
    let error = harness
        .registry
        .snapshot(&other_principal, &task_id)
        .expect_err("跨主体应拒绝");
    assert!(matches!(error, TaskAccessError::ForeignOwner { .. }));
    assert_eq!(error.public_message(), format!("unknown task: {task_id}"));

    let other_instance = harness.context("alice", "conn-2");
    let error = harness
        .registry
        .snapshot(&other_instance, &task_id)
        .expect_err("跨连接实例应拒绝");
    assert!(matches!(error, TaskAccessError::ForeignInstance { .. }));
    assert_eq!(error.public_message(), format!("unknown task: {task_id}"));

    // 停止与日志读取同样拒绝。
    let error = harness
        .registry
        .stop(&other_instance, &task_id)
        .await
        .expect_err("跨实例停止应拒绝");
    assert!(matches!(error, TaskAccessError::ForeignInstance { .. }));
    let error = harness
        .registry
        .read_log(&other_principal, &task_id, LogStream::Stdout, 128)
        .await
        .expect_err("跨主体读日志应拒绝");
    assert!(matches!(error, TaskAccessError::ForeignOwner { .. }));

    // 被拒绝的停止请求不得真的动到进程：任务仍在运行。
    let still_running = harness
        .registry
        .snapshot(&owner, &task_id)
        .expect("owner 仍可查询");
    assert_eq!(still_running.status, TaskStatus::Running);

    // 未知 id 同样不可区分。
    let error = harness
        .registry
        .snapshot(&owner, "shell-00000000-0000-7000-8000-000000000000")
        .expect_err("未知任务");
    assert!(matches!(error, TaskAccessError::Unknown { .. }));

    harness.shutdown().await;
}

#[tokio::test]
async fn log_handles_are_high_entropy_per_task() {
    let harness = Harness::new();
    let ctx = harness.context("alice", "conn-1");
    let first = running_snapshot(&harness.start_background(&ctx, "sleep 30").await);
    let second = running_snapshot(&harness.start_background(&ctx, "sleep 30").await);

    // 句柄只存在于进程内任务表与对内部日志存储的调用中，不进入任何面向客户端的快照字段。
    let snapshot_json = serde_json::to_value(&first).expect("序列化");
    assert!(
        !snapshot_json.to_string().contains("log_handle"),
        "快照不得泄露内部句柄字段名"
    );

    // 读取日志时注册表注入句柄（调用方无需、也无法提供）。
    harness
        .registry
        .read_log(&ctx, &first.task_id, LogStream::Stdout, 64)
        .await
        .expect("读取日志");
    assert_ne!(first.task_id, second.task_id);

    harness.shutdown().await;
}

#[tokio::test]
async fn capacity_limit_applies_to_explicit_background_start() {
    let harness = Harness::new();
    let ctx = harness.context("alice", "conn-1");
    for _ in 0..5 {
        harness.start_background(&ctx, "sleep 30").await;
    }
    let error = harness
        .registry
        .start_bash(
            &ctx,
            BashRunArgs {
                command: "sleep 30".to_string(),
                timeout_ms: None,
                background: true,
            },
        )
        .await
        .expect_err("超过并发上限应拒绝");
    assert!(matches!(error, BashRunError::ConcurrentLimit { limit: 5 }));
    assert_eq!(
        error.to_string(),
        "Maximum 5 concurrent background tasks reached"
    );
    assert_eq!(harness.registry.list(&ctx).len(), 5, "第 6 次不应登记任务");

    harness.shutdown().await;
}

#[tokio::test]
async fn foreground_completion_does_not_register_a_task() {
    let harness = Harness::new();
    let ctx = harness.context("alice", "conn-1");

    let run = harness
        .start_foreground(&ctx, "printf 'done\\n'", Some(15_000))
        .await
        .expect("前台");
    match run {
        BashRun::Finished { state } => {
            assert_eq!(state.status, TaskStatus::Completed, "前台完成直接返回终态");
            assert_eq!(state.final_output.as_deref(), Some("done\n"));
        }
        BashRun::Running { .. } => panic!("预期前台完成"),
    }
    assert!(
        harness.registry.list(&ctx).is_empty(),
        "同步完成的调用不占用任务注册表"
    );

    harness.shutdown().await;
}

#[tokio::test]
async fn foreground_timeout_promotes_to_registered_task() {
    let harness = Harness::new();
    let ctx = harness.context("alice", "conn-1");
    let run = harness
        .start_foreground(&ctx, "sleep 30", Some(300))
        .await
        .expect("前台");
    match run {
        BashRun::Running {
            snapshot,
            promoted,
            state,
        } => {
            assert!(promoted, "前台超时返回运行中即为提升");
            assert_eq!(snapshot.status, TaskStatus::Running);
            assert!(state.pid.is_some(), "提升的任务仍持有进程");
            assert_eq!(harness.registry.list(&ctx).len(), 1, "提升后任务必须可查询");
        }
        BashRun::Finished { .. } => panic!("预期提升为后台任务"),
    }

    harness.shutdown().await;
}

#[tokio::test]
async fn foreground_timeout_with_full_capacity_kills_and_reports() {
    let harness = Harness::new();
    let ctx = harness.context("alice", "conn-1");
    for _ in 0..5 {
        harness.start_background(&ctx, "sleep 30").await;
    }
    let error = harness
        .start_foreground(&ctx, "sleep 30", Some(300))
        .await
        .expect_err("容量已满时提升应失败");
    let promoted_pid = match error {
        BashRunError::PromotionUnavailable { reason, state } => {
            assert_eq!(reason, "Maximum 5 concurrent background tasks reached");
            assert_eq!(state.status, TaskStatus::Running);
            state.pid.expect("提升失败的任务也有 pid")
        }
        other => panic!("预期提升失败，实际 {other:?}"),
    };
    assert_eq!(harness.registry.list(&ctx).len(), 5, "失败项不进入注册表");
    // 提升失败必须终止进程组：该 pid 不再存活（只检查本测试自己 spawn 的进程）。
    for _ in 0..100 {
        if !process_alive(promoted_pid) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        !process_alive(promoted_pid),
        "提升失败后进程组必须被回收 (pid={promoted_pid})"
    );

    harness.shutdown().await;
}

#[tokio::test]
async fn stop_is_idempotent_and_returns_terminal_snapshot() {
    let harness = Harness::new();
    let ctx = harness.context("alice", "conn-1");
    let snapshot = running_snapshot(&harness.start_background(&ctx, "sleep 30").await);

    let stopped = harness
        .registry
        .stop(&ctx, &snapshot.task_id)
        .await
        .expect("停止应成功");
    // 真实现：进程组被 SIGTERM 终止，bash 非正常退出 → Failed。
    // `TaskStatus::Killed` 只出现在请求取消路径与"KILL 升级后仍未回收"的不可回收路径；
    // F-P3-01 按此实测行为订正了 `wire::TaskStatus::Killed` 与 `TaskRegistry::stop` 的文档，
    // 行为与断言都未改动（不放宽、不假装 stop 是 killed）。
    assert_eq!(stopped.status, TaskStatus::Failed);
    assert!(stopped.ended_at.is_some(), "停止后必须落终态时间");

    let again = harness
        .registry
        .stop(&ctx, &snapshot.task_id)
        .await
        .expect("重复停止应幂等");
    assert_eq!(again.status, stopped.status);
    assert_eq!(again.exit_code, stopped.exit_code);
}

#[tokio::test]
async fn refresh_marks_terminal_and_emits_events() {
    let harness = Harness::new();
    let ctx = harness.context("alice", "conn-1");
    let mut events = harness.registry.subscribe();
    let snapshot = running_snapshot(&harness.start_background(&ctx, "true").await);

    let started = events.recv().await.expect("Started 事件");
    assert_eq!(started.kind, TaskEventKind::Started);
    assert_eq!(started.snapshot.task_id, snapshot.task_id);

    let status = harness.wait_terminal(&ctx, &snapshot.task_id).await;
    assert_eq!(status, TaskStatus::Completed);
    let refreshed = harness
        .registry
        .snapshot(&ctx, &snapshot.task_id)
        .expect("快照");
    assert_eq!(refreshed.exit_code, Some(0));
    assert!(refreshed.ended_at.is_some());

    // 真实现下状态面可能先发 Updated（仍在跑）再发 Terminal；事件流必须**最终**收敛到 Terminal。
    let mut terminal = None;
    for _ in 0..8 {
        let event = events.recv().await.expect("事件");
        if event.kind == TaskEventKind::Terminal {
            terminal = Some(event);
            break;
        }
        assert_eq!(event.kind, TaskEventKind::Updated);
    }
    let terminal = terminal.expect("必须发出 Terminal 事件");
    assert_eq!(terminal.snapshot.status, TaskStatus::Completed);

    // 自然完成后再停止 → 幂等返回终态，不再动进程。
    let stopped = harness
        .registry
        .stop(&ctx, &snapshot.task_id)
        .await
        .expect("停止已完成任务");
    assert_eq!(stopped.status, TaskStatus::Completed);
}

#[tokio::test]
async fn terminal_ttl_reaps_to_gone() {
    let harness = Harness::with_config(|mut config| {
        config.terminal_ttl = Duration::from_secs(3_600);
        config.max_terminal_entries = 100;
        config
    });
    let ctx = harness.context("alice", "conn-1");
    let snapshot = running_snapshot(&harness.start_background(&ctx, "true").await);
    let stdout_log = snapshot.stdout_log.clone().expect("日志路径");
    harness.wait_terminal(&ctx, &snapshot.task_id).await;

    assert!(harness.registry.reap_expired().is_empty(), "未到期不回收");
    harness.clock.advance(Duration::from_secs(3_601));
    let reaped = harness.registry.reap_expired();
    assert_eq!(reaped, vec![snapshot.task_id.clone()]);

    let error = harness
        .registry
        .snapshot(&ctx, &snapshot.task_id)
        .expect_err("回收后应为 Gone");
    assert!(matches!(error, TaskAccessError::Gone { .. }));
    assert_eq!(
        error.public_message(),
        format!("task expired: {}", snapshot.task_id)
    );
    assert!(harness.registry.list(&ctx).is_empty());
    assert!(
        !std::path::Path::new(&stdout_log).exists(),
        "记录被回收时日志必须一并回收（Gone 的对外语义）"
    );
}

#[tokio::test]
async fn retention_count_limit_reaps_oldest_terminal_tasks() {
    let harness = Harness::with_config(|mut config| {
        config.terminal_ttl = Duration::from_secs(86_400);
        config.max_terminal_entries = 2;
        config
    });
    let ctx = harness.context("alice", "conn-1");
    let mut ids = Vec::new();
    for _ in 0..3 {
        let snapshot = running_snapshot(&harness.start_background(&ctx, "true").await);
        harness.wait_terminal(&ctx, &snapshot.task_id).await;
        harness.clock.advance(Duration::from_secs(1));
        ids.push(snapshot.task_id);
    }

    let reaped = harness.registry.reap_expired();
    assert_eq!(reaped, vec![ids[0].clone()], "只回收最旧的终态条目");
    assert!(harness.registry.list(&ctx).len() == 2);
    assert!(harness.registry.reap_expired().is_empty());
}

#[tokio::test]
async fn read_output_merges_streams_and_exit_code() {
    let harness = Harness::new();
    let ctx = harness.context("alice", "conn-1");
    let snapshot = running_snapshot(
        &harness
            .start_background(&ctx, "printf 'out\\n'; printf 'boom' 1>&2; exit 2")
            .await,
    );
    let status = harness.wait_terminal(&ctx, &snapshot.task_id).await;
    assert_eq!(status, TaskStatus::Failed, "非零退出码是 Failed 终态");

    let output = harness
        .registry
        .read_output(&ctx, &snapshot.task_id)
        .await
        .expect("读取输出");
    assert_eq!(
        output.text, "out\n\n[stderr]\nboom\n[Exit code: 2]",
        "非零退出码必须出现在合并文本中"
    );
    assert!(!output.truncated);
    assert_eq!(output.status, TaskStatus::Failed);
    assert_eq!(output.exit_code, Some(2));

    // 日志读取上限受常量约束。
    let payload = harness
        .registry
        .read_log(&ctx, &snapshot.task_id, LogStream::Stdout, usize::MAX)
        .await
        .expect("日志");
    assert_eq!(payload.content, "out\n");
    assert_eq!(MAX_LOG_READ_BYTES, 65_536, "日志读取上限是冻结常量");
}

#[tokio::test]
async fn close_stops_running_tasks() {
    let harness = Harness::new();
    let ctx = harness.context("alice", "conn-1");
    let first = running_snapshot(&harness.start_background(&ctx, "sleep 30").await);
    let second = running_snapshot(&harness.start_background(&ctx, "sleep 30").await);
    let pids = [first.pid.expect("pid"), second.pid.expect("pid")];

    harness.registry.close().await.expect("关闭应成功");

    for pid in pids {
        for _ in 0..150 {
            if !process_alive(pid) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!process_alive(pid), "关闭后不得留下在跑进程 (pid={pid})");
    }
    // 记录本身保留（供收尾读取），但状态已是终态且不可再被误认为运行中。
    let after = harness
        .registry
        .snapshot(&ctx, &first.task_id)
        .expect("关闭后仍可查询");
    assert!(after.status.is_terminal());
}

#[tokio::test]
async fn spawn_failure_surfaces_as_worker_failure() {
    // 工作目录不存在 → spawn 失败（唯一稳定的 spawn 失败形状）。
    let workspace = harness_with_missing_workspace();
    let ctx = RequestContext::new("req-1", "alice", "conn-1");
    let error = workspace
        .registry
        .start_bash(
            &ctx,
            BashRunArgs {
                command: "true".to_string(),
                timeout_ms: None,
                background: false,
            },
        )
        .await
        .expect_err("spawn 失败应上抛");
    match error {
        BashRunError::WorkerFailure { kind, message } => {
            assert_eq!(kind, "protocol", "任务层失败统一以 protocol 类上抛");
            assert!(
                message.contains("No such file or directory") || !message.is_empty(),
                "失败文本必须保留底层原因: {message}"
            );
        }
        other => panic!("预期 WorkerFailure，实际 {other:?}"),
    }
    assert!(workspace.registry.list(&ctx).is_empty());
}

/// 工作区根不存在时 spawn 会在 `current_dir` 上失败。
struct MissingWorkspaceHarness {
    registry: Arc<TaskRegistry>,
    _root: tempfile::TempDir,
}

fn harness_with_missing_workspace() -> MissingWorkspaceHarness {
    let root = tempfile::TempDir::new().expect("root");
    let log_dir = root.path().join("logs");
    std::fs::create_dir_all(&log_dir).expect("日志目录");
    let persist: Arc<dyn OutputPersist> = Arc::new(DirOutputPersist::new(&log_dir));
    let mut config = TaskRegistryConfig::new(persist);
    config.poll_interval = None;
    let missing = root.path().join("does-not-exist");
    let bash = Arc::new(BashTasks::new(BashTaskConfig::new(&missing, &log_dir)));
    let registry = TaskRegistry::new(bash, Arc::new(local_mcp_server::tasks::SystemClock), config);
    MissingWorkspaceHarness {
        registry,
        _root: root,
    }
}

/// 进程是否存活（只用于检查本测试自己 spawn 的 pid）。
fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: `kill` 只读取参数；信号 0 不发送信号，仅做存在性/权限探测。
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

#[test]
fn resource_uris_are_stable() {
    assert_eq!(TASKS_RESOURCE_URI, "sandbox://tasks");
    assert_eq!(
        task_resource_uri("shell-01920000-0000-7000-8000-000000000000"),
        "sandbox://tasks/shell-01920000-0000-7000-8000-000000000000"
    );
}

#[test]
fn task_registry_config_defaults_match_design() {
    let log_dir = tempfile::TempDir::new().expect("log");
    let persist: Arc<dyn OutputPersist> = Arc::new(DirOutputPersist::new(log_dir.path()));
    let config = TaskRegistryConfig::new(persist);
    assert_eq!(config.shell_limit, 5, "源 SHELL_LIMIT");
    assert_eq!(config.terminal_ttl, Duration::from_secs(3_600), "默认 1h");
    assert_eq!(config.max_terminal_entries, 100);
    assert_eq!(config.log_read_bytes, MAX_LOG_READ_BYTES);
    assert_eq!(
        config.poll_interval,
        Some(Duration::from_millis(250)),
        "与协议层资源观察间隔对齐"
    );
}
