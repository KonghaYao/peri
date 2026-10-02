//! Cancellation and dispatch resume behavior.

use super::*;

async fn dispatch_resume_fixture(
    config: SubagentResumeConfig,
    cancel: &CancellationToken,
) -> crate::error::AgentResult<crate::agent::stages::tool_dispatch::DispatchOutcome> {
    use crate::agent::react::{Reasoning, ToolCall};
    use crate::agent::stages::{tool_dispatch::dispatch_tools, StageContext};
    use crate::session::{queue::MessageQueue, transcript::MessageTranscript, turn::TurnContext};
    use crate::tools::{BaseTool, ToolContext};

    // A thin BaseTool adapter enters the real SessionFactory; cancellation is
    // driven by the production dispatch pipeline, not a copied select in the test.
    struct ResumeTool(std::sync::Mutex<Option<SubagentResumeConfig>>);
    #[async_trait::async_trait]
    impl BaseTool for ResumeTool {
        fn name(&self) -> &str {
            "ResumeFixture"
        }
        fn description(&self) -> &str {
            "resume cancellation boundary fixture"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn invoke(
            &self,
            _input: serde_json::Value,
            _ctx: ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            let config = { self.0.lock().unwrap().take() }.expect("one invocation");
            let resumed = SessionFactory::resume_subagent(None, config).await?;
            Ok(resumed.child_thread_id)
        }
    }
    let turn = TurnContext::new(Arc::from("/tmp/work"), Arc::new(cancel.clone()));
    let transcript = Arc::new(parking_lot::RwLock::new(MessageTranscript::new()));
    let ctx = StageContext::new(turn, transcript, MessageQueue::new());
    ctx.runtime.tools.write().insert(
        "ResumeFixture".into(),
        Arc::new(ResumeTool(std::sync::Mutex::new(Some(config)))),
    );
    let reasoning = Reasoning::with_tools(
        "",
        vec![ToolCall::new(
            "resume-call",
            "ResumeFixture",
            serde_json::json!({}),
        )],
    );
    let catalog = ctx
        .runtime
        .tool_catalog
        .pin_working_tools(&ctx.runtime.tools.read())
        .unwrap();
    dispatch_tools(&ctx, &reasoning, &catalog, cancel).await
}

async fn cancel_resume_at_gate(
    config: SubagentResumeConfig,
    cancel: &CancellationToken,
    entered_rx: tokio::sync::oneshot::Receiver<()>,
    store: &MockSessionResources,
    thread_id: &ThreadId,
) {
    let mut dispatch = Box::pin(dispatch_resume_fixture(config, cancel));
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        tokio::select! {
            entered = entered_rx => entered.expect("resume phase gate must be reached"),
            result = &mut dispatch => panic!("dispatch ended before the phase gate: {}", result.is_ok()),
        }
    })
    .await
    .expect("resume must reach the phase gate");
    assert_eq!(
        store.load_meta(thread_id).await.unwrap().agent_status,
        AgentStatus::Active
    );
    cancel.cancel();
    assert!(matches!(
        tokio::time::timeout(std::time::Duration::from_secs(2), dispatch)
            .await
            .unwrap(),
        Err(crate::error::AgentError::Interrupted)
    ));
}

async fn wait_for_resume_status(
    store: &MockSessionResources,
    thread_id: &ThreadId,
    status: AgentStatus,
) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let changed = store.status_changed.notified();
            if store.load_meta(thread_id).await.unwrap().agent_status == status {
                break;
            }
            changed.await;
        }
    })
    .await
    .expect("cancelled resume must finalize persisted status before the thread can resume");
}

#[tokio::test]
async fn test_resume_load_cancelled_by_dispatch_restores_previous_status() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &thread_id, None).await;
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (_release_tx, release_rx) = tokio::sync::oneshot::channel();
    let load_dropped = Arc::new(AtomicBool::new(false));
    *store.load_gate.lock().unwrap() = Some(ResumeLoadGate {
        entered: entered_tx,
        release: release_rx,
        dropped: Arc::clone(&load_dropped),
    });
    let llm = RecordingLLM::new();
    let received = Arc::clone(&llm.received);
    let cancel = CancellationToken::new();
    let config = resume_config_with(
        store.clone(),
        thread_id.clone(),
        Box::new(llm),
        SubagentRunMode::Sync,
        None,
        Some(cancel.clone()),
    );
    cancel_resume_at_gate(config, &cancel, entered_rx, &store, &thread_id).await;
    assert!(
        load_dropped.load(Ordering::SeqCst),
        "real dispatch must drop the pending load future"
    );
    assert!(
        received.read().is_empty(),
        "execution must not start during preparation"
    );
    wait_for_resume_status(&store, &thread_id, AgentStatus::Done).await;
    let resumed =
        SessionFactory::resume_subagent(None, resume_config(store.clone(), thread_id.clone()))
            .await
            .expect("the same thread must remain resumable");
    assert_eq!(resumed.child_thread_id, thread_id);
}

