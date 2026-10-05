//! Bound subagent identity and background tail behavior.

use super::*;

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
    SessionFactory::resume_subagent(Some(&sibling), config)
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
        llm: Box::new(TailChunkLLM(outcome)),
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
        parent_tool_call_id: None,
        cancel_token: None,
        cwd: Some("/tmp/tail-fixture".into()),
        parent_thread_id: Some("tail-parent".into()),
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
        frozen_date: None,
    }
}

struct TailPanicBridge(Arc<std::sync::atomic::AtomicUsize>);

impl crate::agent::LangfuseBridgeLike for TailPanicBridge {
    fn process_render_event(&self, event: &crate::agent::events_v2::RenderEvent) {
        if matches!(
            event,
            crate::agent::events_v2::RenderEvent::TextChunk { .. }
        ) {
            panic!("tail forwarding fixture panic");
        }
    }
    fn process_observe_event(&self, event: &crate::agent::events_v2::ObserveEvent) {
        if matches!(
            event,
            crate::agent::events_v2::ObserveEvent::SubagentStop { .. }
        ) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
    let mut config = tail_spawn_config(store.clone(), outcome);
    config.run_mode = SubagentRunMode::Background;
    config.task_manager = Some(manager.clone());
    config.bg_event_sender = Some(event_tx);
    if panic_forwarder {
        config.langfuse_bridge = Some(Arc::new(TailPanicBridge(bridge_stops.clone())));
    }
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
    let spawned = SessionFactory::spawn_subagent(None, config)
        .await
        .expect("后台注册成功");
    let (result, events, active_at_callback) =
        tokio::time::timeout(std::time::Duration::from_secs(5), complete_rx)
            .await
            .expect("关闭 producer 后必须完成排空，不能被 guard 持有死锁")
            .unwrap();
    assert_eq!(active_at_callback, 1, "通知先于 TaskManager 终态");
    assert_eq!(manager.active_count(), 0, "真实执行收尾后任务结束");
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
    config.langfuse_bridge = Some(Arc::new(TailPanicBridge(bridge_stops.clone())));
    config.event_handler = Some(Arc::new(FnEventHandler(move |event| {
        capture.lock().push(event);
    })));
    let error = match SessionFactory::spawn_subagent(None, config).await {
        Ok(_) => panic!("forwarder panic 不得返回成功"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("An internal error occurred"));
    assert!(error.to_string().contains("child_thread_id:"));
    assert_eq!(bridge_stops.load(std::sync::atomic::Ordering::SeqCst), 0);
    let events = events.lock();
    assert!(matches!(
        events.last(),
        Some(ExecutorEvent::SubagentStopped { is_error: true, .. })
    ));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ExecutorEvent::SubagentStopped { .. }))
            .count(),
        1
    );
    assert_eq!(
        store.statuses().last().map(|(_, status)| status.as_str()),
        Some("error")
    );
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
    let error = match SessionFactory::spawn_subagent(None, config).await {
        Ok(_) => panic!("terminal bridge panic 不得返回成功"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("An internal error occurred"));
    assert!(error.to_string().contains("child_thread_id:"));
    assert!(matches!(
        events.lock().last(),
        Some(ExecutorEvent::SubagentStopped { is_error: true, .. })
    ));
    assert_eq!(
        store.statuses().last().map(|(_, status)| status.as_str()),
        Some("error")
    );
}
