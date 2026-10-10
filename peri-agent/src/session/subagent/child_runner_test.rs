use super::*;
use crate::agent::async_tasks::TaskManager;
use crate::agent::events::BackgroundTaskResult;
use crate::agent::react::{ReactLLM, Reasoning, StreamingContext};
use crate::middleware::chain::MiddlewareChain;
use crate::session::queue::{MessageSource, QueuedMessage};
use crate::session::subagent::{
    agent_id_from_child_thread, build_v2_subagent_context, SubagentHost, V2SubagentContext,
};
use crate::session::FrozenContext;
use crate::tools::BaseTool;
use peri_acp_types::tasks::{BgTaskKind, BgTaskRegistration, TaskManager as TaskManagerPort};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

struct ShellReminderLlm {
    manager: Arc<TaskManager>,
    reminder_calls: Arc<AtomicUsize>,
    settle_on_reminder: bool,
}

#[async_trait::async_trait]
impl ReactLLM for ShellReminderLlm {
    async fn generate_reasoning(
        &self,
        messages: &[crate::messages::BaseMessage],
        _tools: &[&dyn BaseTool],
        _streaming: Option<StreamingContext>,
    ) -> crate::error::AgentResult<Reasoning> {
        if messages.iter().any(|message| {
            message
                .content()
                .contains("active_background_shell_before_submit")
                || message.content().contains("仍持有后台 shell 任务")
        }) {
            self.reminder_calls.fetch_add(1, Ordering::SeqCst);
            if self.settle_on_reminder {
                let mut result = nested_result();
                result.task_id = "shell-task".into();
                assert!(self.manager.complete("shell-task", result));
            }
        }
        Ok(Reasoning::with_answer("", "child answer"))
    }
}

fn child_with_shell_task(
    settle_on_reminder: bool,
) -> (
    Arc<Session>,
    Arc<TaskManager>,
    V2SubagentContext,
    Arc<AtomicUsize>,
) {
    let child_id = uuid::Uuid::now_v7().to_string();
    let child = Session::new(
        Arc::from("/tmp"),
        FrozenContext::builder().build(),
        Some(child_id.clone()),
    );
    let manager = Arc::new(TaskManager::new());
    child.set_subagent_host(SubagentHost {
        task_manager: Some(manager.clone()),
        ..Default::default()
    });
    TaskManagerPort::register(
        manager.as_ref(),
        BgTaskRegistration {
            task_id: "shell-task".into(),
            kind: BgTaskKind::Shell,
            summary: "sleep 180".into(),
            pid: None,
            kill: Some(Box::new(|| {})),
        },
    )
    .unwrap();
    child.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("run shell"),
    ));
    let reminder_calls = Arc::new(AtomicUsize::new(0));
    let built = build_v2_subagent_context(
        Some(child.clone()),
        Box::new(ShellReminderLlm {
            manager: manager.clone(),
            reminder_calls: reminder_calls.clone(),
            settle_on_reminder,
        }),
        Arc::new(MiddlewareChain::new()),
        Vec::new(),
        Arc::new(|_| true),
        None,
        "/tmp",
        child.config().cancel_token.as_ref().clone(),
        None,
        None,
        None,
        None,
        Some(agent_id_from_child_thread(&child_id)),
    );
    (child, manager, built, reminder_calls)
}

#[tokio::test(start_paused = true)]
async fn active_child_shell_reminds_before_bounded_wait_and_can_settle() {
    let (child, manager, built, reminder_calls) = child_with_shell_task(true);
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        run_child_until_terminal(built.context, 10, &child),
    )
    .await
    .expect("shell reminder must bypass the 120-second idle wait");
    assert!(matches!(result, LoopResult::Completed), "{result:?}");
    assert_eq!(reminder_calls.load(Ordering::SeqCst), 1);
    assert_eq!(manager.active_count(), 0);
    assert!(child.transcript().read().entries().iter().any(|entry| {
        matches!(entry, crate::session::TranscriptEntry::Reminder { reminder, .. }
            if reminder.as_reminder().body.contains("shell-task (running)")
                && reminder.as_reminder().body.contains("任务 ID 不是进程 PID"))
    }));
    assert_eq!(
        child
            .transcript()
            .read()
            .entries()
            .iter()
            .filter(|entry| {
                matches!(entry, crate::session::TranscriptEntry::Reminder { reminder, .. }
                    if reminder.as_reminder().kind == "active_background_shell_before_submit")
            })
            .count(),
        1
    );
}

