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
        cancel_handle: BgCancelHandle::Abort(handle.abort_handle()),
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
        cancel_handle: BgCancelHandle::Abort(handle.abort_handle()),
        pid: None,
        output_preview: None,
    };

    registry.register_with_kind(task).unwrap();
    assert_eq!(registry.active_count(), 1);

    // 取消任务：应 abort JoinHandle 并从 registry 移除
    registry.cancel("bg-running").unwrap();

    // 验证 registry 中已清理
    let tasks = registry.list_tasks();
    assert!(
        tasks.is_empty(),
        "cancel 后任务应从 registry 移除，实际: {}",
        tasks.len()
    );
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
    let mut cmd =
        crate::process::shell_command(&format!("sh -c 'sleep 2; touch {}'", marker_path), &[]);
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
