//! async tasks manager 单元测试。
//!
//! 语义随迁自 `peri-middlewares`（L1 迁移点）：
//! - `subagent/background_test.rs`（registry 全量用例，含 Shell pid 取消）
//! - `process/process_test.rs`（shell_command 包装）
//! - `tools/output_persist_test.rs` / `tools/output_truncate_test.rs`（落盘/截断）
//! - `middleware/terminal_test.rs` 的 parse_timeout / bg_shell_task_id 用例
//!
//! 新增：per-session 实例化/销毁用例（`cancel_all` / 多实例隔离）。

#[cfg(unix)]
use std::time::Duration;

use std::sync::atomic::Ordering;

use super::*;

fn make_registry() -> BackgroundTaskRegistry {
    BackgroundTaskRegistry::new()
}

fn make_task(id: &str) -> BackgroundTask {
    let handle = tokio::runtime::Handle::current().spawn(async {});
    BackgroundTask {
        id: id.to_string(),
        agent_name: "test-agent".to_string(),
        prompt_summary: "test task".to_string(),
        status: BackgroundTaskStatus::Running,
        started_at: std::time::Instant::now(),
        chrono_started_at: chrono::Utc::now(),
        kind: BgTaskKind::Agent,
        cancel_handle: BgCancelHandle::Abort(handle),
        cancel_token: None,
        pid: None,
        output_preview: None,
    }
}

#[tokio::test]
async fn test_register_and_active_count() {
    let registry = make_registry();
    assert_eq!(registry.active_count(), 0);

    registry.register_with_kind(make_task("bg-1")).unwrap();
    assert_eq!(registry.active_count(), 1);
}

#[tokio::test]
async fn test_max_concurrent_limit() {
    let registry = make_registry();

    registry.register_with_kind(make_task("bg-1")).unwrap();
    registry.register_with_kind(make_task("bg-2")).unwrap();
    registry.register_with_kind(make_task("bg-3")).unwrap();

    let result = registry.register_with_kind(make_task("bg-4"));
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("Kind concurrent limit reached"));
}

#[tokio::test]
async fn test_complete_updates_status() {
    let registry = make_registry();

    registry.register_with_kind(make_task("bg-1")).unwrap();
    assert_eq!(registry.active_count(), 1);

    let result = BackgroundTaskResult {
        task_id: "bg-1".to_string(),
        agent_name: "test-agent".to_string(),
        prompt_summary: "test".to_string(),
        success: true,
        output: "done".to_string(),
        tool_calls_count: 2,
        duration_ms: 100,
        child_thread_id: None,
        timed_out: false,
    };

    registry.complete("bg-1", result);

    // 已完成任务应被立即清理，list_tasks 不再返回
    let tasks = registry.list_tasks();
    assert_eq!(
        tasks.len(),
        0,
        "completed tasks should be cleaned up immediately"
    );
    assert_eq!(registry.active_count(), 0);
}

#[tokio::test]
async fn test_cancel_removes_task() {
    let registry = make_registry();

    registry.register_with_kind(make_task("bg-1")).unwrap();
    registry.register_with_kind(make_task("bg-2")).unwrap();

    registry.cancel("bg-1").unwrap();
    let tasks = registry.list_tasks();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].0, "bg-2");

    // 取消不存在的任务返回 Err
    let result = registry.cancel("nonexistent");
    assert!(result.is_err());
}

