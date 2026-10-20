use super::super::run_sync::run_sync_subagent;
use super::*;
use crate::agent::async_tasks::handoff::ChildSettlementWindow;
use crate::agent::async_tasks::TaskManager;
use crate::agent::events::BackgroundTaskResult;
use crate::agent::react::{ReactLLM, Reasoning, StreamingContext, ToolCall};
use crate::agent::stages::receive::run_receive;
use crate::agent::stages::{run_react_loop, LoopResult, ReceiveInput};
use crate::middleware::chain::MiddlewareChain;
use crate::session::queue::{MessageSource, QueuedMessage};
use crate::session::subagent::{
    agent_id_from_child_thread, build_v2_subagent_context, ChildExecutionPermit,
    ChildHandoffReason, ChildRunOutcome, ParkedChildOwner, SubagentHost, V2SubagentContext,
};
use crate::session::{FrozenContext, MessageTranscript};
use crate::tools::BaseTool;
use peri_acp_types::tasks::{BgTaskKind, BgTaskRegistration, TaskManager as TaskManagerPort};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

fn bind_owner_store(
    child: &Arc<Session>,
    manager: &Arc<TaskManager>,
) -> (
    Arc<ParkedChildOwner>,
    Arc<crate::session::test_resources::mock::MockSessionResources>,
) {
    let id = child.store().thread_id.clone().unwrap();
    let store = crate::session::test_resources::mock::MockSessionResources::new();
    *child.transcript().write() =
        MessageTranscript::new().with_persistence(store.clone(), id.clone());
    let owner = ParkedChildOwner::new(
        id.clone(),
        id,
        manager.clone(),
        child.queue().clone(),
        Some(store.clone()),
        None,
    );
    owner.set_writer(child.transcript().read().persist_tx_handle());
    (owner, store)
}

fn bind_owner(child: &Arc<Session>, manager: &Arc<TaskManager>) -> Arc<ParkedChildOwner> {
    bind_owner_store(child, manager).0
}

fn permit_for(
    child: &Arc<Session>,
    context: &crate::agent::stages::StageContext,
) -> Option<ChildExecutionPermit> {
    child
        .subagent_host()
        .and_then(|host| host.child_owner.clone())
        .map(|owner| {
            owner
                .acquire(context.session.turn.execution_binding())
                .unwrap()
        })
}

fn make_child(manager: Arc<TaskManager>, with_owner: bool) -> Arc<Session> {
    let child_id = uuid::Uuid::now_v7().to_string();
    let child = Session::new(
        Arc::from("/tmp"),
        FrozenContext::builder().build(),
        Some(child_id.clone()),
    );
    let owner = with_owner.then(|| bind_owner(&child, &manager));
    child.set_subagent_host(SubagentHost {
        task_manager: Some(manager),
        child_owner: owner,
        ..Default::default()
    });
    child
}

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
    let manager = Arc::new(TaskManager::new());
    let child = make_child(manager.clone(), true);
    TaskManagerPort::register(
        manager.as_ref(),
        BgTaskRegistration {
            task_id: "shell-task".into(),
            kind: BgTaskKind::Shell,
            summary: "sleep 300".into(),
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
        Some(agent_id_from_child_thread(
            &child.store().thread_id.clone().unwrap(),
        )),
    );
    (child, manager, built, reminder_calls)
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
    let manager = Arc::new(TaskManager::new());
    let child = make_child(manager.clone(), true);
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
    let built = nested_context(&child, initial_calls.clone(), result_calls.clone());
    (child, manager, built, initial_calls, result_calls)
}

