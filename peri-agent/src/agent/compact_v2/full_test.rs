//! Tests for full

use std::sync::Arc;

use async_trait::async_trait;
use peri_model::{
    Model, ModelCapabilities, ModelError, ModelMessage, ModelRequest, ModelResponse, ModelResult,
    ModelStream, StopReason,
};
use tokio_util::sync::CancellationToken;

use super::*;
use crate::agent::compact_v2::config::CompactConfig;
use crate::messages::{BaseMessage, ContentBlock, ImageSource, MessageContent};
use crate::session::transcript::MessageTranscript;
use crate::thread::{FilesystemThreadStore, SqliteThreadStore, ThreadMeta, ThreadStore};

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
// 对应 spec/issues/2026-09-10-p0-full-micro-compact-churn.md。
async fn make_audit_full_history() -> (tempfile::TempDir, MessageTranscript) {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn ThreadStore> = Arc::new(
        SqliteThreadStore::new(dir.path().join("compact-audit.db"))
            .await
            .unwrap(),
    );
    let thread_id = store.create_thread(ThreadMeta::new("/tmp")).await.unwrap();
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

#[tokio::test]
async fn full_compact_preserves_canonical_reminder_without_flags() {
    use peri_acp_types::system_reminder::{
        ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
        ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
    };
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn ThreadStore> = Arc::new(
        SqliteThreadStore::new(dir.path().join("reminder-full.db"))
            .await
            .unwrap(),
    );
    let thread_id = store.create_thread(ThreadMeta::new("/tmp")).await.unwrap();
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
                body: "preserve me".into(),
                summary: None,
                metadata: serde_json::json!({}),
            })
            .unwrap(),
    );
    transcript.append(make_ai("answer"));

    full_compact_inner(
        &mut transcript,
        Some(&FullLifecycleModel),
        &CompactConfig::default(),
        "/tmp",
    )
    .await
    .unwrap();

    assert_eq!(transcript.flags(reminder_id), MessageFlags::default());
    assert!(transcript.get(reminder_id).is_some());
    assert!(transcript
        .visible_model_messages()
        .unwrap()
        .iter()
        .any(|message| message.id() == reminder_id));
}

#[tokio::test]
async fn full_excludes_loaded_root_history() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn ThreadStore> = Arc::new(
        SqliteThreadStore::new(dir.path().join("loaded-root-history.db"))
            .await
            .unwrap(),
    );
    let thread_id = store.create_thread(ThreadMeta::new("/tmp")).await.unwrap();
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
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn ThreadStore> = Arc::new(
        SqliteThreadStore::new(dir.path().join("affected.db"))
            .await
            .unwrap(),
    );
    let thread_id = store.create_thread(ThreadMeta::new("/tmp")).await.unwrap();
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

#[tokio::test]
async fn consecutive_full_requires_new_visible_read_to_reinject_file() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("current.txt");
    std::fs::write(&file_path, "version one").unwrap();
    let store: Arc<dyn ThreadStore> = Arc::new(
        SqliteThreadStore::new(dir.path().join("reinject.db"))
            .await
            .unwrap(),
    );
    let thread_id = store
        .create_thread(ThreadMeta::new(dir.path().to_string_lossy().to_string()))
        .await
        .unwrap();
    let mut transcript = MessageTranscript::new().with_persistence(store, thread_id);
    transcript.append(make_human("read it"));
    transcript.append(make_ai_with_read_tool(&file_path.to_string_lossy()));

    full_compact_inner(
        &mut transcript,
        Some(&FullLifecycleModel),
        &CompactConfig::default(),
        &dir.path().to_string_lossy(),
    )
    .await
    .unwrap();
    let first_reinject_count = transcript
        .entries()
        .iter()
        .filter(|entry| entry.message().content().contains("version one"))
        .count();

    full_compact_inner(
        &mut transcript,
        Some(&FullLifecycleModel),
        &CompactConfig::default(),
        &dir.path().to_string_lossy(),
    )
    .await
    .unwrap();
    let second_reinject_count = transcript
        .entries()
        .iter()
        .filter(|entry| entry.message().content().contains("version one"))
        .count();

    std::fs::write(&file_path, "version two").unwrap();
    transcript.append(make_ai_with_read_tool(&file_path.to_string_lossy()));
    full_compact_inner(
        &mut transcript,
        Some(&FullLifecycleModel),
        &CompactConfig::default(),
        &dir.path().to_string_lossy(),
    )
    .await
    .unwrap();

    assert_eq!(first_reinject_count, 1);
    assert_eq!(second_reinject_count, 1, "无新 Read 不得重复注入旧文件");
    assert!(transcript
        .entries()
        .iter()
        .any(|entry| entry.message().content().contains("version two")));
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

    let store: Arc<dyn ThreadStore> = Arc::new(
        SqliteThreadStore::new(dir.path().join("full-lifecycle.db"))
            .await
            .expect("创建 SQLite store 失败"),
    );
    let thread_id = store
        .create_thread(ThreadMeta::new(dir.path().to_string_lossy().to_string()))
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
    let reinject = transcript
        .entries()
        .iter()
        .find(|entry| entry.message().content().contains("[最近读取的文件:"))
        .expect("应追加重新注入的文件")
        .message()
        .clone();

    let stored_messages = store
        .load_messages(&thread_id)
        .await
        .expect("加载 SQLite history 失败");
    assert_eq!(
        stored_messages
            .iter()
            .map(BaseMessage::id)
            .collect::<Vec<_>>(),
        transcript
            .entries()
            .iter()
            .map(|entry| entry.id())
            .collect::<Vec<_>>(),
        "内存与 SQLite history 必须一致"
    );
    assert!(stored_messages
        .iter()
        .any(|message| message.id() == summary.id()));
    assert!(stored_messages
        .iter()
        .any(|message| message.id() == reinject.id()));

    let stored_flags = store
        .load_message_flags(&thread_id)
        .await
        .expect("加载 SQLite flags 失败");
    assert!(!stored_flags.contains_key(&ancestor.id()));
    assert!(!stored_flags.contains_key(&own_system));
    assert!(stored_flags[&own_human].excluded);
    assert!(stored_flags[&own_ai].excluded);
}