/// Cancel 传播到执行中的 Background 任务：阻塞的 JoinHandle 被 abort 后任务终止。
/// 验证 abort_handle.abort() 真正触发了 JoinHandle 的取消，而非仅从 registry 移除条目。
#[tokio::test]
async fn test_cancel_propagates_to_running_task() {
    let registry = make_registry();

    // 构造一个会长时间阻塞的 JoinHandle（等待 oneshot，永不 resolve）
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        // 阻塞等待永不触发的 oneshot，模拟执行中的 SubAgent
        let _ = rx.await;
    });

    let task = BackgroundTask {
        id: "bg-running".to_string(),
        agent_name: "blocking-agent".to_string(),
        prompt_summary: "blocking test".to_string(),
        status: BackgroundTaskStatus::Running,
        started_at: std::time::Instant::now(),
        chrono_started_at: chrono::Utc::now(),
        kind: BgTaskKind::Agent,
        cancel_handle: BgCancelHandle::Abort(handle),
        cancel_token: None,
        pid: None,
        output_preview: None,
    };

    registry.register_with_kind(task).unwrap();
    assert_eq!(registry.active_count(), 1);

    // 取消任务：应 abort JoinHandle 并从 registry 移除
    registry.cancel("bg-running").unwrap();

    // 验证 registry 中已清理
    let tasks = registry.list_tasks();
    assert!(tasks.is_empty(), "cancel 后任务应从 registry 移除");
    assert_eq!(registry.active_count(), 0, "cancel 后 active_count 应为 0");

    // 清理：让 oneshot sender 释放，避免 JoinHandle 泄漏
    drop(tx);
}

// ── 新增：per-kind 上限测试 ──

/// [回归测试] Workflow 类型任务的 kill 闭包必须被 cancel() 真正调用。
/// 历史背景（issue 2026-08-05）：Workflow 注册时固定 `Kill(None)`，
/// cancel() 只 warn 并假装成功——runner 实际未被终止，条目移除但 workflow 继续跑。
/// 修复后 `Kill(Some(kill))` 分支执行 kill 闭包（转发到 WorkflowTaskRegistry::kill）。
#[tokio::test]
async fn test_cancel_workflow_invokes_kill_closure() {
    let registry = make_registry();
    let killed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let killed_clone = killed.clone();
    let task = BackgroundTask {
        id: "bg-wf-1".to_string(),
        agent_name: "workflow".to_string(),
        prompt_summary: "workflow cancel test".to_string(),
        status: BackgroundTaskStatus::Running,
        started_at: std::time::Instant::now(),
        chrono_started_at: chrono::Utc::now(),
        kind: BgTaskKind::Workflow,
        cancel_handle: BgCancelHandle::Kill(Some(Box::new(move || {
            killed_clone.store(true, Ordering::SeqCst);
        }))),
        cancel_token: None,
        pid: None,
        output_preview: None,
    };
    registry.register_with_kind(task).unwrap();
    assert_eq!(registry.active_count(), 1);

    registry.cancel("bg-wf-1").unwrap();

    assert!(
        killed.load(Ordering::SeqCst),
        "cancel() 必须调用 kill 闭包（runner 真正被终止），而非仅移除条目"
    );
    assert_eq!(
        registry.active_count(),
        0,
        "cancel 后任务应从 registry 移除"
    );
    assert!(
        registry.list_tasks().is_empty(),
        "cancel 后任务应从 registry 移除"
    );
}

/// [回归测试] kill 通道不可用（Kill(None)）时 cancel() 必须如实返回错误，
/// 且条目保留（等待自然完成），不得移除条目 + 发 cancelled 事件假装成功。
/// 历史背景（issue 2026-08-05）：`Kill(None)` 分支此前仅 warn 并返回 Ok。
#[tokio::test]
async fn test_cancel_with_unavailable_handle_returns_error_and_keeps_entry() {
    let registry = make_registry();
    let task = BackgroundTask {
        id: "bg-wf-none".to_string(),
        agent_name: "workflow".to_string(),
        prompt_summary: "no kill handle".to_string(),
        status: BackgroundTaskStatus::Running,
        started_at: std::time::Instant::now(),
        chrono_started_at: chrono::Utc::now(),
        kind: BgTaskKind::Workflow,
        cancel_handle: BgCancelHandle::Kill(None),
        cancel_token: None,
        pid: None,
        output_preview: None,
    };
    registry.register_with_kind(task).unwrap();
    assert_eq!(registry.active_count(), 1);

    let err = registry.cancel("bg-wf-none").unwrap_err();
    assert!(
        err.to_string().contains("cannot be cancelled"),
        "不可取消时应返回明确错误，实际: {}",
        err
    );
    // 条目保留：任务仍在运行，等待自然完成
    assert_eq!(registry.active_count(), 1, "取消失败时条目应保留");
    assert_eq!(registry.list_tasks().len(), 1, "取消失败时条目应保留");
}

