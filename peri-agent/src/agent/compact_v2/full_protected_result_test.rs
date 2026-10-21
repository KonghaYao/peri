//! Full preserves outstanding mailbox obligations and archives only covered results.

use crate::session::test_resources::mock::MockSessionResources;

use async_trait::async_trait;
use peri_model::{
    Model, ModelCapabilities, ModelError, ModelMessage, ModelRequest, ModelResponse, ModelResult,
    ModelStream, StopReason,
};
use tokio_util::sync::CancellationToken;

use super::*;
use crate::agent::compact_v2::config::CompactConfig;
use crate::messages::{BaseMessage, MessageContent};
use crate::session::transcript::MessageTranscript;
use crate::thread::ThreadMeta;

fn make_human(text: &str) -> BaseMessage {
    BaseMessage::human(MessageContent::text(text.to_string()))
}

fn make_ai(text: &str) -> BaseMessage {
    BaseMessage::ai(MessageContent::text(text.to_string()))
}

struct FullLifecycleModel;

struct CapturingFullModel {
    requests: std::sync::Mutex<Vec<ModelRequest>>,
}

#[async_trait]
impl Model for CapturingFullModel {
    fn capabilities(&self) -> ModelCapabilities {
        FullLifecycleModel.capabilities()
    }

    async fn stream(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> ModelResult<ModelStream> {
        Err(ModelError::cancelled())
    }

    async fn complete(
        &self,
        request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> ModelResult<ModelResponse> {
        self.requests.lock().unwrap().push(request);
        Ok(ModelResponse::new(
            ModelMessage::assistant_text("<summary>captured</summary>"),
            StopReason::EndTurn,
            None,
            None,
        )?)
    }
}

#[async_trait]
impl Model for FullLifecycleModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_tools: false,
            supports_reasoning: false,
            supports_vision: false,
            supports_streaming: true,
        }
    }

    async fn stream(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> ModelResult<ModelStream> {
        // compact 路径只走 complete()，stream() 不应被调用
        Err(ModelError::cancelled())
    }

    async fn complete(
        &self,
        _request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> ModelResult<ModelResponse> {
        Ok(ModelResponse::new(
            ModelMessage::assistant_text("<summary>FULL_SUMMARY_MARKER</summary>"),
            StopReason::EndTurn,
            None,
            None,
        )?)
    }
}

// ─── 后台终态通知的 Full 保护边界（T02/T03/T04/T13）──────────────────────────────

fn terminal_reminder(
    task_id: &str,
    body: &str,
) -> peri_acp_types::system_reminder::TrustedSystemReminder {
    use peri_acp_types::system_reminder::{
        ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
        ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
    };
    TrustedSystemReminderFactory::for_producer()
        .construct(SystemReminder {
            version: SYSTEM_REMINDER_VERSION,
            category: ReminderCategory::Task,
            source: ReminderSource("shell".into()),
            kind: "terminal".into(),
            severity: ReminderSeverity::Info,
            delivery: ReminderDelivery::Configurable,
            audiences: ReminderAudiences(vec![ReminderAudience::Model]),
            body: body.into(),
            summary: None,
            metadata: serde_json::json!({ "task_id": task_id }),
        })
        .unwrap()
}

fn ready_mailbox(
    id: crate::messages::MessageId,
    reminder: &peri_acp_types::system_reminder::TrustedSystemReminder,
) -> crate::session::async_result_mailbox::AsyncResultMailbox {
    use peri_acp_types::session::MessageSource;
    let mailbox = crate::session::async_result_mailbox::AsyncResultMailbox::new("s", "t");
    mailbox
        .accept(id, reminder, MessageSource::ShellComplete)
        .unwrap();
    mailbox
        .mark_canonical_ready(id, reminder, MessageSource::ShellComplete, Some(1))
        .unwrap();
    mailbox
}

async fn protected_history(
    reminder: &peri_acp_types::system_reminder::TrustedSystemReminder,
    id: crate::messages::MessageId,
) -> (tempfile::TempDir, MessageTranscript) {
    let dir = tempfile::tempdir().unwrap();
    let store = MockSessionResources::new();
    let thread_id = store
        .create_thread(ThreadMeta::new_at("/tmp", peri_time::now_wall()))
        .await
        .unwrap();
    let mut transcript = MessageTranscript::new().with_persistence(store, thread_id);
    transcript.append(make_human("summarize the session"));
    transcript.append(make_ai("previous answer"));
    assert!(transcript.mirror_committed_reminder(id, reminder.clone()));
    (dir, transcript)
}

fn protected_view(
    transcript: &MessageTranscript,
    protected: &crate::session::async_result_mailbox::ProtectedResults,
) -> Vec<BaseMessage> {
    crate::agent::compact_v2::projection::render_persisted_llm_view_with_protected(
        transcript,
        &crate::agent::model_bridge::AgentModelBridge::projection_capabilities(&FullLifecycleModel),
        protected,
    )
    .unwrap()
}

/// [T02] Receive 后立即 Full：摘要模型看不到终态事实（不承担消费确认），
/// canonical 不被改写，最终主模型视图在摘要之后仍保留完整事实。
#[tokio::test]
async fn async_result_full_compact_protects_uncovered_success_fact() {
    let marker = "T02_TERMINAL_SUCCESS_FACT exit_code: 0";
    let task_id = "shell-T02-full";
    let reminder = terminal_reminder(task_id, marker);
    let id = crate::messages::MessageId::new();
    let mailbox = ready_mailbox(id, &reminder);
    let (dir, mut transcript) = protected_history(&reminder, id).await;

    let model = CapturingFullModel {
        requests: std::sync::Mutex::new(Vec::new()),
    };
    let result = full_compact_inner_with_protected(
        &mut transcript,
        Some(&model),
        &CompactConfig::default(),
        dir.path().to_str().unwrap(),
        &mailbox.protected_snapshot(),
    )
    .await
    .unwrap();

    assert_eq!(
        result.outcome,
        crate::agent::compact_v2::CompactOutcome::FullApplied
    );
    let requests = model.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(
        !serde_json::to_string(&requests[0])
            .unwrap()
            .contains(marker),
        "摘要请求不得包含未覆盖终态事实：摘要模型不承担消费确认"
    );

    assert!(
        transcript.get(id).is_some(),
        "受保护提醒必须留在 canonical transcript"
    );
    assert!(
        !transcript.flags(id).excluded,
        "受保护提醒不得被写成 excluded 后用重追加补救"
    );

    let view = protected_view(&transcript, &mailbox.protected_snapshot());
    let summary_at = view
        .iter()
        .position(|message| message.content().contains("captured"))
        .unwrap_or_else(|| {
            panic!(
                "摘要消息必须存在：{:?}",
                view.iter()
                    .map(|message| (message.id(), message.content().to_string()))
                    .collect::<Vec<_>>()
            )
        });
    let fact_at = view
        .iter()
        .position(|message| message.content().contains(marker))
        .expect("未覆盖终态事实必须重新投影到最终请求");
    assert!(
        fact_at > summary_at,
        "投影顺序必须是压缩后的历史/摘要，随后是未处理结果事实"
    );
    assert_eq!(
        mailbox
            .protected_snapshot()
            .items
            .iter()
            .filter(|item| item.delivery_id == id)
            .count(),
        1,
        "同一 ID 只投影一次"
    );
}

/// [T03] 失败 shell 后立即 Full：失败类型与 exit_code 原样保留，不能投影为成功。
#[tokio::test]
async fn async_result_full_compact_preserves_failed_exit_code() {
    let marker = "T03_TERMINAL_FAILURE state: failed exit_code: 7";
    let task_id = "shell-T03-full";
    let reminder = terminal_reminder(task_id, marker);
    let id = crate::messages::MessageId::new();
    let mailbox = ready_mailbox(id, &reminder);
    let (dir, mut transcript) = protected_history(&reminder, id).await;

    let model = CapturingFullModel {
        requests: std::sync::Mutex::new(Vec::new()),
    };
    full_compact_inner_with_protected(
        &mut transcript,
        Some(&model),
        &CompactConfig::default(),
        dir.path().to_str().unwrap(),
        &mailbox.protected_snapshot(),
    )
    .await
    .unwrap();

    assert!(
        !serde_json::to_string(&model.requests.lock().unwrap()[0])
            .unwrap()
            .contains(marker),
        "失败事实也不得交给摘要模型归档"
    );
    let view = protected_view(&transcript, &mailbox.protected_snapshot());
    let projected = view
        .iter()
        .find(|message| message.content().contains(task_id))
        .expect("失败终态必须保留");
    assert!(projected.content().contains("state: failed"));
    assert!(projected.content().contains("exit_code: 7"));
}

/// [T04] Covered 后提醒恢复普通历史压缩资格：不存在永久 pin。
#[tokio::test]
async fn async_result_full_compact_archives_covered_result() {
    use peri_acp_types::session::MessageSource;
    let marker = "T04_COVERED_FACT";
    let task_id = "shell-T04-full";
    let reminder = terminal_reminder(task_id, marker);
    let id = crate::messages::MessageId::new();
    let mailbox = ready_mailbox(id, &reminder);
    let receipt = mailbox
        .capture_request("attempt-t04", "request-t04", &[id])
        .expect("请求必须绑定 receipt");
    mailbox.cover_after_commit(&receipt, None).unwrap();
    assert!(
        mailbox.protected_snapshot().is_empty(),
        "Covered 结果不再属于保护集合"
    );
    assert_eq!(
        mailbox.accept(id, &reminder, MessageSource::ShellComplete),
        Ok(crate::session::async_result_mailbox::AcceptOutcome::Duplicate)
    );

    let (dir, mut transcript) = protected_history(&reminder, id).await;
    let result = full_compact_inner_with_protected(
        &mut transcript,
        Some(&FullLifecycleModel),
        &CompactConfig::default(),
        dir.path().to_str().unwrap(),
        &mailbox.protected_snapshot(),
    )
    .await
    .unwrap();

    assert_eq!(
        result.affected_count, 3,
        "human + ai + 已覆盖提醒都属于可归档历史"
    );
    assert!(
        transcript.get(id).is_none(),
        "已覆盖提醒的正文引用必须随 Full 释放"
    );
    assert!(!protected_view(&transcript, &mailbox.protected_snapshot())
        .iter()
        .any(|message| message.content().contains(marker)));
}

/// [T13] 只剩 System 与受保护结果：明确 no-op，不调用摘要模型、不反复 Full。
#[tokio::test]
async fn async_result_full_compact_noop_when_only_protected_facts_remain() {
    let marker = "T13_PROTECTED_ONLY";
    let reminder = terminal_reminder("shell-T13", marker);
    let id = crate::messages::MessageId::new();
    let mailbox = ready_mailbox(id, &reminder);

    let mut transcript = MessageTranscript::new();
    transcript.append(BaseMessage::system("system prompt"));
    transcript.mirror_committed_reminder(id, reminder);

    let model = CapturingFullModel {
        requests: std::sync::Mutex::new(Vec::new()),
    };
    let result = full_compact_inner_with_protected(
        &mut transcript,
        Some(&model),
        &CompactConfig::default(),
        "/tmp",
        &mailbox.protected_snapshot(),
    )
    .await
    .unwrap();

    assert_eq!(
        result.outcome,
        crate::agent::compact_v2::CompactOutcome::Skipped
    );
    assert!(result.summary.is_none());
    assert!(
        model.requests.lock().unwrap().is_empty(),
        "没有可归档历史时不得生成空摘要"
    );
    assert_eq!(
        mailbox.protected_snapshot().items.len(),
        1,
        "处理责任必须保留"
    );
    let view = protected_view(&transcript, &mailbox.protected_snapshot());
    assert!(
        view.iter()
            .any(|message| message.content().contains(marker)),
        "no-op 后下一次请求仍必须包含受保护事实"
    );
}

/// [T10] 手动 `/compact`（force）与自动压力路径共用同一保护规则：
/// 未覆盖终态事实既不进摘要，也不被归档；派生视图仍带完整事实。
#[tokio::test]
async fn async_result_manual_and_auto_full_share_protection() {
    use crate::agent::compact_v2::{run_compact_with_protected, ContextPressure};

    let marker = "T10_PROTECTED_FACT state: succeeded exit_code: 0";
    let reminder = terminal_reminder("shell-T10", marker);
    let id = crate::messages::MessageId::new();
    let mailbox = ready_mailbox(id, &reminder);
    let pressure = ContextPressure {
        estimated_tokens: 96_000,
        context_window: 100_000,
        output_reserve: 4_000,
        predicted_tool_growth: 0,
        safety_buffer: 5_000,
        cache_hit_rate: 0.0,
    };

    for force in [true, false] {
        let (dir, mut transcript) = protected_history(&reminder, id).await;
        let model = CapturingFullModel {
            requests: std::sync::Mutex::new(Vec::new()),
        };
        let mut failures = 0;
        let result = run_compact_with_protected(
            &mut transcript,
            Some(&model),
            &CompactConfig::default(),
            &pressure,
            force,
            &mut failures,
            dir.path().to_str().unwrap(),
            None,
            &mailbox.protected_snapshot(),
        )
        .await;

        assert_eq!(
            result.outcome,
            crate::agent::compact_v2::CompactOutcome::FullApplied,
            "force={force}"
        );
        let requests = model.requests.lock().unwrap();
        assert!(
            !serde_json::to_string(&requests[0])
                .unwrap()
                .contains(marker),
            "force={force}: 手动与自动入口都不得把未覆盖事实交给摘要模型"
        );
        assert!(
            !transcript.flags(id).excluded,
            "force={force}: 保护集合不能被入口差异绕过"
        );
        assert!(protected_view(&transcript, &mailbox.protected_snapshot())
            .iter()
            .any(|message| message.content().contains(marker)));
    }
}