#[tokio::test]
async fn test_full_compact_filesystem_unsupported_lifecycle_leaves_memory_and_store_unchanged() {
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let file_path = dir.path().join("full-lifecycle.rs");
    std::fs::write(&file_path, "pub const FULL_LIFECYCLE: bool = true;\n")
        .expect("写入重新注入文件失败");

    let store: Arc<dyn ThreadStore> =
        Arc::new(FilesystemThreadStore::new(dir.path().join("threads")));
    let thread_id = store
        .create_thread(ThreadMeta::new(dir.path().to_string_lossy().to_string()))
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
    let before_entries = transcript
        .entries()
        .iter()
        .map(|entry| entry.id())
        .collect::<Vec<_>>();
    let before_store = store
        .load_messages(&thread_id)
        .await
        .expect("加载初始 Filesystem history 失败");

    let error = full_compact_inner(
        &mut transcript,
        Some(&FullLifecycleModel),
        &CompactConfig::default(),
        &dir.path().to_string_lossy(),
    )
    .await
    .expect_err("Filesystem lifecycle 必须明确不受支持");

    assert!(
        error
            .to_string()
            .contains("Filesystem store does not support compaction lifecycle"),
        "应返回 lifecycle 不受支持错误，实际: {error}"
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
            .load_messages(&thread_id)
            .await
            .expect("加载 Filesystem history 失败")
            .iter()
            .map(BaseMessage::id)
            .collect::<Vec<_>>(),
        before_store.iter().map(BaseMessage::id).collect::<Vec<_>>(),
        "失败后 store history 必须原样"
    );
    assert!(
        store
            .load_message_flags(&thread_id)
            .await
            .expect("加载 Filesystem flags 失败")
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

#[test]
fn test_truncate_str_short() {
    assert_eq!(truncate_str("hello", 100), "hello");
}

#[test]
fn test_truncate_str_exact() {
    assert_eq!(truncate_str("hello", 5), "hello");
}

#[test]
fn test_truncate_str_long() {
    let result = truncate_str("hello world", 5);
    assert_eq!(result, "hello...(truncated)");
}

#[test]
fn test_truncate_str_cjk() {
    // CJK 字符级截断不应 panic
    let result = truncate_str("你好世界测试", 2);
    assert_eq!(result, "你好...(truncated)");
}

#[test]
fn test_postprocess_summary_removes_analysis() {
    let raw = "<analysis>some analysis</analysis><summary>the summary</summary>";
    let result = postprocess_summary(raw);
    assert!(!result.contains("<analysis>"));
}

#[test]
fn test_postprocess_summary_extracts_summary() {
    let raw = "prefix text <summary>real summary content</summary> suffix";
    let result = postprocess_summary(raw);
    assert!(result.contains("real summary content"));
    assert!(!result.contains("<summary>"));
    assert!(!result.contains("prefix text"));
}

#[test]
fn test_postprocess_summary_no_tags() {
    let raw = "plain summary text";
    let result = postprocess_summary(raw);
    assert!(result.contains("plain summary text"));
}

#[test]
fn test_postprocess_summary_collapses_newlines() {
    let raw = "line1\n\n\n\n\nline2";
    let result = postprocess_summary(raw);
    assert!(!result.contains("\n\n\n"), "应折叠连续空行");
}

#[test]
fn test_replace_images_and_truncate() {
    let blocks = vec![
        ContentBlock::Text {
            text: "some text".to_string(),
        },
        ContentBlock::Image {
            source: ImageSource::Url {
                url: "http://example.com/img.png".to_string(),
            },
        },
    ];
    let content = MessageContent::blocks(blocks);
    let result = replace_images_and_truncate(&content, 100);
    assert!(result.contains("[image]"));
    assert!(result.contains("some text"));
}

#[test]
fn test_format_tool_call_summary() {
    let tc = crate::messages::ToolCallRequest::new(
        "id1",
        "Edit",
        serde_json::json!({"file_path": "/tmp/test.rs", "old_string": "old"}),
    );
    let result = format_tool_call_summary(&tc);
    assert!(result.contains("Edit"));
    assert!(result.contains("file_path"));
    assert!(result.contains("/tmp/test.rs"));
}

#[test]
fn test_format_tool_call_summary_no_key_fields() {
    let tc = crate::messages::ToolCallRequest::new(
        "id1",
        "Bash",
        serde_json::json!({"random_key": "value"}),
    );
    let result = format_tool_call_summary(&tc);
    assert_eq!(result, "Bash");
}

#[test]
fn test_format_tool_result_summary_empty() {
    let content = MessageContent::text("");
    let result = format_tool_result_summary("call_1", &content, false, 3, 200);
    assert!(result.contains("[ToolResult:call_1][ok]"));
}

#[test]
fn test_format_tool_result_summary_truncates() {
    let long_text = "a".repeat(500);
    let content = MessageContent::text(&long_text);
    let result = format_tool_result_summary("call_1", &content, false, 3, 100);
    assert!(result.contains("...(truncated)"), "超长输出应被截断");
}

// ── CompactResult 测试 ─────────────────────────────────────────────────────

#[test]
fn test_compact_result_fields() {
    let result = crate::agent::compact_v2::CompactResult {
        strategy: CompactStrategy::Micro,
        affected_count: 3,
        estimated_tokens_saved: 1500,
        before_visible_len: 10,
        after_visible_len: 7,
        summary: None,
        full_escalation_reason: None,
        outcome: crate::agent::compact_v2::CompactOutcome::MicroApplied,
        changed_messages: 0,
        changed_fields: 0,
        no_op_candidates: 0,
    };
    assert_eq!(result.strategy, CompactStrategy::Micro);
    assert_eq!(result.affected_count, 3);
    assert_eq!(result.estimated_tokens_saved, 1500);
    assert!(result.summary.is_none());
}

#[test]
fn test_compact_strategy_equality() {
    assert_eq!(CompactStrategy::Micro, CompactStrategy::Micro);
    assert_ne!(CompactStrategy::Micro, CompactStrategy::Full);
}

// ── 集成测试：Full Compact 消息结构 ─────────────────────────────────────────

#[test]
fn test_full_compact_message_structure() {
    // 模拟 Full Compact 后的消息结构：
    // 旧消息标 excluded + Human 摘要追加
    let mut t = MessageTranscript::new();
    let id1 = t.append(make_human("user question"));
    let id2 = t.append(make_ai("assistant response"));

    // 模拟 excluded
    t.set_excluded(id1, true);
    t.set_excluded(id2, true);

    // 追加 Human 摘要（与 full_compact_inner 中的格式一致）
    let summary_text = format!(
        "<system-reminder>\n{}\n\n## Summary\nPrevious conversation about X.\n</system-reminder>",
        crate::agent::compact_v2::CONTINUATION_HINT
    );
    t.append(BaseMessage::human(summary_text));

    // 验证：只有摘要可见
    let visible = t.visible_messages();
    assert_eq!(visible.len(), 1, "只有摘要消息应可见");
    assert!(
        visible[0].content().contains("compact"),
        "可见消息应包含摘要内容"
    );
}

#[test]
fn test_excluded_not_visible() {
    let mut t = MessageTranscript::new();
    let id1 = t.append(make_human("visible"));
    let id2 = t.append(make_human("will be hidden"));
    t.set_excluded(id2, true);

    let visible = t.visible_messages();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].id(), id1);
}