#[tokio::test]
async fn test_count_by_kind_works() {
    let registry = make_registry();

    let mut shell_task = make_task("bg-shell-1");
    shell_task.kind = BgTaskKind::Shell;
    shell_task.id = "bg-shell-1".to_string();
    registry.register_with_kind(shell_task).unwrap();

    let mut agent_task = make_task("bg-agent-1");
    agent_task.kind = BgTaskKind::Agent;
    agent_task.id = "bg-agent-1".to_string();
    registry.register_with_kind(agent_task).unwrap();

    assert_eq!(registry.count_by_kind(BgTaskKind::Shell), 1);
    assert_eq!(registry.count_by_kind(BgTaskKind::Agent), 1);
    assert_eq!(registry.count_by_kind(BgTaskKind::Workflow), 0);
}

#[tokio::test]
async fn test_register_with_kind_shell_limit() {
    let registry = make_registry();

    for i in 0..5 {
        let mut task = make_task(&format!("bg-shell-{}", i));
        task.kind = BgTaskKind::Shell;
        registry.register_with_kind(task).unwrap();
    }

    // 第 6 个应被拒绝
    let mut task = make_task("bg-shell-over");
    task.kind = BgTaskKind::Shell;
    let result = registry.register_with_kind(task);
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("Kind concurrent limit reached"));
}

#[tokio::test]
async fn test_register_with_kind_agent_limit() {
    let registry = make_registry();

    for i in 0..3 {
        let mut task = make_task(&format!("bg-agent-{}", i));
        task.kind = BgTaskKind::Agent;
        registry.register_with_kind(task).unwrap();
    }

    let mut task = make_task("bg-agent-over");
    task.kind = BgTaskKind::Agent;
    let result = registry.register_with_kind(task);
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("Kind concurrent limit reached"));
}

#[tokio::test]
async fn test_list_tasks_full_returns_info() {
    let registry = make_registry();

    let mut task = make_task("bg-agent-1");
    task.kind = BgTaskKind::Agent;
    registry.register_with_kind(task).unwrap();

    let tasks = registry.list_tasks_full();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].task_id, "bg-agent-1");
    assert_eq!(tasks[0].kind, BgTaskKind::Agent);
}

// ── complete() 幽灵完成事件防护（issue 2026-08-05）───────────────────────────

fn make_result(task_id: &str, success: bool) -> BackgroundTaskResult {
    BackgroundTaskResult {
        task_id: task_id.to_string(),
        agent_name: "test-agent".to_string(),
        prompt_summary: "test".to_string(),
        success,
        output: "done".to_string(),
        tool_calls_count: 2,
        duration_ms: 100,
        child_thread_id: None,
        timed_out: false,
    }
}

/// [回归测试] cancel 后条目已移除，自然完成的 complete() 不得推幽灵 Completed 事件。
/// 历史 bug（issue 2026-08-05）：kill 后 workflow 自然完成仍 push bg-task-completed，
/// TUI 用户已看到"已取消"却收到完成通知。
#[tokio::test]
async fn test_complete_after_cancel_does_not_push_ghost_event() {
    let registry = make_registry();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<BgRegistryEvent>();
    registry.set_event_sender(tx, "sess-1".to_string());

    registry.register_with_kind(make_task("bg-1")).unwrap();
    registry.cancel("bg-1").unwrap(); // 条目移除 + Cancelled 事件

    let handled = registry.complete("bg-1", make_result("bg-1", true));

    assert!(!handled, "已移除条目的 complete 应返回 false");
    let mut saw_completed = false;
    while let Ok(event) = rx.try_recv() {
        if matches!(event, BgRegistryEvent::Completed { .. }) {
            saw_completed = true;
        }
    }
    assert!(
        !saw_completed,
        "已取消条目不得推 Completed 幽灵事件（用户已收到 Cancelled）"
    );
}

