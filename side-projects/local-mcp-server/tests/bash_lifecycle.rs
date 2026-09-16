//! `Bash` 进程生命周期：前台、超时提升、显式后台、日志、取消、进程组回收与清理
//! （FC-BASH-03/04）。
//!
//! 只使用**无害**合成命令（`printf`/`sleep`/`read`/`head`/`tr`），不触碰宿主文件
//! 之外的位置，不启动网络或系统服务；每个用例结束都会终止/回收进程组，避免孤儿。
//!
//! 本文件验证的是 [`BashTasks`] 这条**真实进程执行路径**（单进程本机形态下的唯一
//! 持有 `Child` 的地方）；owner 绑定、容量、TTL 与事件在 `tests/tasks_registry.rs`。

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use local_mcp_server::tasks::bash::BashTask;
use local_mcp_server::tasks::log::LogStream;
use local_mcp_server::tasks::params::{BashMode, BashStartPayload, BashTaskState};
use local_mcp_server::tasks::{BashTaskConfig, BashTaskError, BashTasks};
use local_mcp_server::wire::TaskStatus;
use tempfile::TempDir;

struct Harness {
    _workspace: TempDir,
    log_dir: TempDir,
    tasks: Arc<BashTasks>,
}

impl Harness {
    fn new() -> Self {
        let workspace = TempDir::new().expect("workspace tempdir");
        let log_dir = TempDir::new().expect("log tempdir");
        let config = BashTaskConfig::new(workspace.path(), log_dir.path());
        Self {
            _workspace: workspace,
            log_dir,
            tasks: Arc::new(BashTasks::new(config)),
        }
    }

    fn log_dir(&self) -> PathBuf {
        self.log_dir.path().to_path_buf()
    }

    async fn start(
        &self,
        task_id: &str,
        command: &str,
        mode: BashMode,
        timeout_ms: Option<u64>,
        await_ms: Option<u64>,
    ) -> Arc<BashTask> {
        let payload = BashStartPayload {
            task_id: task_id.to_string(),
            log_handle: handle_for(task_id),
            command: command.to_string(),
            cwd: self._workspace.path().to_string_lossy().to_string(),
            mode,
            timeout_ms,
            await_ms,
        };
        self.tasks.start(payload, None).await.expect("启动应成功")
    }

    /// 有界等待任务终态（`wait_ms = None` 表示等到终态为止）。
    async fn await_task(&self, task: &Arc<BashTask>, wait_ms: Option<u64>) -> BashTaskState {
        let deadline = wait_ms.map(|ms| tokio::time::Instant::now() + Duration::from_millis(ms));
        task.wait_terminal(deadline).await;
        task.snapshot()
    }

