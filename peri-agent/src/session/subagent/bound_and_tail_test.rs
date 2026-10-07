//! Bound subagent identity and background tail behavior.

use super::*;
use peri_acp_types::session_resources::{work::*, SessionResources};

#[derive(Clone, Default)]
struct LogBuffer(Arc<parking_lot::Mutex<Vec<u8>>>);

impl std::io::Write for LogBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn capture_logs() -> (LogBuffer, tracing::subscriber::DefaultGuard) {
    let logs = LogBuffer::default();
    let writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .without_time()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    (logs, tracing::subscriber::set_default(subscriber))
}

fn matching_log(logs: &LogBuffer, message: &str) -> String {
    let text = String::from_utf8(logs.0.lock().clone()).unwrap();
    let lines: Vec<_> = text.lines().filter(|line| line.contains(message)).collect();
    assert_eq!(lines.len(), 1, "日志必须恰好出现一次：{text}");
    // --nocapture 时输出实际捕获行，便于核验字段；不安装全局 subscriber。
    std::io::Write::write_all(&mut std::io::stdout(), format!("{}\n", lines[0]).as_bytes())
        .unwrap();
    lines[0].to_owned()
}

async fn wait_execution_idle(manager: &TaskManager) {
    use peri_acp_types::tasks::TaskManager as _;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !manager.is_execution_idle() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("后台执行必须确认停止，不能遗留 owned 不确定性");
}