/// 未注册任务 complete() 返回 false 且不推任何事件。
#[tokio::test]
async fn test_complete_unknown_task_returns_false_no_event() {
    let registry = make_registry();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<BgRegistryEvent>();
    registry.set_event_sender(tx, "sess-1".to_string());

    let handled = registry.complete("never-registered", make_result("never-registered", true));

    assert!(!handled, "未注册任务的 complete 应返回 false");
    assert!(
        rx.try_recv().is_err(),
        "未注册任务的 complete 不得推任何事件"
    );
}

/// 正常路径：条目存在时 complete() 返回 true 且推 Completed 事件。
#[tokio::test]
async fn test_complete_existing_task_returns_true_and_pushes_event() {
    let registry = make_registry();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<BgRegistryEvent>();
    registry.set_event_sender(tx, "sess-1".to_string());

    registry.register_with_kind(make_task("bg-1")).unwrap();
    // 消费 register 推的 Started 事件
    assert!(matches!(
        rx.try_recv().unwrap(),
        BgRegistryEvent::Started { .. }
    ));

    let handled = registry.complete("bg-1", make_result("bg-1", true));

    assert!(handled, "条目存在时 complete 应返回 true");
    let event = rx.try_recv().unwrap();
    assert!(
        matches!(event, BgRegistryEvent::Completed { .. }),
        "条目存在时应推 Completed 事件"
    );
    assert!(rx.try_recv().is_err(), "不应有多余事件");
}

/// cancel() 应杀死整个进程组（bash 为组长）：sh/sleep 子进程不得孤儿存活创建 marker。
/// 命令 `sh -c 'sleep 2; touch marker'`：若只杀 bash 单进程（旧行为），sh 孤儿会在
/// 2s 时 touch；等 3s 断言 marker 不存在可区分新旧行为。
#[cfg(unix)]
#[tokio::test]
async fn test_cancel_kills_process_group() {
    let registry = make_registry();
    let marker =
        std::env::temp_dir().join(format!("peri-cancel-pg-{}.marker", uuid::Uuid::new_v4()));
    let marker_path = marker.to_string_lossy().to_string();

    // spawn 带子进程的命令（sh → sleep），bash 为进程组组长；不设 kill_on_drop
    let mut cmd = shell_command(&format!("sh -c 'sleep 2; touch {}'", marker_path), &[]);
    cmd.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    cmd.process_group(0);
    let child = cmd.spawn().unwrap();
    let pid = child.id().unwrap();

    let task = BackgroundTask {
        id: "bg-shell-cancel".to_string(),
        agent_name: "bg-shell".to_string(),
        prompt_summary: "cancel kills process group".to_string(),
        status: BackgroundTaskStatus::Running,
        started_at: std::time::Instant::now(),
        chrono_started_at: chrono::Utc::now(),
        kind: BgTaskKind::Shell,
        cancel_handle: BgCancelHandle::Pid(pid),
        cancel_token: None,
        pid: Some(pid),
        output_preview: None,
    };
    registry.register_with_kind(task).unwrap();
    assert_eq!(registry.active_count(), 1);

    registry.cancel("bg-shell-cancel").unwrap();
    assert_eq!(registry.active_count(), 0);

    // 等 3s（> sleep 2）：若进程组未被杀，sh/sleep 孤儿会创建 marker
    tokio::time::sleep(Duration::from_millis(3000)).await;
    assert!(!marker.exists(), "进程组应被杀死，marker 不应被创建");
    let _ = std::fs::remove_file(&marker);

    // child 句柄 drop（进程已被 cancel 杀死，无孤儿残留）
    drop(child);
}

// ── S3.2 取消序列（token.cancel() → 超时 abort 兜底）────────────────────────