    /// 轮询 stdout 日志直到出现 `needle`（等待子进程完成初始化）。
    async fn wait_for_log(&self, task: &Arc<BashTask>, needle: &str, limit: Duration) {
        let deadline = Instant::now() + limit;
        loop {
            if let Ok(log) =
                self.tasks
                    .read_log(task, &handle_for(task.task_id()), LogStream::Stdout, 4_096)
            {
                if log.content.contains(needle) {
                    return;
                }
            }
            assert!(Instant::now() < deadline, "等待日志 {needle:?} 超时");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// 停止任务（TERM → 2s → KILL），幂等。
    async fn cancel(&self, task: &Arc<BashTask>) -> BashTaskState {
        task.terminate_group(false).await;
        task.snapshot()
    }
}

/// 每个 task id 对应确定的高熵句柄（测试内可控，生产由注册表铸造）。
fn handle_for(task_id: &str) -> String {
    let digest = task_id.bytes().fold(0u64, |acc, byte| {
        acc.wrapping_mul(31).wrapping_add(byte as u64)
    });
    format!("{digest:016x}{digest:016x}{digest:016x}{digest:016x}")
}

/// 进程组是否仍有存活进程。
fn group_alive(pgid: u32) -> bool {
    // SAFETY: `kill` 只读取参数；信号 0 仅做存在性检查。
    unsafe { libc::kill(-(pgid as i32), 0) == 0 }
}

#[tokio::test]
async fn foreground_command_merges_streams_and_exit_code() {
    let harness = Harness::new();
    let task = harness
        .start(
            "shell-fg-1",
            "printf 'hello\\n'; printf 'oops\\n' 1>&2",
            BashMode::Foreground,
            None,
            None,
        )
        .await;
    let state = task.snapshot();
    assert_eq!(state.status, TaskStatus::Completed);
    assert_eq!(state.exit_code, Some(0));
    let output = state.final_output.expect("终态应有输出");
    assert!(output.contains("hello"), "{output}");
    assert!(output.contains("[stderr]\noops"), "{output}");
    assert!(!state.truncated);
    assert!(state.pid.is_some() && state.pgid == state.pid);
}

#[tokio::test]
async fn nonzero_exit_is_reported_as_failed() {
    let harness = Harness::new();
    let task = harness
        .start("shell-fg-2", "exit 3", BashMode::Foreground, None, None)
        .await;
    let state = task.snapshot();
    assert_eq!(state.status, TaskStatus::Failed);
    assert_eq!(state.exit_code, Some(3));
    assert!(state.final_output.expect("输出").contains("[Exit code: 3]"));
}

#[tokio::test]
async fn stdin_is_null_so_readers_hit_eof_fast() {
    let harness = Harness::new();
    let started = Instant::now();
    let task = harness
        .start(
            "shell-fg-3",
            "read x; echo \"got:${x:-<eof>}\"",
            BashMode::Foreground,
            Some(5_000),
            Some(5_000),
        )
        .await;
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "读 stdin 应立即 EOF，实际 {:?}",
        started.elapsed()
    );
    let output = task.snapshot().final_output.expect("输出");
    assert!(output.contains("got:<eof>"), "stdin 应为 null: {output}");
}

#[tokio::test]
async fn foreground_timeout_promotes_and_keeps_process_alive() {
    let harness = Harness::new();
    let started = Instant::now();
    let task = harness
        .start(
            "shell-promote-1",
            "printf 'start\\n'; sleep 1; printf 'end\\n'",
            BashMode::Foreground,
            Some(200),
            Some(200),
        )
        .await;
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "前台超时应快速返回"
    );
    let state = task.snapshot();
    assert_eq!(state.status, TaskStatus::Running, "超时后进程继续存活");
    assert!(state.promoted, "应标记为提升");
    assert!(state.stdout.contains("start"), "部分输出应已捕获");

    // 提升后没有新期限：等待自然完成。
    let finished = harness.await_task(&task, Some(5_000)).await;
    assert_eq!(finished.status, TaskStatus::Completed);
    let output = finished.final_output.expect("终态输出");
    assert!(output.contains("end"), "{output}");
}

#[tokio::test]
async fn explicit_background_returns_immediately_and_tees_logs() {
    let harness = Harness::new();
    let started = Instant::now();
    let task = harness
        .start(
            "shell-bg-1",
            "printf 'first\\n'; sleep 0.8; printf 'second\\n'",
            BashMode::Background,
            None,
            None,
        )
        .await;
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "后台应立即返回"
    );
    let state = task.snapshot();
    assert_eq!(state.status, TaskStatus::Running);
    assert!(!state.promoted);

    // 日志文件在运行期间就可读（追加语义）：轮询等待首行出现。
    harness
        .wait_for_log(&task, "first", Duration::from_secs(5))
        .await;
    let log = harness
        .tasks
        .read_log(&task, &handle_for("shell-bg-1"), LogStream::Stdout, 4_096)
        .expect("日志可读");
    assert!(
        log.content.contains("first"),
        "运行期日志: {:?}",
        log.content
    );
    assert!(log.total_bytes > 0);

    let finished = harness.await_task(&task, Some(5_000)).await;
    assert_eq!(finished.status, TaskStatus::Completed);
    assert!(finished
        .final_output
        .expect("输出")
        .contains("first\nsecond"));

    let stdout_path = PathBuf::from(finished.stdout_log.expect("日志路径"));
    assert!(
        stdout_path.starts_with(harness.log_dir()),
        "日志必须在任务私有目录内: {}",
        stdout_path.display()
    );
    let log_content = std::fs::read_to_string(&stdout_path).expect("日志文件");
    assert!(log_content.contains("second"));
}