#[tokio::test(flavor = "current_thread")]
async fn background_subagent_panic_settles_and_logs() {
    use peri_acp_types::tasks::TaskManager as _;
    for with_callback in [false, true] {
        let (logs, _capture) = capture_logs();
        let manager = Arc::new(TaskManager::new());
        let mut config = tail_spawn_config(MockSessionResources::new(), TailOutcome::Completed);
        config.run_mode = SubagentRunMode::Background;
        config.task_manager = Some(manager.clone());
        config.on_subagent_start = Some(Arc::new(|_, _| panic!("后台执行测试 panic")));
        let results = Arc::new(parking_lot::Mutex::new(Vec::new()));
        if with_callback {
            let results = results.clone();
            config.on_bg_complete = Some(Arc::new(move |result, _| {
                results.lock().push(result.clone());
                Ok(())
            }));
        }
        let spawned = AdmittedSessionFactory::spawn_subagent(None, config)
            .await
            .unwrap();
        let task_id = spawned.task_id.unwrap();
        wait_execution_idle(&manager).await;
        assert_eq!(manager.active_count(), 0);
        assert_eq!(manager.snapshot().tasks[0].status, "failed");
        if with_callback {
            let results = results.lock();
            assert_eq!(results.len(), 1);
            assert!(!results[0].success);
            assert_eq!(results[0].task_id, task_id);
            assert_eq!(
                results[0].child_thread_id.as_deref(),
                Some(spawned.child_thread_id.as_str())
            );
        }
        let line = matching_log(&logs, "background subagent execution panicked");
        assert!(line.contains("ERROR"));
        assert!(line.contains(&format!("task_id={task_id}")));
        assert!(line.contains(&format!("thread_id={}", spawned.child_thread_id)));
        assert!(!String::from_utf8(logs.0.lock().clone())
            .unwrap()
            .contains("scope still uncertain"));
        manager.begin_session_close();
        assert!(manager.wait_session_close().await);
        assert!(manager.session_close_settled());
        assert!(manager.is_execution_idle());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn subagent_terminal_write_failure_logs_and_continues() {
    for background in [false, true] {
        let (logs, _capture) = capture_logs();
        let store = MockSessionResources::new();
        let manager = Arc::new(TaskManager::new());
        let mut config = tail_spawn_config(store.clone(), TailOutcome::Completed);
        let stopped_store = store.clone();
        config.on_subagent_stop = Some(Arc::new(move |_, _, _, _| {
            // 只在最后的状态 patch 前注入失败，不干扰执行与持久化结算。
            stopped_store.restrict_to_history_read_only();
        }));
        let delivered = Arc::new(parking_lot::Mutex::new(Vec::new()));
        if background {
            config.run_mode = SubagentRunMode::Background;
            config.task_manager = Some(manager.clone());
            let delivered = delivered.clone();
            config.on_bg_complete = Some(Arc::new(move |result, _| {
                delivered.lock().push(result.success);
                Ok(())
            }));
        }
        let spawned = AdmittedSessionFactory::spawn_subagent(None, config)
            .await
            .expect("终态状态 patch 失败不能中断原流程");
        if background {
            wait_execution_idle(&manager).await;
            assert_eq!(*delivered.lock(), vec![true]);
            assert_eq!(manager.snapshot().tasks[0].status, "completed");
        }
        let line = matching_log(&logs, "subagent terminal status write failed");
        assert!(line.contains("WARN"));
        assert!(line.contains(&format!("thread_id={}", spawned.child_thread_id)));
        let error = peri_acp_types::session_resources::SessionResourceError::new(
            peri_acp_types::session_resources::SessionResourceErrorKind::Unsupported,
        );
        assert!(line.contains(&format!("error={error}")));
    }
}

async fn unsettled_execution(store: &MockSessionResources, child_id: &str) -> WorkSnapshot {
    let snapshot = store
        .load_session_work(&WorkQuery {
            session_id: child_id.into(),
            limit: 1,
        })
        .await
        .unwrap();
    assert_eq!(snapshot.state.admissions.len(), 1);
    let admission = snapshot.state.admissions.values().next().unwrap();
    assert!(admission.entering_receipt.is_some());
    assert!(admission.settled_receipt.is_none());
    assert!(snapshot.state.terminal_acknowledgements.is_empty());
    assert!(store
        .load_meta(&child_id.to_owned())
        .await
        .unwrap()
        .agent_status
        .is_active());
    snapshot
}

#[tokio::test]
async fn test_bound_subagent_resume_requires_same_root_but_allows_siblings() {
    // 真门面：绑定、父子链都由真实实现提供（不可用 mock 自证）。
    let repo = crate::session::test_resources::git_repository();
    let db = tempfile::tempdir().unwrap();
    let store: Arc<dyn peri_acp_types::session_resources::SessionResources> = Arc::new(
        peri_resources::sessions::SessionResourcesImpl::open(db.path().join("sessions.db"))
            .await
            .unwrap(),
    );
    let workspace = store.resolve_workspace(repo.path()).await.unwrap();
    let cwd = workspace.cwd.to_str().unwrap();
    let root_a_id = create_bound_root(&store, &workspace, None).await;
    let root_b_id = create_bound_root(&store, &workspace, None).await;
    // child 的 frozen 必须是 root 已保存快照的逐字节副本（门面在 save_child 内校验）。
    let frozen = FrozenSnapshotBytes::new("{\"version\":1,\"root\":true}");
    let child_id = save_bound_child(&store, &workspace, &root_a_id, &frozen).await;
    let sibling_id = save_bound_child(&store, &workspace, &root_a_id, &frozen).await;
    let caller_b = Session::new(
        Arc::from(cwd),
        FrozenContext::builder().build(),
        Some(root_b_id),
    );
    let mut config = resume_config(MockSessionResources::new(), child_id.clone());
    config.session_resources = Arc::clone(&store);
    let error = resume_err(Some(&caller_b), config).await;
    assert!(error.contains("another root session"), "{error}");
    assert_eq!(
        store
            .load_session_meta(&child_id)
            .await
            .unwrap()
            .agent_status,
        AgentStatus::Done
    );
    let sibling = Session::new(
        Arc::from(cwd),
        FrozenContext::builder().build(),
        Some(sibling_id),
    );
    let mut config = resume_config(MockSessionResources::new(), child_id.clone());
    config.session_resources = Arc::clone(&store);
    AdmittedSessionFactory::resume_subagent(Some(&sibling), config)
        .await
        .expect("same-root siblings can resume");
    assert_eq!(
        store
            .load_session_meta(&child_id)
            .await
            .unwrap()
            .agent_status,
        AgentStatus::Done
    );
}

/// 构造已绑定父会话的后台子 agent 配置。
fn tail_spawn_config(
    store: Arc<MockSessionResources>,
    outcome: TailOutcome,
) -> SubagentSpawnConfig {
    // child 落库继承已绑定父会话的 binding 与 frozen，夹具显式建立这些事实。
    store.register_bound_session("tail-parent", "/tmp/tail-fixture");
    SubagentSpawnConfig {
        agent_name: "tail-agent".into(),
        prompt: "task".into(),
        parent_messages: Vec::new(),
        cancel_policy: SubagentCancelPolicy::Independent,
        max_iterations: 10,
        fork_directive_kind: None,
        run_mode: SubagentRunMode::Sync,
        skill_names: Vec::new(),
        llm: crate::session::test_resources::mock::model::fixture_source(
            std::sync::Arc::new(TailChunkLLM(outcome)),
            "fixture-scripted",
        ),
        chain_assembler: Arc::new(EmptyChainAssembler),
        tools: Vec::new(),
        tool_filter: Arc::new(|_| true),
        system_prompt: None,
        tool_invocation_resolver: None,
        compact_config: None,
        context_budget: None,
        compact_llm: None,
        session_resources: Some(store),
        event_handler: None,
        bg_event_sender: None,
        task_manager: None,
        on_bg_complete: None,
        langfuse_bridge: None,
        on_subagent_start: None,
        on_subagent_stop: None,
        register_runtime: None,
        deregister_runtime: None,
        parent_agent_id: Some(AgentId::new()),
        parent_invocation_id: None,
        cancel_token: None,
        cwd: Some("/tmp/tail-fixture".into()),
        parent_thread_id: Some("tail-parent".into()),
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
        frozen_date: None,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn subagent_start_uses_model_call_identity_for_spawn_and_resume() {
    use crate::agent::events::{ExecutorEvent, FnEventHandler};
    for background in [false, true] {
        let store = MockSessionResources::new();
        let manager = Arc::new(TaskManager::new());
        let events = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let capture = events.clone();
        let handler = Arc::new(FnEventHandler(move |event| capture.lock().push(event)));
        let (event_sender, mut event_receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut config = tail_spawn_config(store.clone(), TailOutcome::Completed);
        config.parent_invocation_id = Some("durable-spawn-invocation".into());
        config.event_handler = Some(handler.clone());
        config.bg_event_sender = Some(event_sender.clone());
        config.run_mode = if background {
            SubagentRunMode::Background
        } else {
            SubagentRunMode::Sync
        };
        config.task_manager = background.then(|| manager.clone());
        let spawned = AdmittedSessionFactory::spawn_subagent(None, config)
            .await
            .unwrap();
        if background {
            wait_execution_idle(&manager).await;
        }
        let parent = Session::new(
            Arc::from("/tmp/tail-fixture"),
            FrozenContext::builder().build(),
            Some("tail-parent".into()),
        );
        let mut config = resume_config(store.clone(), spawned.child_thread_id.clone());
        config.parent_invocation_id = Some("durable-resume-invocation".into());
        config.event_handler = Some(handler);
        config.bg_event_sender = Some(event_sender);
        config.run_mode = if background {
            SubagentRunMode::Background
        } else {
            SubagentRunMode::Sync
        };
        config.task_manager = background.then(|| manager.clone());
        AdmittedSessionFactory::resume_subagent(Some(&parent), config)
            .await
            .unwrap();
        if background {
            wait_execution_idle(&manager).await;
        }
        while let Ok(event) = event_receiver.try_recv() {
            events.lock().push(event);
        }
        let starts: Vec<_> = events
            .lock()
            .iter()
            .filter_map(|event| match event {
                ExecutorEvent::SubagentStarted {
                    parent_tool_call_id,
                    instance_id,
                    is_background,
                    ..
                } => Some((
                    parent_tool_call_id.clone(),
                    instance_id.clone(),
                    *is_background,
                )),
                _ => None,
            })
            .collect();
        assert_eq!(
            starts,
            vec![
                (
                    Some("model-call:durable-spawn-invocation".into()),
                    spawned.child_thread_id.clone(),
                    background
                ),
                (
                    Some("model-call:durable-resume-invocation".into()),
                    spawned.child_thread_id,
                    background
                ),
            ]
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn subagent_spawn_rejects_unresolvable_identity_before_creating_child() {
    let store = MockSessionResources::new();
    let mut config = tail_spawn_config(store.clone(), TailOutcome::Completed);
    config.parent_invocation_id = Some("missing-invocation".into());
    let before = store.threads().len();
    let error = match SessionFactory::spawn_subagent(None, config).await {
        Ok(_) => panic!("untrusted invocation identity must not create a child"),
        Err(error) => error,
    };
    assert!(error
        .to_string()
        .contains("delegation invocation unavailable"));
    assert_eq!(store.threads().len(), before);
}

struct TailPanicBridge {
    stops: Arc<std::sync::atomic::AtomicUsize>,
    panic_on_chunk: bool,
    lifecycle_turns: Arc<parking_lot::Mutex<Vec<crate::session::turn::TurnId>>>,
}

impl crate::agent::LangfuseBridgeLike for TailPanicBridge {
    fn process_render_event(&self, event: &crate::agent::events_v2::RenderEvent) {
        if self.panic_on_chunk
            && matches!(
                event,
                crate::agent::events_v2::RenderEvent::TextChunk { .. }
            )
        {
            panic!("tail forwarding fixture panic");
        }
    }
    fn process_observe_event(&self, event: &crate::agent::events_v2::ObserveEvent) {
        if let crate::agent::events_v2::ObserveEvent::SubagentStart { turn_id, .. }
        | crate::agent::events_v2::ObserveEvent::SubagentStop { turn_id, .. } = event
        {
            self.lifecycle_turns.lock().push(*turn_id);
        }
        if matches!(
            event,
            crate::agent::events_v2::ObserveEvent::SubagentStop { .. }
        ) {
            self.stops.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

async fn assert_background_tail_completion(outcome: TailOutcome, panic_forwarder: bool) {
    use crate::agent::events::{BackgroundTaskResult, ExecutorEvent};
    let store = MockSessionResources::new();
    let manager = Arc::new(TaskManager::new());
    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
    let event_rx = Arc::new(parking_lot::Mutex::new(event_rx));
    let (complete_tx, complete_rx) = tokio::sync::oneshot::channel();
    let complete_tx = parking_lot::Mutex::new(Some(complete_tx));
    let callback_rx = event_rx.clone();
    let callback_manager = manager.clone();
    let bridge_stops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let lifecycle_turns = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let mut config = tail_spawn_config(store.clone(), outcome);
    config.run_mode = SubagentRunMode::Background;
    config.task_manager = Some(manager.clone());
    config.bg_event_sender = Some(event_tx);
    config.langfuse_bridge = Some(Arc::new(TailPanicBridge {
        stops: bridge_stops.clone(),
        panic_on_chunk: panic_forwarder,
        lifecycle_turns: lifecycle_turns.clone(),
    }));
    config.on_bg_complete = Some(Arc::new(move |result: &BackgroundTaskResult, _| {
        let mut events = Vec::new();
        while let Ok(event) = callback_rx.lock().try_recv() {
            events.push(event);
        }
        complete_tx
            .lock()
            .take()
            .unwrap()
            .send((result.clone(), events, callback_manager.active_count()))
            .unwrap();
        Ok(())
    }));
    let spawned = AdmittedSessionFactory::spawn_subagent(None, config)
        .await
        .expect("后台注册成功");
    if panic_forwarder {
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), complete_rx)
                .await
                .unwrap()
                .is_err()
        );
        assert_eq!(manager.active_count(), 1);
        assert!(matches!(
            manager.list_tasks()[0].1,
            BackgroundTaskStatus::Running
        ));
        assert_eq!(bridge_stops.load(std::sync::atomic::Ordering::SeqCst), 0);
        let snapshot = unsettled_execution(&store, &spawned.child_thread_id).await;
        assert_eq!(snapshot.state.terminal_obligations.len(), 1);
        let command = snapshot.state.terminal_obligations.values().next().unwrap();
        let WorkAction::PublishTaskSettlement { delivery, binding } = &command.action else {
            panic!("forwarding failure must retain the exact task terminal publication");
        };
        assert_eq!(binding.owner_task_id, spawned.task_id.unwrap());
        if matches!(outcome, TailOutcome::ModelError) {
            let peri_acp_types::store::PersistedPayload::SystemReminder { reminder, .. } =
                peri_acp_types::store::deserialize_persisted_payload(
                    &delivery.event.content.serialized,
                )
                .unwrap()
            else {
                panic!("terminal responsibility must retain its typed failure reminder");
            };
            let failure: peri_acp_types::error::SafeSubagentFailure =
                serde_json::from_value(reminder.as_reminder().metadata["subagent_failure"].clone())
                    .unwrap();
            assert_eq!(failure.child_thread_id(), spawned.child_thread_id);
            assert_eq!(
                failure.diagnostic().expect("model diagnostic").status(),
                Some(429)
            );
            assert_eq!(
                failure.diagnostic().expect("model diagnostic").provider(),
                Some("fixture")
            );
        }
        let parent = store
            .load_session_work(&WorkQuery {
                session_id: "tail-parent".into(),
                limit: 1,
            })
            .await
            .unwrap();
        assert!(parent.state.deliveries.is_empty());
        let mut receiver = event_rx.lock();
        while let Ok(event) = receiver.try_recv() {
            assert!(!matches!(
                event,
                ExecutorEvent::SubagentStopped { .. } | ExecutorEvent::BackgroundTaskCompleted(_)
            ));
        }
        return;
    }
    let (result, events, active_at_callback) =
        tokio::time::timeout(std::time::Duration::from_secs(5), complete_rx)
            .await
            .expect("关闭 producer 后必须完成排空，不能被 guard 持有死锁")
            .unwrap();
    assert_eq!(active_at_callback, 1, "通知先于 TaskManager 终态");
    assert_eq!(manager.active_count(), 0, "真实执行收尾后任务结束");
    let stored = store
        .load_session_work(&WorkQuery {
            session_id: spawned.child_thread_id.clone(),
            limit: 1,
        })
        .await
        .unwrap();
    let sdk_turn_id = stored
        .state
        .admissions
        .values()
        .next()
        .unwrap()
        .admission
        .execution
        .turn_id;
    assert_eq!(
        *lifecycle_turns.lock(),
        vec![sdk_turn_id, sdk_turn_id],
        "Started/Stopped must use the same stored SDK admission turn"
    );
    assert_eq!(
        result.child_thread_id.as_deref(),
        Some(spawned.child_thread_id.as_str())
    );
    let stop_index = events
        .iter()
        .position(|e| matches!(e, ExecutorEvent::SubagentStopped { .. }))
        .expect("必须配对 Stopped");
    let completed_index = events
        .iter()
        .position(|e| matches!(e, ExecutorEvent::BackgroundTaskCompleted(_)));
    let expected_success = matches!(outcome, TailOutcome::Completed) && !panic_forwarder;
    let expected_completed = expected_success || matches!(outcome, TailOutcome::Cancelled);
    if expected_completed {
        assert_eq!(
            completed_index,
            Some(stop_index + 1),
            "成功/协作取消时 Stopped 后紧接 Completed"
        );
    } else {
        assert!(
            completed_index.is_none(),
            "模型/转发错误通过 callback 和 TaskManager 交付，不合成 Completed 事件"
        );
    }
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, ExecutorEvent::SubagentStopped { .. }))
            .count(),
        1
    );
    assert_eq!(
        completed_index.unwrap_or(stop_index),
        events.len() - 1,
        "callback 已收到所有事件，终态后不能有尾事件"
    );
    assert!(
        event_rx.lock().try_recv().is_err(),
        "callback 后没有滞留事件"
    );
    if !panic_forwarder {
        let chunk_index = events.iter().position(|e| matches!(e, ExecutorEvent::TextChunk { chunk, source_agent_id, .. } if chunk == "tail-chunk" && source_agent_id.as_deref() == Some(spawned.child_thread_id.as_str()))).expect("callback 必须收到末条增量");
        assert!(chunk_index < stop_index, "末条增量必须先于 Stopped");
    }
    assert_eq!(result.success, expected_success);
    assert!(
        matches!(&events[stop_index], ExecutorEvent::SubagentStopped { is_error, .. } if *is_error != expected_success)
    );
    if let Some(completed_index) = completed_index {
        assert!(
            matches!(&events[completed_index], ExecutorEvent::BackgroundTaskCompleted(completed) if completed.success == expected_success)
        );
    }
    let expected_status = match outcome {
        TailOutcome::Cancelled => "cancelled",
        TailOutcome::Completed if !panic_forwarder => "done",
        _ => "error",
    };
    assert_eq!(
        store.statuses().last().map(|(_, status)| status.as_str()),
        Some(expected_status)
    );
    if matches!(outcome, TailOutcome::ModelError) {
        assert!(
            result.subagent_failure.is_some(),
            "模型错误必须保留 typed failure"
        );
        assert!(result.output.contains("429"));
    }
    if panic_forwarder {
        assert_eq!(
            bridge_stops.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "失败的 forwarder 不得提前发布遥测 Stop"
        );
    }
    if panic_forwarder && matches!(outcome, TailOutcome::Completed) {
        assert!(result.output.contains("An internal error occurred"));
    }
}

/// [回归测试] background 曾丢弃 forwarder handle，callback 可在最后增量之前执行。
#[tokio::test(flavor = "current_thread")]
async fn test_spawn_subagent_background_drains_tail_before_completion() {
    assert_background_tail_completion(TailOutcome::Completed, false).await;
}

/// [回归测试] 模型失败也要排空尾事件；错误终态通过 callback 交付，保持 wire 契约。
#[tokio::test(flavor = "current_thread")]
async fn test_spawn_subagent_background_error_drains_tail_before_completion() {
    assert_background_tail_completion(TailOutcome::ModelError, false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn test_spawn_subagent_background_cancel_drains_tail_before_completion() {
    assert_background_tail_completion(TailOutcome::Cancelled, false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn test_spawn_subagent_background_forwarder_panic_is_failure() {
    assert_background_tail_completion(TailOutcome::Completed, true).await;
}

#[tokio::test(flavor = "current_thread")]
async fn test_spawn_subagent_background_forwarder_panic_preserves_model_failure() {
    assert_background_tail_completion(TailOutcome::ModelError, true).await;
}

#[tokio::test(flavor = "current_thread")]
async fn test_spawn_subagent_sync_forwarder_panic_is_failure() {
    use crate::agent::events::{ExecutorEvent, FnEventHandler};
    let store = MockSessionResources::new();
    let events = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let capture = events.clone();
    let bridge_stops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut config = tail_spawn_config(store.clone(), TailOutcome::Completed);
    config.langfuse_bridge = Some(Arc::new(TailPanicBridge {
        stops: bridge_stops.clone(),
        panic_on_chunk: true,
        lifecycle_turns: Arc::new(parking_lot::Mutex::new(Vec::new())),
    }));
    config.event_handler = Some(Arc::new(FnEventHandler(move |event| {
        capture.lock().push(event);
    })));
    let error = match AdmittedSessionFactory::spawn_subagent(None, config).await {
        Ok(_) => panic!("forwarder panic 不得返回成功"),
        Err(error) => error,
    };
    assert!(error
        .to_string()
        .contains("Subagent event forwarding failed"));
    assert!(error.to_string().contains("child_thread_id:"));
    assert_eq!(bridge_stops.load(std::sync::atomic::Ordering::SeqCst), 0);
    let child_id = error
        .downcast_ref::<crate::session::subagent::SubagentFailure>()
        .unwrap()
        .child_thread_id();
    assert_eq!(
        events
            .lock()
            .iter()
            .filter(|event| matches!(event, ExecutorEvent::SubagentStopped { .. }))
            .count(),
        0
    );
    unsettled_execution(&store, child_id).await;
}

struct TerminalPanicBridge;

impl crate::agent::LangfuseBridgeLike for TerminalPanicBridge {
    fn process_render_event(&self, _event: &crate::agent::events_v2::RenderEvent) {}
    fn process_observe_event(&self, event: &crate::agent::events_v2::ObserveEvent) {
        if matches!(
            event,
            crate::agent::events_v2::ObserveEvent::SubagentStop { .. }
        ) {
            panic!("terminal forwarding fixture panic");
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn test_spawn_subagent_terminal_bridge_panic_is_failure() {
    use crate::agent::events::{ExecutorEvent, FnEventHandler};
    let store = MockSessionResources::new();
    let events = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let capture = events.clone();
    let mut config = tail_spawn_config(store.clone(), TailOutcome::Completed);
    config.langfuse_bridge = Some(Arc::new(TerminalPanicBridge));
    config.event_handler = Some(Arc::new(FnEventHandler(move |event| {
        capture.lock().push(event);
    })));
    let error = match AdmittedSessionFactory::spawn_subagent(None, config).await {
        Ok(_) => panic!("terminal bridge panic 不得返回成功"),
        Err(error) => error,
    };
    assert!(error
        .to_string()
        .contains("Subagent terminal event forwarding failed"));
    assert!(error.to_string().contains("child_thread_id:"));
    assert!(!events
        .lock()
        .iter()
        .any(|event| matches!(event, ExecutorEvent::SubagentStopped { .. })));
    let child_id = error
        .downcast_ref::<crate::session::subagent::SubagentFailure>()
        .unwrap()
        .child_thread_id();
    unsettled_execution(&store, child_id).await;
}