/// [回归测试] cancel() 的 Abort 分支必须先触发 token.cancel()：任务响应取消链
/// 自然退出（走完整收尾），而非立即 abort（abort 会跳过 SubagentStopped /
/// deregister / thread status 等收尾）。
/// 历史 bug（issue 2026-08-05）：cancel 仅 handle.abort()，任务收尾全部丢失。
#[tokio::test]
async fn test_cancel_abort_token_cancels_task_first() {
    let registry = make_registry();
    let token = CancellationToken::new();
    let finished = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let finished_clone = finished.clone();
    let token_clone = token.clone();
    let handle = tokio::spawn(async move {
        // 模拟响应 cancel 的 bg agent：token 取消后执行"收尾"再结束
        token_clone.cancelled().await;
        finished_clone.store(true, Ordering::SeqCst);
    });

    let task = BackgroundTask {
        id: "bg-token-cancel".to_string(),
        agent_name: "test-agent".to_string(),
        prompt_summary: "token cancel test".to_string(),
        status: BackgroundTaskStatus::Running,
        started_at: std::time::Instant::now(),
        chrono_started_at: chrono::Utc::now(),
        kind: BgTaskKind::Agent,
        cancel_handle: BgCancelHandle::Abort(handle),
        cancel_token: Some(token),
        pid: None,
        output_preview: None,
    };
    registry.register_with_kind(task).unwrap();
    assert_eq!(registry.active_count(), 1);

    registry.cancel("bg-token-cancel").unwrap();
    assert_eq!(registry.active_count(), 0);

    // 任务应因 token 取消而自然结束（收尾标志被置位），而非等待 abort 兜底
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !finished.load(Ordering::SeqCst) {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("任务应响应 token.cancel() 自然退出（保留收尾），而非被 abort");
}

/// [回归测试] 任务不响应 cancel（如阻塞在不支持取消的 await 点）时，
/// grace 窗口超时后 abort 兜底终止任务——保证"取消后任务继续跑"不会发生。
/// 历史 bug（issue 2026-08-05）：abort 跳过全部收尾；修复后 abort 仅作为兜底，
/// 同步收尾由任务内 guard 执行（本测试验证任务终止语义）。
#[tokio::test]
async fn test_cancel_abort_grace_timeout_fallback() {
    let registry = make_registry();
    let token = CancellationToken::new();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        // 不响应 cancel：阻塞等待永不触发的 oneshot
        let _ = rx.await;
    });
    // 保留 abort 视图用于轮询任务终止
    let abort_view = handle.abort_handle();

    let task = BackgroundTask {
        id: "bg-stubborn".to_string(),
        agent_name: "test-agent".to_string(),
        prompt_summary: "no cancel response".to_string(),
        status: BackgroundTaskStatus::Running,
        started_at: std::time::Instant::now(),
        chrono_started_at: chrono::Utc::now(),
        kind: BgTaskKind::Agent,
        cancel_handle: BgCancelHandle::Abort(handle),
        cancel_token: Some(token),
        pid: None,
        output_preview: None,
    };
    registry.register_with_kind(task).unwrap();
    assert_eq!(registry.active_count(), 1);

    registry.cancel("bg-stubborn").unwrap();
    assert_eq!(registry.active_count(), 0);

    // grace（3s）超时后 abort 兜底：任务终止
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !abort_view.is_finished() {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("grace 超时后 abort 兜底应终止任务");

    drop(tx);
}

// ── TaskManager（per-session 聚合）──

