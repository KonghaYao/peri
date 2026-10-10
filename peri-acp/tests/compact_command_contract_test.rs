//! 手动 compact 的 canonical 历史与取消终态回归（真实门面与命令注册表）。
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use peri_acp_types::{
    command::{CommandContext, DependencyBag, FeedbackLevel},
    event::{EventSink, ExecutorEvent},
    messages::BaseMessage,
    session_resources::{FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionResources},
    store::PersistedPayload,
    workspace::SessionBinding,
};
use peri_agent::session::exec::compact_pipeline::execute_compact;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct AuditSink(Mutex<Vec<String>>, Mutex<Vec<String>>);

#[async_trait]
impl EventSink for AuditSink {
    async fn push_event(&self, _: &str, event: &ExecutorEvent, _: u32) {
        self.0
            .lock()
            .unwrap()
            .push(serde_json::to_string(event).unwrap());
    }
    async fn push_done(&self, _: &str, reason: &str, _: Option<&str>) {
        self.1.lock().unwrap().push(reason.to_owned());
    }
}

/// [回归测试] 手动 compact 预取消曾返回 Cancelled，却发出正常完成的 done。
#[tokio::test]
async fn test_cancelled_compact_done_agrees_with_prompt_result() {
    use peri_acp_types::command::PromptStopReason;
    use peri_acp_types::messages::MessageContent;
    use peri_agent::session::exec::executor_helpers::{
        intercept_immediate_command, InterceptOutcome, InterceptRequest,
    };
    let registry = Arc::new(peri_acp::session::command::CommandRegistry::new());
    peri_acp::session::command::register_builtins(&registry);
    let history = vec![BaseMessage::human("preserve me")];
    let ids: Vec<_> = history.iter().map(BaseMessage::id).collect();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let sink = Arc::new(AuditSink::default());
    let event_sink: Arc<dyn EventSink> = sink.clone();
    let task_manager: Arc<dyn peri_acp_types::tasks::TaskManager> =
        Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    let content = MessageContent::text("/compact");
    let result = intercept_immediate_command(InterceptRequest {
        mcp_pool: None,
        content: &content,
        history: &history,
        history_payloads: history
            .iter()
            .cloned()
            .map(PersistedPayload::Message)
            .collect(),
        cwd: "/tmp",
        session_id: "audit-cancel",
        cancel: &cancel,
        session_resources: None,
        thread_id: None,
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
        frozen_system_prompt: None,
        event_sink: &event_sink,
        auxiliary_model: &None,
        task_manager: &task_manager,
        command_lookup: Arc::new(move |text| registry.resolve(text)),
        compact_config_loader: Arc::new(Default::default),
    })
    .await;
    let InterceptOutcome::Handled(result) = result else {
        panic!("真实注册表应拦截 compact");
    };
    assert_eq!(result.stop_reason, PromptStopReason::Cancelled);
    assert!(result.failure.is_none());
    assert_eq!(
        result
            .persisted_payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>(),
        ids
    );
    assert_eq!(
        sink.1.lock().unwrap().as_slice(),
        ["cancelled"],
        "取消后的 done 必须与 PromptResult 的 Cancelled 一致"
    );
}

struct SummaryModel;

#[async_trait]
impl peri_model::Model for SummaryModel {
    fn capabilities(&self) -> peri_model::ModelCapabilities {
        peri_model::ModelCapabilities {
            supports_tools: false,
            supports_reasoning: false,
            supports_vision: false,
            supports_streaming: true,
        }
    }
    async fn stream(
        &self,
        _: peri_model::ModelRequest,
        _: CancellationToken,
    ) -> peri_model::ModelResult<peri_model::ModelStream> {
        Err(peri_model::ModelError::cancelled())
    }
    async fn complete(
        &self,
        _: peri_model::ModelRequest,
        _: CancellationToken,
    ) -> peri_model::ModelResult<peri_model::ModelResponse> {
        peri_model::ModelResponse::new(
            peri_model::ModelMessage::assistant_text("<summary>AUDIT_SUMMARY</summary>"),
            peri_model::StopReason::EndTurn,
            None,
            None,
        )
    }
}