#[tokio::test(start_paused = true)]
async fn ignored_child_shell_reminder_keeps_existing_child_ownership() {
    let (child, manager, built, reminder_calls) = child_with_shell_task(false);
    let owner_child = child.clone();
    let mut owner =
        tokio::spawn(
            async move { run_child_until_terminal(built.context, 10, &owner_child).await },
        );
    assert!(tokio::time::timeout(Duration::from_secs(1), &mut owner)
        .await
        .is_err());
    assert_eq!(reminder_calls.load(Ordering::SeqCst), 1);
    assert_eq!(manager.active_count(), 1);
    let mut result = nested_result();
    result.task_id = "shell-task".into();
    assert!(manager.complete("shell-task", result));
    assert!(matches!(owner.await.unwrap(), LoopResult::Completed));
    assert_eq!(reminder_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn child_shell_with_exhausted_budget_waits_without_unprocessable_reminder() {
    let (child, manager, built, reminder_calls) = child_with_shell_task(false);
    let owner_child = child.clone();
    let mut owner =
        tokio::spawn(async move { run_child_until_terminal(built.context, 1, &owner_child).await });
    assert!(tokio::time::timeout(Duration::from_secs(1), &mut owner)
        .await
        .is_err());
    assert_eq!(reminder_calls.load(Ordering::SeqCst), 0);
    assert_eq!(manager.active_count(), 1);
    let mut result = nested_result();
    result.task_id = "shell-task".into();
    assert!(manager.complete("shell-task", result));
    assert!(matches!(owner.await.unwrap(), LoopResult::Completed));
}

#[tokio::test(start_paused = true)]
async fn cancelled_child_shell_does_not_emit_submit_reminder() {
    let (child, _manager, built, reminder_calls) = child_with_shell_task(false);
    child.config().cancel_token.cancel();
    assert!(matches!(
        run_child_until_terminal(built.context, 10, &child).await,
        LoopResult::Interrupted
    ));
    assert_eq!(reminder_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn hook_stopped_child_keeps_waiting_without_model_reminder() {
    let (child, manager, built, reminder_calls) = child_with_shell_task(false);
    child.queue().push(QueuedMessage::info(
        MessageSource::HookStopIntent,
        crate::messages::BaseMessage::human("stop"),
    ));
    let owner_child = child.clone();
    let mut owner =
        tokio::spawn(
            async move { run_child_until_terminal(built.context, 10, &owner_child).await },
        );
    assert!(tokio::time::timeout(Duration::from_secs(1), &mut owner)
        .await
        .is_err());
    assert_eq!(reminder_calls.load(Ordering::SeqCst), 0);
    assert_eq!(manager.active_count(), 1);
    let mut result = nested_result();
    result.task_id = "shell-task".into();
    assert!(manager.complete("shell-task", result));
    assert!(matches!(owner.await.unwrap(), LoopResult::Completed));
}

struct NestedResultLlm {
    initial_calls: Arc<AtomicUsize>,
    result_calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ReactLLM for NestedResultLlm {
    async fn generate_reasoning(
        &self,
        messages: &[crate::messages::BaseMessage],
        _tools: &[&dyn BaseTool],
        _streaming: Option<StreamingContext>,
    ) -> crate::error::AgentResult<Reasoning> {
        if messages
            .iter()
            .any(|message| message.content().contains("nested-result"))
        {
            self.result_calls.fetch_add(1, Ordering::SeqCst);
            Ok(Reasoning::with_answer("", "processed nested result"))
        } else {
            self.initial_calls.fetch_add(1, Ordering::SeqCst);
            Ok(Reasoning::with_answer("", "waiting for nested result"))
        }
    }
}

fn child_with_nested_task() -> (
    Arc<Session>,
    Arc<TaskManager>,
    V2SubagentContext,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
) {
    let child_id = uuid::Uuid::now_v7().to_string();
    let child = Session::new(
        Arc::from("/tmp"),
        FrozenContext::builder().build(),
        Some(child_id.clone()),
    );
    let manager = Arc::new(TaskManager::new());
    child.set_subagent_host(SubagentHost {
        task_manager: Some(manager.clone()),
        ..Default::default()
    });
    TaskManagerPort::register(
        manager.as_ref(),
        BgTaskRegistration {
            task_id: "nested-task".into(),
            kind: BgTaskKind::Agent,
            summary: "nested work".into(),
            pid: None,
            kill: Some(Box::new(|| {})),
        },
    )
    .unwrap();
    child.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("initial child work"),
    ));
    let initial_calls = Arc::new(AtomicUsize::new(0));
    let result_calls = Arc::new(AtomicUsize::new(0));
    let built = build_v2_subagent_context(
        Some(child.clone()),
        Box::new(NestedResultLlm {
            initial_calls: initial_calls.clone(),
            result_calls: result_calls.clone(),
        }),
        Arc::new(MiddlewareChain::new()),
        Vec::new(),
        Arc::new(|_| true),
        None,
        "/tmp",
        child.config().cancel_token.as_ref().clone(),
        None,
        None,
        None,
        None,
        Some(agent_id_from_child_thread(&child_id)),
    );
    (child, manager, built, initial_calls, result_calls)
}

fn settle_nested_task(child: &Session, manager: &TaskManager) {
    let delivery = crate::session::bg_complete::task_bg_complete_callback(
        crate::agent::async_tasks::delivery::SessionTerminalDelivery::for_queue(
            child.queue().clone(),
        ),
    );
    assert!(manager
        .settle_completed("nested-task", nested_result(), delivery)
        .unwrap());
}

fn nested_result() -> BackgroundTaskResult {
    BackgroundTaskResult {
        task_id: "nested-task".into(),
        agent_name: "nested".into(),
        prompt_summary: "nested work".into(),
        success: true,
        output: "nested-result".into(),
        tool_calls_count: 0,
        duration_ms: 1,
        child_thread_id: None,
        timed_out: false,
        subagent_failure: None,
        shell_output: None,
    }
}

#[tokio::test(start_paused = true)]
async fn child_budget_is_not_reset_after_bounded_idle_handoff() {
    let (child, manager, built, initial_calls, result_calls) = child_with_nested_task();
    let owner_child = child.clone();
    let turn = built.context.session.turn.clone();
    let mut owner =
        tokio::spawn(async move { run_child_until_terminal(built.context, 1, &owner_child).await });
    assert!(tokio::time::timeout(Duration::from_secs(121), &mut owner)
        .await
        .is_err());
    assert_eq!(initial_calls.load(Ordering::SeqCst), 1);
    settle_nested_task(&child, &manager);
    assert!(matches!(
        owner.await.unwrap(),
        LoopResult::Error(crate::error::AgentError::MaxIterationsExceeded(0))
    ));
    assert_eq!(turn.current_step(), 1);
    assert_eq!(initial_calls.load(Ordering::SeqCst), 1);
    assert_eq!(result_calls.load(Ordering::SeqCst), 0);
    assert_eq!(manager.active_count(), 0);
}

#[tokio::test(start_paused = true)]
async fn nested_child_late_result_is_processed_before_delegation_completes() {
    let (child, manager, built, initial_calls, result_calls) = child_with_nested_task();
    let owner_child = child.clone();
    let mut owner =
        tokio::spawn(
            async move { run_child_until_terminal(built.context, 10, &owner_child).await },
        );

    assert!(
        tokio::time::timeout(Duration::from_secs(121), &mut owner)
            .await
            .is_err(),
        "bounded RCRA wait must leave a live child owner"
    );
    assert_eq!(initial_calls.load(Ordering::SeqCst), 1);
    assert_eq!(result_calls.load(Ordering::SeqCst), 0);

    settle_nested_task(&child, &manager);
    assert!(matches!(owner.await.unwrap(), LoopResult::Completed));
    assert_eq!(manager.active_count(), 0);
    assert_eq!(initial_calls.load(Ordering::SeqCst), 1);
    assert_eq!(result_calls.load(Ordering::SeqCst), 1);
    assert!(super::super::util::extract_last_ai_text(&child).contains("processed nested result"));
}

/// [回归测试] child 已经过 bounded wait 且无人输入时，投递暂时失败仍能自动继续。
#[tokio::test(start_paused = true)]
async fn nested_child_recovers_pending_delivery_without_input() {
    let (child, manager, built, initial_calls, result_calls) = child_with_nested_task();
    let owner_child = child.clone();
    let mut owner =
        tokio::spawn(
            async move { run_child_until_terminal(built.context, 10, &owner_child).await },
        );
    assert!(tokio::time::timeout(Duration::from_secs(121), &mut owner)
        .await
        .is_err());
    let delivery = crate::session::bg_complete::task_bg_complete_callback(
        crate::agent::async_tasks::delivery::SessionTerminalDelivery::for_queue(
            child.queue().clone(),
        ),
    );
    let attempts = Arc::new(AtomicUsize::new(0));
    let called = attempts.clone();
    assert!(manager
        .settle_completed(
            "nested-task",
            nested_result(),
            Arc::new(move |result, kind| {
                if called.fetch_add(1, Ordering::SeqCst) == 0 {
                    Err("temporary queue acceptance failure".into())
                } else {
                    delivery(result, kind)
                }
            })
        )
        .is_err());
    assert_eq!(manager.snapshot().tasks[0].status, "delivery_pending");
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), owner)
            .await
            .unwrap()
            .unwrap(),
        LoopResult::Completed
    ));
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(initial_calls.load(Ordering::SeqCst), 1);
    assert_eq!(result_calls.load(Ordering::SeqCst), 1);
    assert_eq!(manager.active_count(), 0);
}