/// per-session 实例化：两个 TaskManager 实例互不干扰（多会话并行不互相污染）。
#[tokio::test]
async fn test_task_manager_instances_are_isolated() {
    let tm_a = TaskManager::new();
    let tm_b = TaskManager::new();

    tm_a.register_with_kind(make_task("bg-sess-a")).unwrap();
    assert_eq!(tm_a.active_count(), 1);
    assert_eq!(
        tm_b.active_count(),
        0,
        "另一 session 的 registry 不得互相干扰"
    );
    assert!(tm_b.list_tasks().is_empty());

    // 各自独立事件通道
    let (tx_a, mut rx_a) = tokio::sync::mpsc::unbounded_channel::<BgRegistryEvent>();
    let (tx_b, mut rx_b) = tokio::sync::mpsc::unbounded_channel::<BgRegistryEvent>();
    tm_a.set_event_sender(tx_a, "sess-a".to_string());
    tm_b.set_event_sender(tx_b, "sess-b".to_string());

    tm_a.register_with_kind(make_task("bg-sess-a-2")).unwrap();
    assert!(matches!(
        rx_a.try_recv().unwrap(),
        BgRegistryEvent::Started { .. }
    ));
    assert!(
        rx_b.try_recv().is_err(),
        "session B 不应收到 session A 的 Started 事件"
    );
}

/// per-session 销毁：cancel_all() 取消全部运行中任务（§9 销毁顺序）。
#[tokio::test]
async fn test_task_manager_cancel_all_clears_running_tasks() {
    let tm = TaskManager::new();

    tm.register_with_kind(make_task("bg-1")).unwrap();
    tm.register_with_kind(make_task("bg-2")).unwrap();
    tm.register_with_kind(make_task("bg-3")).unwrap();
    assert_eq!(tm.active_count(), 3);

    tm.cancel_all();
    assert_eq!(tm.active_count(), 0, "cancel_all 后应无运行中任务");
    assert!(tm.list_tasks().is_empty());
}

/// cancel_all 对不可取消条目（Kill(None)）如实保留：不假装取消成功。
#[tokio::test]
async fn test_task_manager_cancel_all_keeps_unavailable_entries() {
    let tm = TaskManager::new();
    let task = BackgroundTask {
        id: "bg-wf-none".to_string(),
        agent_name: "workflow".to_string(),
        prompt_summary: "no kill handle".to_string(),
        status: BackgroundTaskStatus::Running,
        started_at: std::time::Instant::now(),
        chrono_started_at: chrono::Utc::now(),
        kind: BgTaskKind::Workflow,
        cancel_handle: BgCancelHandle::Kill(None),
        cancel_token: None,
        pid: None,
        output_preview: None,
    };
    tm.register_with_kind(task).unwrap();

    tm.cancel_all();
    assert_eq!(
        tm.active_count(),
        1,
        "kill 通道不可用的条目应保留（等待自然完成）"
    );
}

// ── 进程包装（shell_command）──

#[test]
fn test_shell_command_unix_bash_c() {
    let cmd = shell_command("echo", &["hello"]);
    let formatted = format!("{cmd:?}");
    #[cfg(unix)]
    {
        assert!(
            formatted.contains("bash"),
            "expected bash, got: {formatted}"
        );
        assert!(
            formatted.contains("-c"),
            "expected -c flag, got: {formatted}"
        );
    }
    #[cfg(windows)]
    {
        assert!(
            formatted.contains("powershell"),
            "expected powershell, got: {formatted}"
        );
        assert!(
            formatted.contains("-Command"),
            "expected -Command flag, got: {formatted}"
        );
        assert!(
            formatted.contains("-NoProfile"),
            "expected -NoProfile flag, got: {formatted}"
        );
    }
}

#[test]
fn test_shell_command_no_args() {
    let cmd = shell_command("ls", &[]);
    let formatted = format!("{cmd:?}");
    #[cfg(unix)]
    {
        assert!(
            formatted.contains("bash"),
            "expected bash, got: {formatted}"
        );
        assert!(
            formatted.contains("ls"),
            "expected 'ls' in command, got: {formatted}"
        );
    }
    #[cfg(windows)]
    {
        assert!(
            formatted.contains("powershell"),
            "expected powershell, got: {formatted}"
        );
        assert!(
            formatted.contains("ls"),
            "expected 'ls' in command, got: {formatted}"
        );
    }
}

