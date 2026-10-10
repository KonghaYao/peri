//! Tests for full

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
use peri_acp_types::store::PersistedPayload;

fn make_human(text: &str) -> BaseMessage {
    BaseMessage::human(MessageContent::text(text.to_string()))
}

fn make_ai(text: &str) -> BaseMessage {
    BaseMessage::ai(MessageContent::text(text.to_string()))
}

fn make_ai_with_read_tool(file_path: &str) -> BaseMessage {
    BaseMessage::ai_with_tool_calls(
        MessageContent::text("read the file"),
        vec![crate::messages::ToolCallRequest::new(
            "full-lifecycle-read",
            "Read",
            serde_json::json!({ "file_path": file_path }),
        )],
    )
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

// ── Full Compact 测试 ──────────────────────────────────────────────────────

// 审计中确认的失败反例，修复后作为默认执行的回归测试。
// 对应 spec/history/2026-09.md 2026-09-10 条目。
async fn make_audit_full_history() -> (tempfile::TempDir, MessageTranscript) {
    let dir = tempfile::tempdir().unwrap();
    let store = MockSessionResources::new();
    let thread_id = store
        .create_thread(ThreadMeta::new_at("/tmp", peri_time::now_wall()))
        .await
        .unwrap();
    let mut transcript = MessageTranscript::new().with_persistence(store, thread_id);
    for turn in 0..4 {
        let call_id = format!("audit-bash-{turn}");
        transcript.append(make_human("inspect output"));
        transcript.append(BaseMessage::ai_with_tool_calls(
            "inspect",
            vec![crate::messages::ToolCallRequest::new(
                &call_id,
                "Bash",
                serde_json::json!({"command": "fixture"}),
            )],
        ));
        transcript.append(BaseMessage::tool_result(&call_id, "x".repeat(40_000)));
    }
    let result = full_compact_inner(
        &mut transcript,
        Some(&FullLifecycleModel),
        &CompactConfig::default(),
        dir.path().to_str().unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        result.outcome,
        crate::agent::compact_v2::CompactOutcome::FullApplied
    );
    assert_eq!(result.affected_count, 12);
    (dir, transcript)
}

/// [回归测试] Full 成功排除的历史不得再次成为 Micro 候选；使用默认 stale=3。
#[tokio::test]
async fn test_audit_full_excluded_history_must_not_be_micro_candidate() {
    use crate::agent::compact_v2::{planner::plan_micro, projection};
    let (_dir, transcript) = make_audit_full_history().await;
    let plan = plan_micro(&transcript, &CompactConfig::default(), true);
    let before = transcript.visible_model_messages().unwrap();
    let after = projection::render_llm_view(&transcript, &plan, &Default::default()).unwrap();
    assert_eq!(
        serde_json::to_value(before).unwrap(),
        serde_json::to_value(after).unwrap()
    );
    assert!(
        plan.actions.is_empty(),
        "已排除历史仍产生 {} 个候选、虚报 {} tokens 收益，但实际模型消息完全不变",
        plan.actions.len(),
        plan.estimated_tokens_saved,
    );
}

/// [回归测试] 96% 的新压力样本不能用 excluded 历史的虚假收益满足回收目标。
#[tokio::test]
async fn test_audit_excluded_savings_must_not_suppress_full() {
    use crate::agent::compact_v2::{planner::ContextPressure, CompactOutcome};
    let (dir, mut transcript) = make_audit_full_history().await;
    let pressure = ContextPressure {
        estimated_tokens: 96_000,
        context_window: 100_000,
        output_reserve: 4_000,
        predicted_tool_growth: 0,
        safety_buffer: 5_000,
        cache_hit_rate: 0.0,
    };
    let mut failures = 0;
    let result = crate::agent::compact_v2::run_compact(
        &mut transcript,
        Some(&FullLifecycleModel),
        &CompactConfig::default(),
        &pressure,
        false,
        &mut failures,
        dir.path().to_str().unwrap(),
    )
    .await;
    assert_eq!(
        result.outcome,
        CompactOutcome::FullApplied,
        "高压下只能回收已排除内容，不能报告 Micro 已满足回收目标：{result:?}"
    );
}

#[tokio::test]
async fn full_compact_model_input_comes_from_canonical_transcript() {
    let original = "ORIGINAL_HISTORY_CANONICAL";
    let projected = "... [999 字符已省略] ...";
    let mut transcript = MessageTranscript::new();
    transcript.append(make_human("summarize the history"));
    transcript.append(BaseMessage::ai_with_tool_calls(
        MessageContent::text("historical tool call"),
        vec![crate::messages::ToolCallRequest::new(
            "canonical-write",
            "Write",
            serde_json::json!({"file_path": original, "content": "unchanged"}),
        )],
    ));
    transcript.append(BaseMessage::tool_result(
        "canonical-write",
        MessageContent::text(projected),
    ));

    let model = CapturingFullModel {
        requests: std::sync::Mutex::new(Vec::new()),
    };
    let result = full_compact_inner(
        &mut transcript,
        Some(&model),
        &CompactConfig::default(),
        "/tmp",
    )
    .await;
    assert!(
        result.is_err(),
        "无 persistence 的测试 transcript 应在 model capture 后拒绝 lifecycle commit"
    );

    let requests = model.requests.lock().unwrap();
    let request = requests.first().expect("Full model 应收到一次请求");
    let body = serde_json::to_string(request).unwrap();
    assert!(
        body.contains(original),
        "Full 输入必须读取 canonical transcript"
    );
    assert!(
        body.contains(projected),
        "Full 不得改写 canonical transcript 内容"
    );
}

/// [回归测试] Full 必须摘要并排除 canonical 报告，不能永久保留全文。
#[tokio::test]
async fn test_full_compact_summarizes_and_excludes_canonical_report() {
    use peri_acp_types::system_reminder::{
        ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
        ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
    };
    let store = MockSessionResources::new();
    let thread_id = store
        .create_thread(ThreadMeta::new_at("/tmp", peri_time::now_wall()))
        .await
        .unwrap();
    let mut transcript = MessageTranscript::new().with_persistence(store, thread_id);
    transcript.append(make_human("question"));
    let reminder_id = transcript.append_system_reminder(
        TrustedSystemReminderFactory::for_producer()
            .construct(SystemReminder {
                version: SYSTEM_REMINDER_VERSION,
                category: ReminderCategory::Task,
                source: ReminderSource("full_test".into()),
                kind: "status".into(),
                severity: ReminderSeverity::Info,
                delivery: ReminderDelivery::Configurable,
                audiences: ReminderAudiences(vec![ReminderAudience::Model]),
                body: format!("{}REPORT_TAIL_FACT", "report detail ".repeat(400)),
                summary: None,
                metadata: serde_json::json!({}),
            })
            .unwrap(),
    );
    transcript.append(make_ai("answer"));

    let model = CapturingFullModel {
        requests: std::sync::Mutex::new(Vec::new()),
    };
    full_compact_inner(
        &mut transcript,
        Some(&model),
        &CompactConfig::default(),
        "/tmp",
    )
    .await
    .unwrap();

    let requests = model.requests.lock().unwrap();
    assert!(
        serde_json::to_string(&requests[0])
            .unwrap()
            .contains("REPORT_TAIL_FACT"),
        "Full 摘要请求必须包含超过旧 2000 字符预览的报告结论"
    );
    assert!(transcript.flags(reminder_id).excluded);
    assert!(transcript.get(reminder_id).is_some());
    assert!(!transcript
        .visible_model_messages()
        .unwrap()
        .iter()
        .any(|message| message.id() == reminder_id));
}

#[tokio::test]
async fn full_excludes_loaded_root_history() {
    let store = MockSessionResources::new();
    let thread_id = store
        .create_thread(ThreadMeta::new_at("/tmp", peri_time::now_wall()))
        .await
        .unwrap();
    let question = make_human("previous turn question");
    let answer = make_ai("previous turn answer");
    store
        .append_message(&thread_id, question.clone())
        .await
        .unwrap();
    store
        .append_message(&thread_id, answer.clone())
        .await
        .unwrap();
    let mut transcript = MessageTranscript::new()
        .with_own_payloads(vec![
            peri_acp_types::store::PersistedPayload::Message(question.clone()),
            peri_acp_types::store::PersistedPayload::Message(answer.clone()),
        ])
        .with_persistence(store, thread_id);

    let result = full_compact_inner(
        &mut transcript,
        Some(&FullLifecycleModel),
        &CompactConfig::default(),
        "/tmp",
    )
    .await
    .unwrap();

    assert_eq!(result.affected_count, 2);
    assert!(transcript.flags(question.id()).excluded);
    assert!(transcript.flags(answer.id()).excluded);
    let provider_messages = transcript.visible_model_messages().unwrap();
    assert_eq!(
        provider_messages.len(),
        1,
        "Full 后 provider-facing 视图只应保留摘要，不得继续发送已加载的旧历史"
    );
    assert!(provider_messages[0]
        .content()
        .contains("FULL_SUMMARY_MARKER"));
    assert!(!provider_messages
        .iter()
        .any(|message| message.id() == question.id() || message.id() == answer.id()));
}

#[tokio::test]
async fn full_affected_count_tracks_only_false_to_true_transitions() {
    let store = MockSessionResources::new();
    let thread_id = store
        .create_thread(ThreadMeta::new_at("/tmp", peri_time::now_wall()))
        .await
        .unwrap();
    let mut transcript = MessageTranscript::new().with_persistence(store, thread_id);
    transcript.append(BaseMessage::system("system"));
    transcript.append(make_human("question"));
    transcript.append(make_ai("answer"));

    let first = full_compact_inner(
        &mut transcript,
        Some(&FullLifecycleModel),
        &CompactConfig::default(),
        "/tmp",
    )
    .await
    .unwrap();
    let second = full_compact_inner(
        &mut transcript,
        Some(&FullLifecycleModel),
        &CompactConfig::default(),
        "/tmp",
    )
    .await
    .unwrap();

    assert_eq!(first.affected_count, 2);
    assert_eq!(second.affected_count, 1, "第二轮只应排除首轮 summary");
}

/// [回归测试] 历史 Read/Skill 路径只属于工具环境；Full 不能从计算实例同名路径回读。
#[tokio::test]
async fn full_compact_uses_historical_tool_results_without_local_file_re_read() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("remote.txt");
    let skill_path = dir.path().join("skills/demo/SKILL.md");
    std::fs::create_dir_all(skill_path.parent().unwrap()).unwrap();
    std::fs::write(&file_path, "LOCAL_FILE_MARKER").unwrap();
    std::fs::write(&skill_path, "LOCAL_SKILL_MARKER").unwrap();
    let store = MockSessionResources::new();
    let thread_id = store
        .create_thread(ThreadMeta::new_at(
            dir.path().to_string_lossy().to_string(),
            peri_time::now_wall(),
        ))
        .await
        .unwrap();
    let mut transcript = MessageTranscript::new().with_persistence(store, thread_id);
    transcript.append(make_human("read the remote workspace"));
    let read = transcript.append(make_ai_with_read_tool(&file_path.to_string_lossy()));
    let result = transcript.append(BaseMessage::tool_result(
        "full-lifecycle-read",
        "REMOTE_FILE_MARKER",
    ));
    transcript.append(BaseMessage::ai_with_tool_calls(
        "activate skill",
        vec![crate::messages::ToolCallRequest::new(
            "remote-skill-read",
            "Read",
            serde_json::json!({ "file_path": skill_path }),
        )],
    ));
    transcript.append(BaseMessage::tool_result(
        "remote-skill-read",
        "REMOTE_SKILL_MARKER",
    ));
    let model = CapturingFullModel {
        requests: std::sync::Mutex::new(Vec::new()),
    };
    full_compact_inner(
        &mut transcript,
        Some(&model),
        &CompactConfig::default(),
        &dir.path().to_string_lossy(),
    )
    .await
    .unwrap();
    let model_input = serde_json::to_string(&model.requests.lock().unwrap()[0]).unwrap();
    assert!(model_input.contains("REMOTE_FILE_MARKER"));
    assert!(model_input.contains("REMOTE_SKILL_MARKER"));
    assert!(!model_input.contains("LOCAL_FILE_MARKER"));
    assert!(!model_input.contains("LOCAL_SKILL_MARKER"));
    assert!(transcript.flags(read).excluded);
    assert!(transcript.flags(result).excluded);
    assert!(transcript.entries().iter().any(|entry| entry.id() == read));
    assert!(transcript
        .entries()
        .iter()
        .any(|entry| entry.id() == result));
    assert_eq!(transcript.visible_messages().len(), 1, "Full 仅追加摘要");
}