fn nested_context(
    child: &Arc<Session>,
    initial_calls: Arc<AtomicUsize>,
    result_calls: Arc<AtomicUsize>,
) -> V2SubagentContext {
    let child_id = child.store().thread_id.clone().unwrap();
    build_v2_subagent_context(
        Some(child.clone()),
        Box::new(NestedResultLlm {
            initial_calls,
            result_calls,
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
    )
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
        timed_out: false,
        child_thread_id: None,
        subagent_failure: None,
        shell_output: None,
    }
}

/// 无未结任务：不启动收尾窗口，正常立即结束。
#[tokio::test(start_paused = true)]
async fn child_without_active_tasks_settles_immediately() {
    let (child, manager, built, initial_calls, result_calls) = child_with_nested_task();
    settle_nested_task(&child, &manager);
    let permit = permit_for(&child, &built.context);
    let outcome = tokio::time::timeout(
        Duration::from_secs(1),
        run_child_until_terminal(built.context, 10, &child),
    )
    .await
    .expect("no active tasks must not trigger a settlement wait");
    if let Some(permit) = permit {
        permit.finish(Ok(()));
    }
    assert!(matches!(outcome, ChildRunOutcome::Settled), "{outcome:?}");
    assert_eq!(initial_calls.load(Ordering::SeqCst), 0);
    assert_eq!(result_calls.load(Ordering::SeqCst), 1);
}

/// 截止前 shell 提醒被消费并结算：正常 Settled，不多发提醒。
#[tokio::test(start_paused = true)]
async fn shell_reminder_settles_before_deadline() {
    let (child, manager, built, reminder_calls) = child_with_shell_task(true);
    let permit = permit_for(&child, &built.context);
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        run_child_until_terminal(built.context, 10, &child),
    )
    .await
    .expect("settling on the reminder must not wait for the deadline");
    if let Some(permit) = permit {
        permit.finish(Ok(()));
    }
    assert!(matches!(result, ChildRunOutcome::Settled), "{result:?}");
    assert_eq!(reminder_calls.load(Ordering::SeqCst), 1);
    assert_eq!(manager.active_count(), 0);
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

/// 忽略提醒但在 D 前完成：Settled，不产生交接记录。
#[tokio::test(start_paused = true)]
async fn ignored_shell_reminder_settles_when_task_finishes_before_deadline() {
    let (child, manager, built, reminder_calls) = child_with_shell_task(false);
    let permit = permit_for(&child, &built.context);
    let owner = tokio::spawn(async move {
        let outcome = run_child_until_terminal(built.context, 10, &child).await;
        if let Some(permit) = permit {
            permit.finish(Ok(()));
        }
        outcome
    });
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(manager.complete("shell-task", nested_result()));
    let result = owner.await.unwrap();
    assert!(matches!(result, ChildRunOutcome::Settled), "{result:?}");
    assert_eq!(reminder_calls.load(Ordering::SeqCst), 1);
}

/// 预算耗尽但任务在 D 前结算：保留 MaxIterations 原因，不附加残留交接。
#[tokio::test(start_paused = true)]
async fn exhausted_budget_settles_without_unprocessable_reminder() {
    let (child, manager, built, reminder_calls) = child_with_shell_task(false);
    let turn = built.context.session.turn.clone();
    let child_for_run = child.clone();
    let permit = permit_for(&child, &built.context);
    let owner = tokio::spawn(async move {
        let outcome = run_child_until_terminal(built.context, 1, &child_for_run).await;
        if let Some(permit) = permit {
            permit.finish(Ok(()));
        }
        outcome
    });
    tokio::time::advance(Duration::from_secs(1)).await;
    settle_task(&manager, &child, "shell-task");
    let outcome = owner.await.unwrap();
    assert!(
        matches!(
            outcome,
            ChildRunOutcome::Failed {
                error: crate::error::AgentError::MaxIterationsExceeded(_),
                residual: None,
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(turn.current_step(), 1);
    assert_eq!(reminder_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn cancelled_child_does_not_emit_shell_reminder() {
    let (child, _manager, built, reminder_calls) = child_with_shell_task(false);
    child.config().cancel_token.cancel();
    let permit = permit_for(&child, &built.context);
    let outcome = run_child_until_terminal(built.context, 10, &child).await;
    if let Some(permit) = permit {
        permit.finish(Ok(()));
    }
    assert!(matches!(outcome, ChildRunOutcome::Interrupted));
    assert_eq!(reminder_calls.load(Ordering::SeqCst), 0);
}

/// 永久 Working 且无通知：D 后交接一次，Shell 保持真实 Running。
#[tokio::test(start_paused = true)]
async fn permanent_shell_hands_off_at_deadline_without_notifications() {
    let (child, manager, built, _) = child_with_shell_task(false);
    let permit = permit_for(&child, &built.context);
    let started = tokio::time::Instant::now();
    let outcome = tokio::time::timeout(
        Duration::from_secs(125),
        run_child_until_terminal(built.context, 10, &child),
    )
    .await
    .expect("deadline must close the settlement wait");
    if let Some(permit) = permit {
        permit.finish(Ok(()));
    }
    let elapsed = started.elapsed();
    let ChildRunOutcome::HandedOff { receipt } = outcome else {
        panic!("permanent shell must hand off: {outcome:?}");
    };
    assert!(elapsed >= Duration::from_secs(120), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(125), "{elapsed:?}");
    assert_eq!(receipt.reason, ChildHandoffReason::SettlementDeadline);
    assert!(receipt
        .tasks
        .iter()
        .any(|task| task.task_id == "shell-task"));
    assert_eq!(manager.active_count(), 1, "交接不得取消或标记 Shell 成功");
    assert!(child.transcript().read().entries().iter().any(|entry| {
        matches!(entry, crate::session::TranscriptEntry::Reminder { reminder, .. }
            if reminder.as_reminder().kind == "child_tasks_handed_off")
    }));
}

/// activity 风暴/任务新增不重置同一个绝对 D。
#[tokio::test(start_paused = true)]
async fn activity_and_new_tasks_do_not_reset_deadline() {
    let (child, manager, built, _) = child_with_shell_task(false);
    let child_for_run = child.clone();
    let permit = permit_for(&child, &built.context);
    let mut owner = tokio::spawn(async move {
        let outcome = run_child_until_terminal(built.context, 10, &child_for_run).await;
        if let Some(permit) = permit {
            permit.finish(Ok(()));
        }
        outcome
    });
    while !child.transcript().read().entries().iter().any(|entry| {
        matches!(entry, crate::session::TranscriptEntry::Reminder { reminder, .. }
            if reminder.as_reminder().kind == "active_background_shell_before_submit")
    }) {
        tokio::task::yield_now().await;
    }
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    // 连续每秒注入 activity/新任务，D 仍按首次收尾起算。
    for index in 0..5 {
        tokio::time::advance(Duration::from_secs(1)).await;
        TaskManagerPort::register(
            manager.as_ref(),
            BgTaskRegistration {
                task_id: format!("shell-storm-{index}"),
                kind: BgTaskKind::Shell,
                summary: "per-second activity".into(),
                pid: None,
                kill: Some(Box::new(|| {})),
            },
        )
        .unwrap();
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_secs(114)).await;
    TaskManagerPort::register(
        manager.as_ref(),
        BgTaskRegistration {
            task_id: "shell-late".into(),
            kind: BgTaskKind::Shell,
            summary: "added at 119s".into(),
            pid: None,
            kill: Some(Box::new(|| {})),
        },
    )
    .unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(5), &mut owner)
        .await
        .expect("activity storm must not move D")
        .unwrap();
    let ChildRunOutcome::HandedOff { receipt } = outcome else {
        panic!("expected handoff: {outcome:?}");
    };
    assert!(receipt
        .tasks
        .iter()
        .any(|task| task.task_id == "shell-late"));
    assert!(receipt
        .tasks
        .iter()
        .any(|task| task.task_id == "shell-storm-4"));
    assert_eq!(manager.active_count(), 7, "所有 Shell 都保持真实状态");
}

/// task 已终态但结果未消费：active=0 不能证明委派完成。
#[tokio::test(start_paused = true)]
async fn terminal_result_without_consumption_still_hands_off() {
    let (child, manager, built, reminder_calls) = child_with_shell_task(false);
    child.queue().push(QueuedMessage::info(
        MessageSource::HookStopIntent,
        crate::messages::BaseMessage::human("stop"),
    ));
    let run_child = child.clone();
    let permit = permit_for(&child, &built.context);
    let mut owner = tokio::spawn(async move {
        let outcome = run_child_until_terminal(built.context, 10, &run_child).await;
        if let Some(permit) = permit {
            permit.finish(Ok(()));
        }
        outcome
    });
    tokio::time::advance(Duration::from_secs(1)).await;
    settle_task(&manager, &child, "shell-task");
    let outcome = tokio::time::timeout(Duration::from_secs(125), &mut owner)
        .await
        .expect("unconsumed result must keep the bounded settlement obligation")
        .unwrap();
    let ChildRunOutcome::HandedOff { receipt } = outcome else {
        panic!("expected handoff: {outcome:?}");
    };
    assert_eq!(receipt.reason, ChildHandoffReason::HookStop);
    assert!(receipt.tasks.is_empty(), "task 已终态，不得列入任务引用");
    assert!(!receipt.message_ids.is_empty(), "未消费结果必须有消息引用");
    assert_eq!(reminder_calls.load(Ordering::SeqCst), 0);
}

fn settle_task(manager: &TaskManager, child: &Session, task_id: &str) {
    let delivery = crate::session::bg_complete::task_bg_complete_callback(
        crate::agent::async_tasks::delivery::SessionTerminalDelivery::for_queue(
            child.queue().clone(),
        ),
    );
    let mut result = nested_result();
    result.task_id = task_id.to_string();
    assert!(manager.settle_completed(task_id, result, delivery).unwrap());
}

/// Receive 期间跨过 D：消息已落库并记录引用，但不再准入推理。
#[tokio::test(start_paused = true)]
async fn receive_across_deadline_preserves_committed_result() {
    let manager = Arc::new(TaskManager::new());
    let child = make_child(manager.clone(), true);
    let (initial_calls, result_calls) =
        (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let mut built = nested_context(&child, initial_calls.clone(), result_calls.clone());
    let window = Arc::new(ChildSettlementWindow::default());
    window.start();
    tokio::time::advance(Duration::from_secs(121)).await;
    built.context.async_ctx.child_settlement = Some(window);
    let message = crate::messages::BaseMessage::human("late prompt");
    let message_id = message.id();
    child
        .queue()
        .push(QueuedMessage::prompt(MessageSource::UserInput, message));
    let received = run_receive(ReceiveInput {
        context: built.context.clone(),
    })
    .await
    .unwrap();
    assert_eq!(received.consumed_count, 1);
    assert!(child.transcript().read().entries().iter().any(|entry| entry
        .as_message()
        .is_some_and(|message| message.id() == message_id)));
    assert_eq!(received.process_message_ids, vec![message_id]);
    assert!(
        built
            .context
            .async_ctx
            .received_after_deadline
            .lock()
            .is_empty(),
        "Receive 只返回身份；登记由 loop 在判定 SettlementDue 时统一完成"
    );
    assert_eq!(initial_calls.load(Ordering::SeqCst), 0);
}

/// D 已在 Receive 前到期：不消费新输入，直接由循环退出原因结束。
#[tokio::test(start_paused = true)]
async fn deadline_before_receive_does_not_consume_new_input() {
    let manager = Arc::new(TaskManager::new());
    let child = make_child(manager.clone(), true);
    let (initial_calls, result_calls) =
        (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let mut built = nested_context(&child, initial_calls.clone(), result_calls.clone());
    let window = Arc::new(ChildSettlementWindow::default());
    window.start();
    tokio::time::advance(Duration::from_secs(121)).await;
    built.context.async_ctx.child_settlement = Some(window);
    let message = crate::messages::BaseMessage::human("must not be consumed");
    let message_id = message.id();
    child
        .queue()
        .push(QueuedMessage::prompt(MessageSource::UserInput, message));
    let outcome = run_react_loop(built.context.clone(), 10).await;
    assert!(matches!(outcome, LoopResult::SettlementDue));
    assert!(child.queue().snapshot().iter().any(|message| message
        .message()
        .is_some_and(|message| message.id() == message_id)));
    assert_eq!(initial_calls.load(Ordering::SeqCst), 0);
}

struct ToolThenExpireLlm {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ReactLLM for ToolThenExpireLlm {
    async fn generate_reasoning(
        &self,
        _messages: &[crate::messages::BaseMessage],
        _tools: &[&dyn BaseTool],
        _streaming: Option<StreamingContext>,
    ) -> crate::error::AgentResult<Reasoning> {
        assert_eq!(self.calls.fetch_add(1, Ordering::SeqCst), 0);
        tokio::time::advance(Duration::from_secs(121)).await;
        Ok(Reasoning::with_tools(
            "call the tool",
            vec![ToolCall::new(
                "call-1",
                "MissingTool",
                serde_json::json!({}),
            )],
        ))
    }
}

/// D 前已准入的 Reason 在 D 后产生 tool-call：完成 Act 结算，不开下一轮。
#[tokio::test(start_paused = true)]
async fn admitted_reason_completes_act_after_deadline() {
    let manager = Arc::new(TaskManager::new());
    let child = make_child(manager.clone(), true);
    child.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("use a tool"),
    ));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut built = build_v2_subagent_context(
        Some(child.clone()),
        Box::new(ToolThenExpireLlm {
            calls: calls.clone(),
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
        Some(agent_id_from_child_thread(
            &child.store().thread_id.clone().unwrap(),
        )),
    );
    let window = Arc::new(ChildSettlementWindow::default());
    window.start();
    built.context.async_ctx.child_settlement = Some(window);
    let outcome = run_react_loop(built.context, 10).await;
    assert!(matches!(outcome, LoopResult::SettlementDue), "{outcome:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "截止后不得发起新模型请求");
    let transcript = child.transcript();
    let entries = transcript.read();
    let has_tool_result = entries
        .visible_messages()
        .iter()
        .any(|message| matches!(message, crate::messages::BaseMessage::Tool { .. }));
    assert!(has_tool_result, "已准入 Act 必须结算 tool result");
}

/// 预算耗尽时仍有未结任务：保留原错误并附残留交接。
#[tokio::test(start_paused = true)]
async fn max_iterations_keeps_reason_and_residual_handoff() {
    let (child, manager, built, _) = child_with_shell_task(false);
    TaskManagerPort::register(
        manager.as_ref(),
        BgTaskRegistration {
            task_id: "shell-permanent".into(),
            kind: BgTaskKind::Shell,
            summary: "permanent".into(),
            pid: None,
            kill: Some(Box::new(|| {})),
        },
    )
    .unwrap();
    let run_child = child.clone();
    let permit = permit_for(&child, &built.context);
    let mut owner = tokio::spawn(async move {
        let outcome = run_child_until_terminal(built.context, 1, &run_child).await;
        if let Some(permit) = permit {
            permit.finish(Ok(()));
        }
        outcome
    });
    tokio::time::advance(Duration::from_secs(1)).await;
    let delivery = crate::session::bg_complete::task_bg_complete_callback(
        crate::agent::async_tasks::delivery::SessionTerminalDelivery::for_queue(
            child.queue().clone(),
        ),
    );
    let mut result = nested_result();
    result.task_id = "shell-task".into();
    assert!(manager
        .settle_completed("shell-task", result, delivery)
        .unwrap());
    let outcome = tokio::time::timeout(Duration::from_secs(5), &mut owner)
        .await
        .expect("budget error must not wait for D")
        .unwrap();
    let ChildRunOutcome::Failed { error, residual } = outcome else {
        panic!("expected typed failure: {outcome:?}");
    };
    assert!(matches!(
        error,
        crate::error::AgentError::MaxIterationsExceeded(_)
    ));
    let receipt = residual.expect("unsettled shell must be handed off");
    assert!(receipt
        .tasks
        .iter()
        .any(|task| task.task_id == "shell-permanent"));
}

/// 截止后的晚结果不自动复活旧 run；显式 resume 读取同一队列并推理。
#[tokio::test(start_paused = true)]
async fn late_result_after_deadline_requires_explicit_resume() {
    let (child, manager, built, initial_calls, result_calls) = child_with_nested_task();
    let child_id = child.store().thread_id.clone().unwrap();
    let first = run_sync_subagent(
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
        child.clone(),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(
        matches!(first, ChildRunOutcome::HandedOff { .. }),
        "{first:?}"
    );
    settle_nested_task(&child, &manager);
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(
        result_calls.load(Ordering::SeqCst),
        0,
        "旧 run 不得自动消费截止后的结果"
    );
    child.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("continue explicitly"),
    ));
    let resumed = nested_context(&child, initial_calls.clone(), result_calls.clone());
    let second = run_sync_subagent(
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
        resumed,
        child.clone(),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(matches!(second, ChildRunOutcome::Settled), "{second:?}");
    assert_eq!(result_calls.load(Ordering::SeqCst), 1);
    assert!(super::super::util::extract_last_ai_text(&child).contains("processed nested result"));
}

/// 无 owner 的嵌入路径在 D 后显式失败，不无限等待。
#[tokio::test(start_paused = true)]
async fn missing_owner_reports_capability_failure() {
    let manager = Arc::new(TaskManager::new());
    let child = make_child(manager.clone(), false);
    TaskManagerPort::register(
        manager.as_ref(),
        BgTaskRegistration {
            task_id: "shell-task".into(),
            kind: BgTaskKind::Shell,
            summary: "permanent".into(),
            pid: None,
            kill: Some(Box::new(|| {})),
        },
    )
    .unwrap();
    child.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("run shell"),
    ));
    let built = build_v2_subagent_context(
        Some(child.clone()),
        Box::new(NestedResultLlm {
            initial_calls: Arc::new(AtomicUsize::new(0)),
            result_calls: Arc::new(AtomicUsize::new(0)),
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
        Some(agent_id_from_child_thread(
            &child.store().thread_id.clone().unwrap(),
        )),
    );
    let outcome = tokio::time::timeout(
        Duration::from_secs(125),
        run_child_until_terminal(built.context, 10, &child),
    )
    .await
    .expect("missing owner must not wait beyond D");
    let ChildRunOutcome::HandoffFailed { failure, .. } = outcome else {
        panic!("expected capability failure: {outcome:?}");
    };
    assert!(failure
        .error
        .to_string()
        .contains("handoff_capability_unavailable"));
}

/// 交接 canonical 写入失败：结构化 HandoffFailed，不返回 HandedOff 或成功。
#[tokio::test(start_paused = true)]
async fn handoff_persistence_failure_is_not_reported_as_handed_off() {
    let manager = Arc::new(TaskManager::new());
    let child_id = uuid::Uuid::now_v7().to_string();
    let child = Session::new(
        Arc::from("/tmp"),
        FrozenContext::builder().build(),
        Some(child_id.clone()),
    );
    let (owner, store) = bind_owner_store(&child, &manager);
    child.set_subagent_host(SubagentHost {
        task_manager: Some(manager.clone()),
        child_owner: Some(owner.clone()),
        ..Default::default()
    });
    store.restrict_to_history_read_only();
    TaskManagerPort::register(
        manager.as_ref(),
        BgTaskRegistration {
            task_id: "shell-task".into(),
            kind: BgTaskKind::Shell,
            summary: "permanent".into(),
            pid: None,
            kill: Some(Box::new(|| {})),
        },
    )
    .unwrap();
    child.queue().push(QueuedMessage::prompt(
        MessageSource::UserInput,
        crate::messages::BaseMessage::human("run shell"),
    ));
    let built = build_v2_subagent_context(
        Some(child.clone()),
        Box::new(NestedResultLlm {
            initial_calls: Arc::new(AtomicUsize::new(0)),
            result_calls: Arc::new(AtomicUsize::new(0)),
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
    let permit = permit_for(&child, &built.context);
    let outcome = tokio::time::timeout(
        Duration::from_secs(125),
        run_child_until_terminal(built.context, 10, &child),
    )
    .await
    .expect("persistence failure must not wait beyond D");
    if let Some(permit) = permit {
        permit.finish(Err("handoff persistence failed".into()));
    }
    let ChildRunOutcome::HandoffFailed { failure, .. } = outcome else {
        panic!("持久化失败不得伪装成交接: {outcome:?}");
    };
    assert!(
        failure.error.to_string().contains("persistence")
            || failure
                .error
                .to_string()
                .contains("handoff_capability_unavailable")
    );
    assert!(
        owner.receipt().is_none(),
        "未提交的交接不得留 committed receipt"
    );
    assert!(owner.pending_receipt().is_some(), "冻结身份必须保留供重试");
    assert_eq!(manager.active_count(), 1);
}

struct FixedLlm(&'static str);

#[async_trait::async_trait]
impl ReactLLM for FixedLlm {
    async fn generate_reasoning(
        &self,
        _messages: &[crate::messages::BaseMessage],
        _tools: &[&dyn BaseTool],
        _streaming: Option<StreamingContext>,
    ) -> crate::error::AgentResult<Reasoning> {
        Ok(Reasoning::with_answer("", self.0))
    }
}

/// 旧执行的 ContinueCurrentRun 控制消息不得驱动新 run。
fn fixed_context(session: &Arc<Session>, answer: &'static str) -> V2SubagentContext {
    let id = session.store().thread_id.clone().unwrap();
    build_v2_subagent_context(
        Some(session.clone()),
        Box::new(FixedLlm(answer)),
        Arc::new(MiddlewareChain::new()),
        Vec::new(),
        Arc::new(|_| true),
        None,
        "/tmp",
        session.config().cancel_token.as_ref().clone(),
        None,
        None,
        None,
        None,
        Some(agent_id_from_child_thread(&id)),
    )
}

fn transcript_contains(session: &Session, needle: &str) -> bool {
    session
        .transcript()
        .read()
        .entries()
        .iter()
        .any(|entry| match entry {
            crate::session::TranscriptEntry::Message(message) => message.content().contains(needle),
            crate::session::TranscriptEntry::Reminder { reminder, .. } => {
                reminder.as_reminder().body.contains(needle)
                    || reminder.as_reminder().metadata.to_string().contains(needle)
            }
        })
}

/// A→B→C：C 的未完成结果归属直接父 B，A 不接收 C 的身份；各层均有界结束。

#[path = "child_runner_background_test.rs"]
mod background;
#[path = "child_runner_deadline_test.rs"]
mod deadline;
#[path = "child_runner_nested_test.rs"]
mod nested;
#[cfg(unix)]
#[path = "child_runner_process_test.rs"]
mod process;