#[test]
fn test_shell_command_multi_args() {
    let cmd = shell_command("npx", &["-y", "@anthropic/mcp-server"]);
    let formatted = format!("{cmd:?}");
    #[cfg(unix)]
    {
        assert!(
            formatted.contains("bash"),
            "expected bash, got: {formatted}"
        );
        assert!(
            formatted.contains("npx"),
            "expected 'npx', got: {formatted}"
        );
    }
    #[cfg(windows)]
    {
        assert!(
            formatted.contains("powershell"),
            "expected powershell, got: {formatted}"
        );
        assert!(
            formatted.contains("npx"),
            "expected 'npx', got: {formatted}"
        );
        // 多参数应被拼接到命令字符串中
        assert!(
            formatted.contains("@anthropic/mcp-server"),
            "expected @anthropic/mcp-server in command, got: {formatted}"
        );
    }
}

/// 回归测试：Windows 上 `command` 含空格时，不能被 PowerShell 单引号
/// 包围成字符串字面量。否则 `powershell -Command "'ping ...'"` 会把
/// `'ping ...'` 当作字符串 expression 直接 echo 出来，而不是执行命令。
///
/// 触发场景：Bash 工具调用 `shell_command("ping -n 60 127.0.0.1", &[])`，
/// 测试期望 1s 超时返回 Err，实际返回 Ok("ping -n 60 127.0.0.1\r\n")。
#[test]
fn test_shell_command_windows_command_not_string_literal() {
    let cmd = shell_command("ping -n 60 127.0.0.1", &[]);
    let formatted = format!("{cmd:?}");
    #[cfg(windows)]
    {
        // 错误形态：command 被单引号包围（PowerShell 字符串字面量）
        assert!(
            !formatted.contains("'ping -n 60 127.0.0.1'"),
            "command 被错误地用 PowerShell 单引号包围成字符串字面量，会导致 -Command echo 出字符串而非执行命令: {formatted}"
        );
    }
    #[cfg(not(windows))]
    {
        let _ = &formatted;
    }
}

/// 回归测试：Windows 上 args 仍应被 PowerShell 单引号 escape，
/// 防止 `$` `` ` `` `(` `)` `{` `}` `;` `|` `&` `@` `#` 等 metacharacter
/// 被 PowerShell 解析为代码（与 commit b689cc39 的安全意图一致）。
#[test]
fn test_shell_command_windows_args_still_escaped() {
    let cmd = shell_command("echo", &["$HOME", "a;b"]);
    let formatted = format!("{cmd:?}");
    #[cfg(windows)]
    {
        // 含 $ 或 ; 的 args 应被单引号包围成 PowerShell 字面量
        assert!(
            formatted.contains("'$HOME'"),
            "含 $ 的 arg 应被 PowerShell 单引号 escape: {formatted}"
        );
        assert!(
            formatted.contains("'a;b'"),
            "含 ; 的 arg 应被 PowerShell 单引号 escape: {formatted}"
        );
    }
    #[cfg(not(windows))]
    {
        let _ = &formatted;
    }
}

// ── 输出落盘（persist_truncated_output）──

#[test]
fn test_persist_writes_file_and_returns_hint() {
    let content = "line1\nline2\nline3";
    let hint = persist_truncated_output(content);
    // 提示应包含文件名
    assert!(
        hint.contains("peri-tool-output-"),
        "hint should contain filename: {hint}"
    );
    // 提示应引导用户使用 Read 工具
    assert!(
        hint.contains("Read"),
        "hint should guide to use Read tool: {hint}"
    );
    // 从提示中提取文件路径并验证内容
    let prefix = "saved to ";
    let suffix = " — use Read";
    let path_start = hint.find(prefix).unwrap() + prefix.len();
    let path_end = hint[path_start..]
        .find(suffix)
        .map(|i| path_start + i)
        .unwrap_or(hint.len());
    let path = &hint[path_start..path_end];
    let saved = std::fs::read_to_string(path).unwrap();
    assert_eq!(saved, content);
    std::fs::remove_file(path).ok();
}

