//! Full Compact 软/硬记忆两段消息的回归。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use peri_model::{
    Model, ModelCapabilities, ModelError, ModelMessage, ModelRequest, ModelResponse, ModelResult,
    ModelStream, StopReason,
};
use tokio_util::sync::CancellationToken;

use crate::agent::compact_v2::config::CompactConfig;
use crate::agent::compact_v2::hard_memory::{
    HardMemoryGoal, HardMemorySnapshot, HardMemorySource, HardMemoryTask, HARD_MEMORY_HEADER,
};
use crate::messages::BaseMessage;
use crate::session::transcript::MessageTranscript;

#[derive(Default)]
struct OrderLog(Mutex<Vec<&'static str>>);

struct RecordingModel {
    log: Arc<OrderLog>,
    requests: Mutex<Vec<String>>,
}

#[async_trait]
impl Model for RecordingModel {
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
        Err(ModelError::cancelled())
    }

    async fn complete(
        &self,
        request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> ModelResult<ModelResponse> {
        self.log.0.lock().unwrap().push("summary");
        self.requests
            .lock()
            .unwrap()
            .push(serde_json::to_string(&request).unwrap_or_default());
        Ok(ModelResponse::try_new(
            ModelMessage::assistant_text("<summary>soft body</summary>"),
            StopReason::EndTurn,
            None,
            None,
        )?)
    }
}

struct FixedSource {
    log: Arc<OrderLog>,
    snapshot: HardMemorySnapshot,
}

impl HardMemorySource for FixedSource {
    fn snapshot(&self) -> HardMemorySnapshot {
        self.log.0.lock().unwrap().push("hard_memory");
        self.snapshot.clone()
    }
}

async fn history() -> (
    crate::session::test_resources::TestSession,
    MessageTranscript,
) {
    let session = crate::session::test_resources::TestSession::open().await;
    let mut transcript =
        MessageTranscript::new().with_persistence(session.resources(), session.thread_id.clone());
    transcript.append(BaseMessage::human("summarize this session"));
    transcript.append(BaseMessage::ai("previous answer"));
    (session, transcript)
}

fn shell_task() -> HardMemoryTask {
    HardMemoryTask {
        kind: peri_acp_types::tasks::BgTaskKind::Shell,
        task_id: "shell-runtime-01".into(),
        child_thread_id: None,
        summary: "npm run dev".into(),
        status: "running".into(),
        started_at: "2026-11-11T00:00:00Z".into(),
    }
}

#[tokio::test]
async fn full_compact_appends_soft_then_hard_memory_with_runtime_ids() {
    let log = Arc::new(OrderLog::default());
    let model = RecordingModel {
        log: Arc::clone(&log),
        requests: Mutex::new(Vec::new()),
    };
    let source = FixedSource {
        log: Arc::clone(&log),
        snapshot: HardMemorySnapshot {
            tasks: vec![shell_task()],
            goal: Some(HardMemoryGoal {
                goal_id: Some("goal-runtime-01".into()),
                objective: Some("finish hard memory".into()),
                status: peri_acp_types::goal::GoalStatus::Active,
                token_budget: None,
                tokens_used: 12,
                blocked_reason: None,
            }),
        },
    };
    let (_session, mut transcript) = history().await;

    let result = crate::agent::compact_v2::full::full_compact_inner_with_hard_memory(
        &mut transcript,
        Some(&model),
        &CompactConfig::default(),
        "/tmp",
        &crate::session::async_result_mailbox::ProtectedResults::default(),
        Some(&source),
    )
    .await
    .expect("full compact must apply");

    assert!(
        result
            .summary
            .as_deref()
            .is_some_and(|summary| summary.contains("soft body")),
        "CompactResult.summary 只承载软记忆"
    );
    assert_eq!(
        log.0.lock().unwrap().as_slice(),
        ["summary", "hard_memory"],
        "硬记忆必须在软摘要生成之后读取"
    );
    let requests = model.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(
        !requests[0].contains(HARD_MEMORY_HEADER),
        "摘要请求不得包含硬记忆（它属于摘要之后的消息段）"
    );

    let visible = transcript.visible_messages();
    assert_eq!(visible.len(), 2, "软摘要与硬记忆是两条消息");
    let summary = visible[0].content();
    let hard = visible[1].content();
    assert!(
        peri_acp_types::compact::is_continuation_hint(&summary),
        "第一条必须是带续接标记的软摘要"
    );
    assert!(summary.contains("soft body"));
    assert!(hard.starts_with(HARD_MEMORY_HEADER));
    assert!(hard.contains("shell-runtime-01"), "必须使用真实运行时 id");
    assert!(hard.contains("goal-runtime-01"));
    assert_ne!(visible[0].id(), visible[1].id());
}

#[tokio::test]
async fn second_compaction_keeps_prior_hard_memory_out_of_summary_request() {
    let log = Arc::new(OrderLog::default());
    let model = RecordingModel {
        log,
        requests: Mutex::new(Vec::new()),
    };
    let source = FixedSource {
        log: Arc::new(OrderLog::default()),
        snapshot: HardMemorySnapshot {
            tasks: vec![shell_task()],
            goal: None,
        },
    };
    let (_session, mut transcript) = history().await;

    for _ in 0..2 {
        let _ = crate::agent::compact_v2::full::full_compact_inner_with_hard_memory(
            &mut transcript,
            Some(&model),
            &CompactConfig::default(),
            "/tmp",
            &crate::session::async_result_mailbox::ProtectedResults::default(),
            Some(&source),
        )
        .await
        .expect("full compact must apply");
    }

    let requests = model.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        !requests[1].contains(HARD_MEMORY_HEADER),
        "第二次 Full 的摘要请求不得包含上一次的硬记忆"
    );
    assert!(
        !requests[1].contains("shell-runtime-01"),
        "旧运行时 id 不得进入摘要输入"
    );

    let visible = transcript.visible_messages();
    assert_eq!(visible.len(), 2, "旧摘要与硬记忆被归档，新两段重新生成");
    assert!(peri_acp_types::compact::is_continuation_hint(
        &visible[0].content()
    ));
    assert!(visible[1].content().starts_with(HARD_MEMORY_HEADER));
    assert!(visible[1].content().contains("shell-runtime-01"));
}

#[tokio::test]
async fn empty_runtime_keeps_single_summary_message() {
    let log = Arc::new(OrderLog::default());
    let model = RecordingModel {
        log: Arc::clone(&log),
        requests: Mutex::new(Vec::new()),
    };
    let source = FixedSource {
        log,
        snapshot: HardMemorySnapshot::default(),
    };
    let (_session, mut transcript) = history().await;

    let _ = crate::agent::compact_v2::full::full_compact_inner_with_hard_memory(
        &mut transcript,
        Some(&model),
        &CompactConfig::default(),
        "/tmp",
        &crate::session::async_result_mailbox::ProtectedResults::default(),
        Some(&source),
    )
    .await
    .expect("full compact must apply");

    let visible = transcript.visible_messages();
    assert_eq!(visible.len(), 1, "无任务时不得追加硬记忆消息");
    assert!(!visible[0].content().contains(HARD_MEMORY_HEADER));
    assert!(peri_acp_types::compact::is_continuation_hint(
        &visible[0].content()
    ));
}