#[tokio::test]
async fn background_timeout_terminates_process_group() {
    let harness = Harness::new();
    let task = harness
        .start(
            "shell-bg-timeout",
            "sleep 30",
            BashMode::Background,
            Some(300),
            None,
        )
        .await;
    let pgid = task.snapshot().pgid.expect("pgid");
    assert_eq!(task.snapshot().status, TaskStatus::Running);

    let finished = harness.await_task(&task, Some(5_000)).await;
    assert_eq!(finished.status, TaskStatus::TimedOut, "{finished:?}");
    assert!(finished.timed_out);
    assert!(!group_alive(pgid), "超时后进程组必须被回收");
}

#[tokio::test]
async fn cancel_escalates_to_kill_for_term_ignoring_process() {
    let harness = Harness::new();
    let task = harness
        .start(
            "shell-cancel-1",
            "trap '' TERM; printf 'ready\n'; sleep 30",
            BashMode::Background,
            None,
            None,
        )
        .await;
    let pgid = task.snapshot().pgid.expect("pgid");
    assert!(group_alive(pgid));
    // 必须等 trap 真正安装后再发信号，否则 TERM 会命中尚未 trap 的 bash（测试竞态）。
    harness
        .wait_for_log(&task, "ready", Duration::from_secs(5))
        .await;

    let started = Instant::now();
    let cancelled = harness.cancel(&task).await;
    assert!(
        started.elapsed() >= Duration::from_millis(1_500),
        "TERM 被忽略时应等待升级窗口，实际 {:?}（状态 {:?}）",
        started.elapsed(),
        cancelled.status
    );
    assert!(
        cancelled.status.is_terminal(),
        "取消后必须是终态: {:?}",
        cancelled.status
    );
    assert!(!group_alive(pgid), "TERM 无效时应升级 KILL 回收进程组");

    // 幂等：再次停止返回同一终态。
    let again = harness.cancel(&task).await;
    assert_eq!(again.status, cancelled.status);
    assert_eq!(again.exit_code, cancelled.exit_code);
}