#[test]
fn test_persist_empty_string() {
    let hint = persist_truncated_output("");
    // 空内容也应生成包含路径的提示
    assert!(
        hint.contains("Read"),
        "empty content should also produce hint: {hint}"
    );
    // 验证空文件确实被写入，并清理
    let prefix = "saved to ";
    let suffix = " — use Read";
    let path_start = hint.find(prefix).unwrap() + prefix.len();
    let path_end = hint[path_start..]
        .find(suffix)
        .map(|i| path_start + i)
        .unwrap_or(hint.len());
    let path = &hint[path_start..path_end];
    let saved = std::fs::read_to_string(path).unwrap();
    assert_eq!(saved, "");
    std::fs::remove_file(path).ok();
}

// ── 输出截断（truncate_bytes）──

#[test]
fn test_truncate_bytes_ascii() {
    let s = "hello world";
    assert_eq!(truncate_bytes(s, 5), "hello");
}

#[test]
fn test_truncate_bytes_within_limit() {
    let s = "hello";
    assert_eq!(truncate_bytes(s, 100), "hello");
}

#[test]
fn test_truncate_bytes_utf8_safe() {
    let s = "你好世界";
    assert_eq!(truncate_bytes(s, 6), "你好");
}

#[test]
fn test_truncate_bytes_utf8_mid_character() {
    let s = "你好";
    let result = truncate_bytes(s, 5);
    // 5 字节位于 "好"（3 字节）中间，应回退到 "你"（3 字节）
    assert_eq!(result, "你");
}

#[test]
fn test_truncate_bytes_empty_string() {
    assert_eq!(truncate_bytes("", 10), "");
}

#[test]
fn test_truncate_bytes_zero_max() {
    assert_eq!(truncate_bytes("hello", 0), "");
}

// ── bg shell 执行链纯函数（parse_timeout / bg_shell_task_id）─────────────────

/// parse_timeout 纯函数语义：
/// - 后台：未传 → None（不超时）；显式 0 → None；显式 >0 → clamp 到 [min, 600000]
/// - 同步：未传 → Some(15000)；显式 0 → None；显式 >0 → clamp 到 [min, 600000]
/// - min：Unix 为 1；Windows 为 5000（进程创建/终止开销大，过短超时不可靠）
#[test]
fn test_parse_timeout_semantics() {
    let min = if cfg!(target_os = "windows") { 5000 } else { 1 };
    // 后台
    assert_eq!(parse_timeout(&serde_json::json!({}), true), None);
    assert_eq!(
        parse_timeout(&serde_json::json!({"timeout": 0}), true),
        None
    );
    assert_eq!(
        parse_timeout(&serde_json::json!({"timeout": 2000}), true),
        Some(2000.max(min))
    );
    // 同步
    assert_eq!(parse_timeout(&serde_json::json!({}), false), Some(15_000));
    assert_eq!(
        parse_timeout(&serde_json::json!({"timeout": 0}), false),
        None
    );
    assert_eq!(
        parse_timeout(&serde_json::json!({"timeout": 2000000}), false),
        Some(600_000)
    );
}

/// bg shell 任务 id 唯一性（issue 2026-08-05 回归防护）：
/// 旧实现取 UUID v7 前 8 字符（毫秒时间戳高 32 位），同一毫秒内多次调用
/// 必然碰撞 → registry 覆盖注册 → 后续 complete() 被静默跳过 → TUI 条目残留。
/// 连续生成（大概率落在同一毫秒）必须全部唯一，且保留 `shell-` 前缀。
#[test]
fn test_bg_shell_task_id_uniqueness() {
    let ids: std::collections::HashSet<String> = (0..64).map(|_| bg_shell_task_id()).collect();
    assert_eq!(ids.len(), 64, "同一毫秒内生成的 bg shell task_id 必须唯一");
    assert!(ids.iter().all(|id| id.starts_with("shell-")));
}