/// [回归测试] Host 的 canonical 历史曾导致第二次 compact 被可见视图校验拒绝。
#[tokio::test]
async fn test_compact_again_using_host_canonical_history() {
    let directory = tempfile::tempdir().unwrap();
    let resources: Arc<dyn SessionResources> = Arc::new(
        peri_resources::sessions::SessionResourcesImpl::open(directory.path().join("audit.db"))
            .await
            .unwrap(),
    );
    let workspace = resources.resolve_workspace(directory.path()).await.unwrap();
    let thread_id = uuid::Uuid::now_v7().to_string();
    resources
        .create_session(&NewSession {
            thread_id: thread_id.clone(),
            created_at: "2026-09-27T00:00:00Z".into(),
            meta: NewSessionMeta {
                title: Some("audit".into()),
                cwd: workspace.cwd.to_string_lossy().into_owned(),
                parent_thread_id: None,
                hidden: false,
                cancel_policy: Default::default(),
                snapshot_at_message_id: None,
            },
            binding: SessionBinding::from_workspace(&workspace),
            frozen: FrozenSnapshotBytes::new("{\"version\":1,\"audit\":true}"),
        })
        .await
        .unwrap();
    let initial = vec![BaseMessage::human("question"), BaseMessage::ai("answer")];
    resources
        .append_history(
            &thread_id,
            &initial
                .iter()
                .cloned()
                .map(PersistedPayload::Message)
                .collect::<Vec<_>>(),
        )
        .await
        .unwrap();
    let sink = Arc::new(AuditSink::default());
    let context = |history: Vec<BaseMessage>| {
        let mut ctx = CommandContext::new(
            "audit".into(),
            history,
            workspace.cwd.to_string_lossy().into_owned(),
            sink.clone(),
            CancellationToken::new(),
            DependencyBag::new(),
        );
        ctx.auxiliary_model = Some(Arc::new(SummaryModel));
        ctx.session_resources = Some(resources.clone());
        ctx.thread_id = Some(thread_id.clone());
        ctx
    };
    let first = execute_compact(context(initial)).await;
    assert_eq!(first.feedback.as_ref().unwrap().level, FeedbackLevel::Info);
    let snapshot = resources.load_session_snapshot(&thread_id).await.unwrap();
    assert_eq!(snapshot.payloads.len(), 3);
    assert_eq!(
        snapshot
            .flags
            .values()
            .filter(|flags| flags.excluded)
            .count(),
        2
    );
    // Host finish_prompt_turn 保存 canonical payload；下一轮 handle_prompt 仅筛选 Message，
    // 不按 excluded 筛选。这是输入边界复现，不声称启动了真实 TUI/transport。
    let next_history = snapshot
        .payloads
        .iter()
        .filter_map(|payload| payload.as_message().cloned())
        .collect();
    let second = execute_compact(context(next_history)).await;
    let after = resources.load_session_snapshot(&thread_id).await.unwrap();
    let completions = sink
        .0
        .lock()
        .unwrap()
        .iter()
        .filter(|event| event.contains("compact_completed"))
        .count();
    assert_eq!(
        second.feedback.as_ref().unwrap().level,
        FeedbackLevel::Info,
        "第二次 compact 应成功；实际反馈={:?}，stop={:?}，消息数={}，完成事件数={}",
        second.feedback,
        second.stop_reason,
        after.payloads.len(),
        completions,
    );
    assert_eq!(completions, 2, "每次提交都应发布一次完成事件");
    assert_eq!(after.payloads.len(), 4, "只追加两次摘要，不重复原文");
    assert_eq!(second.messages.len(), 1, "只返回本次可见摘要");
    for original in &snapshot.payloads {
        assert!(after.flags[&original.id()].excluded);
        assert_eq!(
            after
                .payloads
                .iter()
                .filter(|item| item.id() == original.id())
                .count(),
            1,
            "完整原文和前次摘要各保留一份"
        );
    }
}