#[tokio::test]
async fn request_cancellation_kills_group_and_keeps_partial_output() {
    let harness = Harness::new();
    let token = tokio_util::sync::CancellationToken::new();
    let payload = BashStartPayload {
        task_id: "shell-cancel-req".to_string(),
        log_handle: handle_for("shell-cancel-req"),
        command: "trap '' TERM; printf 'ready\n'; sleep 30".to_string(),
        cwd: harness._workspace.path().to_string_lossy().to_string(),
        mode: BashMode::Foreground,
        timeout_ms: None,
        await_ms: None,
    };
    let handle = tokio::spawn({
        let token = token.clone();
        let tasks = Arc::clone(&harness.tasks);
        async move { tasks.start(payload, Some(token)).await }
    });
    // 等待日志出现后再取消（trap 已安装）。
    let started = Instant::now();
    loop {
        let log = harness.tasks.logs().read_tail(
            &handle_for("shell-cancel-req"),
            LogStream::Stdout,
            4_096,
        );
        if log
            .map(|chunk| chunk.content.contains("ready"))
            .unwrap_or(false)
        {
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(5), "等待日志超时");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    token.cancel();
    let task = handle.await.expect("任务句柄").expect("start 返回");
    let state = task.snapshot();
    assert!(state.cancelled, "应标记为客户端取消");
    assert!(
        state.status.is_terminal(),
        "取消后为终态: {:?}",
        state.status
    );
    let pgid = state.pgid.expect("pgid");
    assert!(
        !group_alive(pgid),
        "取消必须回收进程组（TERM 被 trap 时升级 KILL）"
    );
    assert!(state.stdout.contains("ready"), "部分输出应保留");
}

#[tokio::test]
async fn log_read_rejects_wrong_handle() {
    let harness = Harness::new();
    let task = harness
        .start("shell-guard-1", "sleep 5", BashMode::Background, None, None)
        .await;

    // 句柄不匹配即拒绝（猜测/重放防线；注册表在**进程内**注入正确句柄）。
    let guessed = harness
        .tasks
        .read_log(&task, &"f".repeat(64), LogStream::Stdout, 128);
    assert!(matches!(guessed, Err(BashTaskError::HandleMismatch { .. })));

    // 未知任务 id 由注册表的授权层拒绝（对象消失的是跨进程的"worker 表查询"）。
    let payload = BashStartPayload {
        task_id: "shell-does-not-exist".to_string(),
        log_handle: handle_for("shell-does-not-exist"),
        command: "true".to_string(),
        cwd: harness._workspace.path().to_string_lossy().to_string(),
        mode: BashMode::Background,
        timeout_ms: None,
        await_ms: None,
    };
    // 任务层不做 id 存在性判定（表在注册表）；这里确认启动本身成功且能被回收。
    let other = harness.tasks.start(payload, None).await.expect("启动");
    harness.cancel(&other).await;

    harness.cancel(&task).await;
}

#[tokio::test]
async fn log_read_reports_truncation_of_tail_window() {
    let harness = Harness::new();
    let task = harness
        .start(
            "shell-log-1",
            "for i in 1 2 3 4 5 6 7 8 9 10; do echo \"line-$i\"; done",
            BashMode::Foreground,
            None,
            None,
        )
        .await;
    assert_eq!(task.snapshot().status, TaskStatus::Completed);

    let full = harness
        .tasks
        .read_log(&task, &handle_for("shell-log-1"), LogStream::Stdout, 4_096)
        .expect("日志");
    assert!(!full.truncated);
    assert!(full.content.contains("line-10"));

    let tail = harness
        .tasks
        .read_log(&task, &handle_for("shell-log-1"), LogStream::Stdout, 8)
        .expect("日志");
    assert!(tail.truncated, "超窗口读取应标记截断");
    assert!(tail.total_bytes > tail.content.len() as u64);
    assert!(tail.content.len() <= 8, "尾部窗口不超过上限");
}

#[tokio::test]
async fn truncated_output_is_persisted_with_path() {
    let harness = Harness::new();
    let task = harness
        .start(
            "shell-big-1",
            "head -c 70000 /dev/zero | tr '\\0' 'x'",
            BashMode::Foreground,
            None,
            None,
        )
        .await;
    let state = task.snapshot();
    assert_eq!(state.status, TaskStatus::Completed);
    assert!(state.truncated, "超过 65000 字节应标记截断");
    let output = state.final_output.expect("输出");
    assert!(output.contains("[Output truncated: exceeds 65000 byte limit]"));
    assert!(output.contains("[Full output saved to"));
    let path = PathBuf::from(state.persisted_path.expect("落盘路径"));
    assert!(path.exists(), "落盘文件应存在: {}", path.display());
    assert_eq!(
        std::fs::metadata(&path).expect("metadata").len(),
        70_000,
        "落盘必须是完整输出"
    );
}

#[tokio::test]
async fn cancel_kills_descendants_in_the_same_process_group() {
    let harness = Harness::new();
    let task = harness
        .start(
            "shell-grandchild",
            "sh -c 'echo $$; echo READY; sleep 30'",
            BashMode::Background,
            None,
            None,
        )
        .await;
    let pgid = task.snapshot().pgid.expect("pgid");
    harness
        .wait_for_log(&task, "READY", Duration::from_secs(5))
        .await;
    let log = harness
        .tasks
        .read_log(
            &task,
            &handle_for("shell-grandchild"),
            LogStream::Stdout,
            256,
        )
        .expect("日志");
    let descendant: i32 = log
        .content
        .lines()
        .find_map(|line| line.trim().parse::<i32>().ok())
        .expect("应打印子进程 pid");
    // SAFETY: 信号 0 只做存在性检查。
    assert!(unsafe { libc::kill(descendant, 0) == 0 }, "子进程应存活");

    harness.cancel(&task).await;
    // SAFETY: 同上。
    assert!(
        unsafe { libc::kill(descendant, 0) != 0 },
        "取消后子孙进程也必须被回收（同进程组）"
    );
    assert!(!group_alive(pgid));
}

#[tokio::test]
async fn partial_capture_is_capped_while_log_keeps_everything() {
    let harness = Harness::new();
    let task = harness
        .start(
            "shell-cap-1",
            "head -c 2200000 /dev/zero | tr '\\0' 'x'",
            BashMode::Foreground,
            None,
            None,
        )
        .await;
    let state = task.snapshot();
    assert_eq!(state.status, TaskStatus::Completed);
    assert!(
        state.stdout.len() <= local_mcp_server::tasks::MAX_PARTIAL_CAPTURE_BYTES,
        "捕获缓冲上限 2 MiB，实际 {}",
        state.stdout.len()
    );
    let log_path = PathBuf::from(state.stdout_log.expect("日志路径"));
    let logged = std::fs::metadata(&log_path).expect("日志").len();
    assert_eq!(logged, 2_200_000, "日志必须保留全部输出（tee 语义）");
}

#[tokio::test]
async fn working_directory_is_fixed_per_invocation() {
    let harness = Harness::new();
    let workspace = harness._workspace.path().to_string_lossy().to_string();

    let first = harness
        .start(
            "shell-cd-1",
            "cd / && pwd",
            BashMode::Foreground,
            None,
            None,
        )
        .await;
    let first_state = first.snapshot();
    assert_eq!(first_state.status, TaskStatus::Completed);
    assert_eq!(
        first_state.final_output.expect("输出").trim(),
        "/",
        "同一次调用内的 cd 生效"
    );

    let second = harness
        .start("shell-cd-2", "pwd", BashMode::Foreground, None, None)
        .await;
    // macOS 上 tempdir 可能经 /var → /private/var 符号链接，比较 canonicalize 结果。
    let reported = second
        .snapshot()
        .final_output
        .expect("输出")
        .trim()
        .to_string();
    assert_eq!(
        std::fs::canonicalize(&reported).expect("canonicalize 输出路径"),
        std::fs::canonicalize(&workspace).expect("canonicalize 工作区"),
        "cd 不跨调用持久（每次调用从工作区开始）：{reported}"
    );
}

#[tokio::test]
async fn shutdown_kills_all_running_tasks() {
    let harness = Harness::new();
    let first = harness
        .start("shell-shut-1", "sleep 30", BashMode::Background, None, None)
        .await;
    let second = harness
        .start("shell-shut-2", "sleep 30", BashMode::Background, None, None)
        .await;
    let pgids = [
        first.snapshot().pgid.expect("pgid"),
        second.snapshot().pgid.expect("pgid"),
    ];

    harness
        .tasks
        .shutdown(&[Arc::clone(&first), Arc::clone(&second)])
        .await;

    for pgid in pgids {
        assert!(!group_alive(pgid), "关闭后不得留下进程组");
    }
    assert!(
        first.is_terminal() && second.is_terminal(),
        "关闭必须落终态"
    );
}