#[tokio::test]
async fn test_resume_active_write_cancelled_by_dispatch_finishes_before_rollback() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &thread_id, None).await;
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let write_dropped = Arc::new(AtomicBool::new(false));
    *store.active_write_gate.lock().unwrap() = Some(ResumeLoadGate {
        entered: entered_tx,
        release: release_rx,
        dropped: Arc::clone(&write_dropped),
    });
    let llm = RecordingLLM::new();
    let received = Arc::clone(&llm.received);
    let cancel = CancellationToken::new();
    let config = resume_config_with(
        store.clone(),
        thread_id.clone(),
        Box::new(llm),
        SubagentRunMode::Sync,
        None,
        Some(cancel.clone()),
    );
    cancel_resume_at_gate(config, &cancel, entered_rx, &store, &thread_id).await;
    assert!(
        !write_dropped.load(Ordering::SeqCst),
        "a committed write must retain its owner until it returns"
    );
    assert!(received.read().is_empty());
    assert_eq!(
        store.load_meta(&thread_id).await.unwrap().agent_status,
        AgentStatus::Active
    );
    release_tx.send(()).unwrap();
    wait_for_resume_status(&store, &thread_id, AgentStatus::Done).await;
    assert!(write_dropped.load(Ordering::SeqCst));
    assert!(
        received.read().is_empty(),
        "cancelled preparation must never execute after the write resumes"
    );
    let statuses: Vec<_> = store
        .statuses()
        .iter()
        .map(|(_, status)| status.clone())
        .collect();
    assert_eq!(
        statuses,
        ["done", "active", "done"],
        "rollback follows completion of the active write exactly once"
    );
    let resumed =
        SessionFactory::resume_subagent(None, resume_config(store.clone(), thread_id.clone()))
            .await
            .unwrap();
    assert_eq!(resumed.child_thread_id, thread_id);
}

#[tokio::test]
async fn test_resume_cancelled_during_assembly_never_starts_execution() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    struct CancelDuringAssembly {
        cancel: CancellationToken,
        assembled: Arc<AtomicBool>,
    }
    impl SubagentChainAssembler for CancelDuringAssembly {
        fn assemble(&self, _ctx: &SubagentChainContext) -> MiddlewareChain {
            // build_subagent_session_v2 has already constructed the Session and
            // attached its persisted history when it invokes this real callback.
            self.assembled.store(true, Ordering::SeqCst);
            self.cancel.cancel();
            MiddlewareChain::new()
        }
    }
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &thread_id, None).await;
    let cancel = CancellationToken::new();
    let assembled = Arc::new(AtomicBool::new(false));
    let starts = Arc::new(AtomicUsize::new(0));
    let llm = RecordingLLM::new();
    let received = Arc::clone(&llm.received);
    let mut config = resume_config_with(
        store.clone(),
        thread_id.clone(),
        Box::new(llm),
        SubagentRunMode::Sync,
        None,
        Some(cancel.clone()),
    );
    config.chain_assembler = Arc::new(CancelDuringAssembly {
        cancel: cancel.clone(),
        assembled: assembled.clone(),
    });
    let starts_hook = starts.clone();
    config.on_subagent_start = Some(Arc::new(move |_, _| {
        starts_hook.fetch_add(1, Ordering::SeqCst);
    }));
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        SessionFactory::resume_subagent(None, config),
    )
    .await
    .unwrap()
    .expect("cancelled sync preparation retains the interrupted result contract");
    assert!(result.interrupted);
    assert_eq!(result.child_thread_id, thread_id);
    assert!(result.task_id.is_none());
    assert_eq!(
        result.session.store().thread_id.as_deref(),
        Some(thread_id.as_str())
    );
    assert!(
        assembled.load(Ordering::SeqCst),
        "the cancellation must happen after constructing the real session"
    );
    assert_eq!(
        starts.load(Ordering::SeqCst),
        0,
        "cancelled preparation must not emit lifecycle Start"
    );
    assert!(received.read().is_empty());
    wait_for_resume_status(&store, &thread_id, AgentStatus::Done).await;
    let resumed =
        SessionFactory::resume_subagent(None, resume_config(store.clone(), thread_id.clone()))
            .await
            .unwrap();
    assert_eq!(resumed.child_thread_id, thread_id);
}

