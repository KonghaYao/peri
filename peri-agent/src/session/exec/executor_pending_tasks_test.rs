// Responsibility-specific cases share the original test namespace and fixtures.

// [回归测试] §7.3 摘要计数与 loop 的 busy 判据同源：无任务/无 manager 记 0，
// 未结算任务按 active_count 计入（终态结算后不再计入）。
#[test]
fn test_pending_task_count_reflects_unsettled_tasks() {
    use crate::session::exec::executor_helpers;
    use peri_acp_types::tasks::{BgTaskKind, BgTaskRegistration, TaskManager as TaskManagerPort};
    let none: Option<Arc<dyn TaskManagerPort>> = None;
    assert_eq!(executor_helpers::pending_task_count(none.as_ref()), 0);
    let manager: Arc<dyn TaskManagerPort> = Arc::new(crate::agent::async_tasks::TaskManager::new());
    assert_eq!(executor_helpers::pending_task_count(Some(&manager)), 0);
    let ticket = manager
        .register(BgTaskRegistration {
            task_id: "bg-pending".into(),
            kind: BgTaskKind::Shell,
            summary: "sleep 300".into(),
            pid: None,
            kill: Some(Box::new(|| {})),
        })
        .expect("the register fixture result is required for this assertion");
    assert_eq!(executor_helpers::pending_task_count(Some(&manager)), 1);
    manager.complete(
        &ticket,
        peri_acp_types::event::BackgroundTaskResult {
            task_id: "bg-pending".into(),
            agent_name: "bg-shell".into(),
            prompt_summary: "sleep 300".into(),
            success: true,
            output: "done".into(),
            tool_calls_count: 0,
            duration_ms: 1,
            timed_out: false,
            child_thread_id: None,
            subagent_failure: None,
            shell_output: None,
        },
    );
    assert_eq!(
        executor_helpers::pending_task_count(Some(&manager)),
        0,
        "结算后不得再计入 pending"
    );
}