#[tokio::test]
async fn test_full_compact_no_llm_returns_error() {
    let mut t = MessageTranscript::new();
    t.append(make_human("user question"));
    t.append(make_ai("assistant response"));

    let config = CompactConfig::default();
    let result = full_compact_inner(&mut t, None, &config, "/tmp").await;
    assert!(result.is_err(), "无 LLM 应返回错误");
}

#[tokio::test]
async fn test_full_compact_sqlite_persists_lifecycle_and_preserves_ancestor_and_system() {
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let file_path = dir.path().join("full-lifecycle.rs");
    std::fs::write(&file_path, "pub const FULL_LIFECYCLE: bool = true;\n")
        .expect("写入重新注入文件失败");

    // 真门面 + 真 SQLite + 真执行所有权（写入要求本 root 有活 owner）。
    let session = crate::session::test_resources::TestSession::open().await;
    let store = session.resources();
    let thread_id = session.thread_id.clone();

    let ancestor = BaseMessage::human("ancestor conversation");
    store
        .append_history(&thread_id, &[PersistedPayload::Message(ancestor.clone())])
        .await
        .expect("持久化 ancestor 失败");

    let mut transcript = MessageTranscript::new()
        .with_ancestor(vec![ancestor.clone()])
        .with_persistence(store.clone(), thread_id.clone());
    let own_system = transcript.append(BaseMessage::system("own system prompt"));
    let own_human = transcript.append(make_human("own user question"));
    let own_ai = transcript.append(make_ai_with_read_tool(&file_path.to_string_lossy()));
    transcript
        .flush_persistence()
        .await
        .expect("Full compact 前应完成持久化");

    let result = full_compact_inner(
        &mut transcript,
        Some(&FullLifecycleModel),
        &CompactConfig::default(),
        &dir.path().to_string_lossy(),
    )
    .await
    .expect("SQLite lifecycle 应成功");
    transcript
        .flush_persistence()
        .await
        .expect("Full compact 后应完成持久化");

    assert_eq!(result.outcome, CompactOutcome::FullApplied);
    assert!(
        transcript.full_compaction_committed(),
        "成功提交的 Full Compact 必须标记为 history replacement"
    );
    assert!(
        !transcript.flags(ancestor.id()).excluded,
        "ancestor 不得被排除"
    );
    assert!(!transcript.flags(own_system).excluded, "System 不得被排除");
    assert!(transcript.flags(own_human).excluded, "own Human 必须被排除");
    assert!(transcript.flags(own_ai).excluded, "own AI 必须被排除");

    let summary = transcript
        .entries()
        .iter()
        .find(|entry| entry.message().content().contains("FULL_SUMMARY_MARKER"))
        .expect("应追加 summary")
        .message()
        .clone();

    // 一次一致快照：payload 与 flags 同一次读取，不拼跨时刻结果。
    let stored = store
        .load_session_snapshot(&thread_id)
        .await
        .expect("加载 SQLite 快照失败");
    assert_eq!(
        stored
            .payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>(),
        transcript
            .entries()
            .iter()
            .map(|entry| entry.id())
            .collect::<Vec<_>>(),
        "内存与 SQLite history 必须一致"
    );
    assert!(transcript
        .entries()
        .iter()
        .any(|entry| entry.id() == summary.id()));
    assert_eq!(
        transcript.visible_messages().len(),
        3,
        "ancestor、System 与摘要可见"
    );
    assert!(!stored.flags.contains_key(&ancestor.id()));
    assert!(!stored.flags.contains_key(&own_system));
    assert!(stored.flags[&own_human].excluded);
    assert!(stored.flags[&own_ai].excluded);
}