#[tokio::test]
async fn test_resume_running_cancelled_by_dispatch_finalizes_claim() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    struct GatedLLM(std::sync::Mutex<Option<ResumeLoadGate>>);
    #[async_trait::async_trait]
    impl crate::agent::react::ReactLLM for GatedLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            _tools: &[&dyn crate::tools::BaseTool],
            _streaming: Option<crate::agent::react::StreamingContext>,
        ) -> crate::error::AgentResult<crate::agent::react::Reasoning> {
            let gate = { self.0.lock().unwrap().take() }.expect("one LLM call");
            gate.wait().await;
            Ok(crate::agent::react::Reasoning::with_answer(
                "",
                "unreachable",
            ))
        }
        fn model_name(&self) -> String {
            "gated-resume".into()
        }
        fn provider_capabilities(
            &self,
        ) -> crate::agent::compact_v2::projection::ProviderCapabilities {
            crate::agent::compact_v2::projection::ProviderCapabilities::default()
        }
    }
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &thread_id, None).await;
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (_release_tx, release_rx) = tokio::sync::oneshot::channel();
    let llm_dropped = Arc::new(AtomicBool::new(false));
    let starts = Arc::new(AtomicUsize::new(0));
    let stops = Arc::new(AtomicUsize::new(0));
    let cancel = CancellationToken::new();
    let mut config = resume_config_with(
        store.clone(),
        thread_id.clone(),
        Box::new(GatedLLM(std::sync::Mutex::new(Some(ResumeLoadGate {
            entered: entered_tx,
            release: release_rx,
            dropped: llm_dropped.clone(),
        })))),
        SubagentRunMode::Sync,
        None,
        Some(cancel.clone()),
    );
    let starts_hook = starts.clone();
    config.on_subagent_start = Some(Arc::new(move |_, _| {
        starts_hook.fetch_add(1, Ordering::SeqCst);
    }));
    let stops_hook = stops.clone();
    config.on_subagent_stop = Some(Arc::new(move |_, _, _, _| {
        stops_hook.fetch_add(1, Ordering::SeqCst);
    }));
    cancel_resume_at_gate(config, &cancel, entered_rx, &store, &thread_id).await;
    assert!(llm_dropped.load(Ordering::SeqCst));
    assert_eq!(
        starts.load(Ordering::SeqCst),
        1,
        "this cancellation happens after lifecycle Start"
    );
    wait_for_resume_status(&store, &thread_id, AgentStatus::Cancelled).await;
    assert_eq!(
        stops.load(Ordering::SeqCst),
        0,
        "claim cleanup must not invent or duplicate the normal Stop hook"
    );
    let resumed =
        SessionFactory::resume_subagent(None, resume_config(store.clone(), thread_id.clone()))
            .await
            .unwrap();
    assert_eq!(resumed.child_thread_id, thread_id);
}

#[tokio::test]
async fn test_resume_precancelled_background_still_registers_and_completes() {
    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &thread_id, None).await;
    let task_manager = Arc::new(TaskManager::new());
    let token = CancellationToken::new();
    token.cancel();
    let llm = RecordingLLM::new();
    let received = llm.received.clone();
    let (completed_tx, completed_rx) = tokio::sync::oneshot::channel();
    let completed_tx = std::sync::Mutex::new(Some(completed_tx));
    let mut config = resume_config_with(
        store.clone(),
        thread_id.clone(),
        Box::new(llm),
        SubagentRunMode::Background,
        Some(task_manager),
        Some(token),
    );
    config.on_bg_complete = Some(Arc::new(move |result, _kind| {
        if let Some(completed) = completed_tx.lock().unwrap().take() {
            let _ = completed.send(result.clone());
        }
    }));
    let spawned = SessionFactory::resume_subagent(None, config)
        .await
        .expect("background cancellation retains real registration and completion");
    assert_eq!(spawned.child_thread_id, thread_id);
    let task_id = spawned
        .task_id
        .expect("background must return a registered task");
    assert!(
        !spawned.interrupted,
        "background interruption is reported asynchronously"
    );
    let completed = tokio::time::timeout(std::time::Duration::from_secs(2), completed_rx)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(completed.task_id, task_id);
    assert_eq!(
        completed.child_thread_id.as_deref(),
        Some(thread_id.as_str())
    );
    assert!(!completed.success);
    assert!(received.read().is_empty());
    assert_eq!(
        store.load_meta(&thread_id).await.unwrap().agent_status,
        AgentStatus::Cancelled
    );
    let resumed =
        SessionFactory::resume_subagent(None, resume_config(store.clone(), thread_id.clone()))
            .await
            .unwrap();
    assert_eq!(resumed.child_thread_id, thread_id);
}