#[tokio::test(start_paused = true)]
async fn nested_child_owner_stops_on_cancel_after_bounded_wait() {
    let (child, _manager, built, initial_calls, result_calls) = child_with_nested_task();
    let cancel = child.config().cancel_token.clone();
    let mut owner =
        tokio::spawn(async move { run_child_until_terminal(built.context, 10, &child).await });

    assert!(tokio::time::timeout(Duration::from_secs(121), &mut owner)
        .await
        .is_err());
    cancel.cancel();
    assert!(matches!(owner.await.unwrap(), LoopResult::Interrupted));
    assert_eq!(initial_calls.load(Ordering::SeqCst), 1);
    assert_eq!(result_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn foreground_child_waits_for_nested_result_before_returning() {
    let (child, manager, built, initial_calls, result_calls) = child_with_nested_task();
    let child_id = child.store().thread_id.clone().unwrap();
    let owner_child = child.clone();
    let mut owner = tokio::spawn(async move {
        super::super::run_sync::run_sync_subagent(
            &child_id,
            "fixture",
            "/tmp",
            10,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            built,
            owner_child,
            None,
        )
        .await
    });

    assert!(tokio::time::timeout(Duration::from_secs(121), &mut owner)
        .await
        .is_err());
    assert_eq!(initial_calls.load(Ordering::SeqCst), 1);
    assert_eq!(result_calls.load(Ordering::SeqCst), 0);
    settle_nested_task(&child, &manager);
    assert!(!owner.await.unwrap().unwrap());
    assert_eq!(result_calls.load(Ordering::SeqCst), 1);
    assert!(super::super::util::extract_last_ai_text(&child).contains("processed nested result"));
}

#[tokio::test(start_paused = true)]
async fn background_child_reports_parent_completion_after_nested_result() {
    let (child, nested_manager, built, initial_calls, result_calls) = child_with_nested_task();
    let parent_manager = Arc::new(TaskManager::new());
    let child_id = child.store().thread_id.clone().unwrap();
    let (completed_tx, mut completed_rx) = tokio::sync::oneshot::channel();
    let completed_tx = parking_lot::Mutex::new(Some(completed_tx));
    let deliveries = Arc::new(AtomicUsize::new(0));
    let delivery_count = deliveries.clone();
    super::super::background::spawn_background_subagent(
        "parent-task".into(),
        child_id,
        "fixture".into(),
        "initial child work".into(),
        "/tmp".into(),
        10,
        None,
        Some(parent_manager.clone()),
        Some(Arc::new(move |result: &BackgroundTaskResult, _| {
            delivery_count.fetch_add(1, Ordering::SeqCst);
            completed_tx
                .lock()
                .take()
                .unwrap()
                .send(result.clone())
                .unwrap();
            Ok(())
        })),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        child.config().cancel_token.as_ref().clone(),
        built,
        None,
    )
    .await
    .unwrap();

    assert!(
        tokio::time::timeout(Duration::from_secs(121), &mut completed_rx)
            .await
            .is_err(),
        "parent must not receive success at the child's bounded wait"
    );
    assert_eq!(parent_manager.active_count(), 1);
    assert_eq!(initial_calls.load(Ordering::SeqCst), 1);
    assert_eq!(result_calls.load(Ordering::SeqCst), 0);
    settle_nested_task(&child, &nested_manager);
    let result = completed_rx.await.unwrap();
    assert!(result.success);
    assert!(result.output.contains("processed nested result"));
    assert_eq!(result_calls.load(Ordering::SeqCst), 1);
    assert_eq!(deliveries.load(Ordering::SeqCst), 1);
}

/// [回归测试] session/cancel-bg-task 的协作取消必须给直接父队列投递一次原身份终态。
#[tokio::test(start_paused = true)]
async fn background_child_cancellation_delivers_original_terminal_to_parent_queue() {
    let (child, nested_manager, built, initial_calls, _result_calls) = child_with_nested_task();
    let parent_manager = Arc::new(TaskManager::new());
    let parent = Session::new(Arc::from("/tmp"), FrozenContext::builder().build(), None);
    let deliveries = Arc::new(AtomicUsize::new(0));
    let called = deliveries.clone();
    let recorded = Arc::new(parking_lot::Mutex::new(None));
    let record = recorded.clone();
    let callback = crate::session::bg_complete::task_bg_complete_callback(
        crate::agent::async_tasks::delivery::SessionTerminalDelivery::for_queue(
            parent.queue().clone(),
        ),
    );
    let callback: peri_acp_types::tasks::OnBgCompleteFn = Arc::new(move |result, kind| {
        called.fetch_add(1, Ordering::SeqCst);
        *record.lock() = Some(result.clone());
        callback(result, kind)
    });
    let mut events = parent_manager.subscribe_events();
    super::super::background::spawn_background_subagent(
        "parent-task".into(),
        child.store().thread_id.clone().unwrap(),
        "fixture".into(),
        "initial child work".into(),
        "/tmp".into(),
        10,
        None,
        Some(parent_manager.clone()),
        Some(callback.clone()),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        child.config().cancel_token.as_ref().clone(),
        built,
        None,
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_secs(121)).await;
    assert_eq!(initial_calls.load(Ordering::SeqCst), 1);
    assert!(nested_manager.complete("nested-task", nested_result()));
    parent_manager.cancel_async("parent-task").await.unwrap();
    assert!(tokio::time::timeout(Duration::from_secs(4), async {
        while deliveries.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .is_ok());
    let result = recorded.lock().clone().unwrap();
    assert_eq!(result.task_id, "parent-task");
    assert_eq!(result.child_thread_id, child.store().thread_id);
    assert!(!result.success);
    assert_eq!(parent.queue().len(), 1);
    assert!(!parent_manager
        .settle_completed("parent-task", result, callback)
        .unwrap());
    assert_eq!(deliveries.load(Ordering::SeqCst), 1);
    assert_eq!(parent_manager.snapshot().tasks[0].status, "cancelled");
    let mut cancelled = 0;
    while let Ok(change) = events.try_recv() {
        match change.event {
            peri_acp_types::tasks::BgRegistryEvent::Cancelled { .. } => cancelled += 1,
            peri_acp_types::tasks::BgRegistryEvent::Completed { .. } => panic!("ghost terminal"),
            _ => {}
        }
    }
    assert_eq!(cancelled, 1);
    assert_eq!(
        TaskManagerPort::shutdown(parent_manager.as_ref()).await,
        peri_acp_types::tasks::TaskShutdownReport::Complete
    );
}