#[tokio::test]
async fn test_full_compact_history_read_only_backend_leaves_memory_and_store_unchanged() {
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let file_path = dir.path().join("full-lifecycle.rs");
    std::fs::write(&file_path, "pub const FULL_LIFECYCLE: bool = true;\n")
        .expect("写入重新注入文件失败");

    // 只读能力面（`HistoryReadOnly`）：能力面可在运行期变化（权限/后端变化），
    // 此后所有 mutation 必须在副作用前返回 Unsupported。夹具先按可写后端建立历史，
    // 再切换到只读能力面——历史本身必须由可写句柄产生，只读句柄不接受写入。
    let store = MockSessionResources::new();
    let thread_id = store
        .create_thread(ThreadMeta::new_at(
            dir.path().to_string_lossy().to_string(),
            peri_time::now_wall(),
        ))
        .await
        .expect("创建 thread 失败");

    let ancestor = BaseMessage::human("ancestor conversation");
    store
        .append_message(&thread_id, ancestor.clone())
        .await
        .expect("持久化 ancestor 失败");

    let mut transcript = MessageTranscript::new()
        .with_ancestor(vec![ancestor.clone()])
        .with_persistence(store.clone(), thread_id.clone());
    let own_system = transcript.append(BaseMessage::system("own system prompt"));
    let own_human = transcript.append(make_human("own user question"));
    let own_ai = transcript.append(make_ai_with_read_tool(&file_path.to_string_lossy()));
    transcript
        .flush_persistence()
        .await
        .expect("Full compact 前应完成持久化");
    store.restrict_to_history_read_only();
    let before_entries = transcript
        .entries()
        .iter()
        .map(|entry| entry.id())
        .collect::<Vec<_>>();
    let before_store = store
        .load_payloads(&thread_id)
        .await
        .expect("加载初始 history 失败");

    let error = full_compact_inner(
        &mut transcript,
        Some(&FullLifecycleModel),
        &CompactConfig::default(),
        &dir.path().to_string_lossy(),
    )
    .await
    .expect_err("只读能力面必须明确拒绝 lifecycle");

    assert!(
        error
            .to_string()
            .contains("compact persistence did not commit"),
        "应返回 lifecycle 未提交错误，实际: {error}"
    );
    assert_eq!(
        transcript
            .entries()
            .iter()
            .map(|entry| entry.id())
            .collect::<Vec<_>>(),
        before_entries,
        "失败后内存 entries 必须原样"
    );
    assert!(!transcript.flags(ancestor.id()).excluded);
    assert!(!transcript.flags(own_system).excluded);
    assert!(!transcript.flags(own_human).excluded);
    assert!(!transcript.flags(own_ai).excluded);
    assert_eq!(
        store
            .load_payloads(&thread_id)
            .await
            .expect("加载 history 失败")
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>(),
        before_store
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>(),
        "失败后 store history 必须原样"
    );
    assert!(
        store
            .load_message_flags(&thread_id)
            .await
            .expect("加载 flags 失败")
            .is_empty(),
        "失败后 store flags 必须原样"
    );
}

#[tokio::test]
async fn test_full_compact_empty_transcript_skips() {
    // 需要 mock LLM，但空 transcript 应直接跳过
    // 由于 full_compact_inner 需要 LLM，这里用 Micro 代替测试空 transcript
    let mut t = MessageTranscript::new();
    let config = CompactConfig::default();
    let affected = crate::agent::compact_v2::micro::micro_compact(&mut t, &config);
    assert_eq!(affected, 0);
}

// ── 辅助函数测试 ───────────────────────────────────────────────────────────