/// Newly added provenance reads remain inside the existing real dispatch claim.
#[tokio::test]
async fn test_resume_provenance_read_cancelled_by_dispatch_restores_previous_status() {
    for inherited_read in [true, false] {
        use std::sync::atomic::{AtomicBool, Ordering};
        let store = MockSessionResources::new();
        let thread_id = uuid::Uuid::now_v7().to_string();
        preset_resumable_thread(&store, &thread_id, None).await;
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (_release_tx, release_rx) = tokio::sync::oneshot::channel();
        let load_dropped = Arc::new(AtomicBool::new(false));
        let gate = if inherited_read {
            &store.inherited_load_gate
        } else {
            &store.flags_load_gate
        };
        *gate.lock().unwrap() = Some(ResumeLoadGate {
            entered: entered_tx,
            release: release_rx,
            dropped: Arc::clone(&load_dropped),
        });
        let llm = RecordingLLM::new();
        let received = Arc::clone(&llm.received);
        let cancel = CancellationToken::new();
        let starts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let start_counter = starts.clone();
        let mut config = resume_config_with(
            store.clone(),
            thread_id.clone(),
            Box::new(llm),
            SubagentRunMode::Sync,
            None,
            Some(cancel.clone()),
        );
        config.on_subagent_start = Some(Arc::new(move |_, _| {
            start_counter.fetch_add(1, Ordering::SeqCst);
        }));
        cancel_resume_at_gate(config, &cancel, entered_rx, &store, &thread_id).await;
        assert!(
            load_dropped.load(Ordering::SeqCst),
            "real dispatch must drop the pending load future"
        );
        assert!(
            received.read().is_empty(),
            "execution must not start during preparation"
        );
        assert_eq!(starts.load(Ordering::SeqCst), 0);
        wait_for_resume_status(&store, &thread_id, AgentStatus::Done).await;
        let resumed =
            SessionFactory::resume_subagent(None, resume_config(store.clone(), thread_id.clone()))
                .await
                .expect("the same thread must remain resumable");
        assert_eq!(resumed.child_thread_id, thread_id);
    }
}

/// Invalid inherited/own overlap must reject reconstruction without stranding Active.
#[tokio::test]
async fn test_resume_provenance_overlap_rolls_back_claim_before_retry() {
    use peri_acp_types::store::{InheritedContext, PersistedPayload};
    use std::sync::atomic::{AtomicUsize, Ordering};

    let store = MockSessionResources::new();
    let thread_id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(&store, &thread_id, None).await;
    let message = BaseMessage::human("same id must not belong to both regions");
    store
        .append_messages(&thread_id, std::slice::from_ref(&message))
        .await
        .unwrap();
    store
        .store_inherited_context(
            &thread_id,
            &InheritedContext {
                payloads: vec![PersistedPayload::Message(message)],
                flags: Default::default(),
            },
        )
        .await
        .unwrap();
    let starts = Arc::new(AtomicUsize::new(0));
    let counter = starts.clone();
    let mut config = resume_config(store.clone(), thread_id.clone());
    config.on_subagent_start = Some(Arc::new(move |_, _| {
        counter.fetch_add(1, Ordering::SeqCst);
    }));
    let error = resume_err(None, config).await;
    assert!(
        error.contains("inherited context overlaps child own history"),
        "{error}"
    );
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    assert_eq!(
        store.load_meta(&thread_id).await.unwrap().agent_status,
        AgentStatus::Done
    );
    assert_eq!(
        store
            .statuses()
            .iter()
            .map(|(_, status)| status.as_str())
            .collect::<Vec<_>>(),
        ["done", "active", "done"]
    );
    // Repair the corrupt fixture and exercise the same thread's real resume.
    store.clear_inherited();
    let resumed =
        SessionFactory::resume_subagent(None, resume_config(store.clone(), thread_id.clone()))
            .await
            .unwrap();
    assert_eq!(resumed.child_thread_id, thread_id);
}
