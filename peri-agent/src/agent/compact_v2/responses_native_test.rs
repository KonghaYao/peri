//! Responses 原生历史在 Compact 生命周期中的行为。
//!
//! 不变量：
//! - 微压缩不得拆改携带原生记录的消息（记录与其派生 text/tool 视图整体保留）；
//! - 大压缩可以整轮删除，但可见视图不得留下孤立工具结果；
//! - 摘要输入与遥测出口不得携带 reasoning 密文与来源身份。

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use peri_model::{
    Model, ModelCapabilities, ModelError, ModelMessage, ModelRequest, ModelResponse, ModelResult,
    ModelStream, StopReason,
};
use tokio_util::sync::CancellationToken;

use super::config::CompactConfig;
use super::full::full_compact_inner;
use super::projection::{
    plan_from_persisted_directives, render_llm_view, MessageProjectionDirective, MicroCompactPlan,
    PersistedDirectiveRestore, ProjectionAction, ProjectionActionEntry, ProjectionTarget,
    ProviderCapabilities, PROJECTION_POLICY_VERSION,
};
use crate::messages::{
    BaseMessage, ContentBlock, MessageContent, MessageId, ToolCallRequest,
    RESPONSES_NATIVE_HISTORY_TAG,
};
use crate::session::transcript::MessageTranscript;
use crate::thread::{SqliteThreadStore, ThreadMeta, ThreadStore};

const ENCRYPTED_REASONING: &str = "CIPHERTEXT-REASONING";

fn native_history_payload() -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "source": {"nonce": "ab".repeat(16), "digest": "cd".repeat(32)},
        "items": [
            {
                "type": "reasoning",
                "id": "rs_1",
                "summary": [{"type": "summary_text", "text": "先想一步"}],
                "encrypted_content": ENCRYPTED_REASONING,
                "status": "completed",
            },
            {
                "type": "message",
                "id": "msg_1",
                "role": "assistant",
                "status": "completed",
                "content": [{"type": "output_text", "text": "答案正文"}],
            },
            {
                "type": "function_call",
                "id": "fc_1",
                "call_id": "call_1",
                "name": "shell",
                "arguments": "{\"command\":\"ls -la\"}",
                "status": "completed",
            },
        ],
    })
}

/// 携带原生记录 + 派生视图（文本/tool_calls）的 assistant 消息，形状与 adapter 输出一致。
fn ai_with_native_history() -> BaseMessage {
    BaseMessage::ai_with_tool_calls(
        MessageContent::Blocks(vec![
            ContentBlock::reasoning("先想一步"),
            ContentBlock::text("答案正文"),
            ContentBlock::responses_native_history(native_history_payload()),
        ]),
        vec![ToolCallRequest::new(
            "call_1",
            "shell",
            serde_json::json!({"command": "ls -la"}),
        )],
    )
}

fn transcript_with_native_history() -> (MessageTranscript, MessageId) {
    let mut transcript = MessageTranscript::new();
    transcript.append(BaseMessage::human("调用工具看看"));
    let ai_id = transcript.append(ai_with_native_history());
    transcript.append(BaseMessage::tool_result("call_1", "总用量 12"));
    (transcript, ai_id)
}

fn assert_no_orphan_tool_results(messages: &[BaseMessage]) {
    let mut call_ids = HashSet::new();
    for message in messages {
        if let BaseMessage::Ai { tool_calls, .. } = message {
            for call in tool_calls {
                call_ids.insert(call.id.clone());
            }
        }
    }
    for message in messages {
        if let BaseMessage::Tool { tool_call_id, .. } = message {
            assert!(
                call_ids.contains(tool_call_id),
                "可见视图不得留下孤立工具结果：{tool_call_id}"
            );
        }
    }
}

#[test]
fn micro_projection_never_splits_messages_with_native_history() {
    let (transcript, ai_id) = transcript_with_native_history();
    let original = transcript.get(ai_id).expect("ai entry").message().clone();

    // 伪造针对原生记录消息的 block 级 action（截断派生文本 + 排除记录块）
    let plan = MicroCompactPlan {
        policy_version: PROJECTION_POLICY_VERSION,
        target_reclaim_tokens: 0,
        actions: vec![
            ProjectionActionEntry {
                message_id: ai_id,
                target: ProjectionTarget::ContentBlock { index: 0 },
                action: ProjectionAction::CompactText { max_chars: 1 },
            },
            ProjectionActionEntry {
                message_id: ai_id,
                target: ProjectionTarget::ContentBlock { index: 2 },
                action: ProjectionAction::Exclude,
            },
        ],
        ..Default::default()
    };

    let view = render_llm_view(
        &transcript,
        &plan,
        &ProviderCapabilities::openai_responses(),
    )
    .expect("render succeeds");
    let projected = view
        .iter()
        .find(|message| message.id() == ai_id)
        .expect("ai message visible");

    assert_eq!(
        projected.message_content(),
        original.message_content(),
        "含原生记录的消息不得被微压缩拆改"
    );
    assert_eq!(projected.tool_calls(), original.tool_calls());
    let wire = serde_json::to_string(projected).expect("serialize");
    assert!(wire.contains(ENCRYPTED_REASONING), "记录必须完整保留");
}

#[test]
fn persisted_directives_do_not_project_native_history_messages() {
    let (mut transcript, ai_id) = transcript_with_native_history();
    transcript.set_flags_projection(
        ai_id,
        MessageProjectionDirective {
            policy_version: PROJECTION_POLICY_VERSION,
            entries: vec![ProjectionActionEntry {
                message_id: ai_id,
                target: ProjectionTarget::ContentBlock { index: 1 },
                action: ProjectionAction::CompactText { max_chars: 1 },
            }],
        },
    );

    let restore = plan_from_persisted_directives(&transcript, PROJECTION_POLICY_VERSION);
    let plan = match restore {
        PersistedDirectiveRestore::Valid(plan) => plan,
        other => panic!("expected valid restore, got {other:?}"),
    };
    assert!(
        plan.actions.is_empty(),
        "含原生记录的消息不得从持久化 directive 恢复出投影 action"
    );

    let original = transcript.get(ai_id).expect("ai entry").message().clone();
    let view = render_llm_view(
        &transcript,
        &plan,
        &ProviderCapabilities::openai_responses(),
    )
    .expect("render succeeds");
    let projected = view
        .iter()
        .find(|message| message.id() == ai_id)
        .expect("ai message visible");
    assert_eq!(projected.message_content(), original.message_content());
}

/// 摘要模型：捕获 compact 摘要请求体。
struct SummaryModel {
    requests: Mutex<Vec<ModelRequest>>,
}

#[async_trait]
impl Model for SummaryModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
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
        ModelResponse::new(
            ModelMessage::assistant_text("<summary>SUMMARY_MARKER</summary>"),
            StopReason::EndTurn,
            None,
            None,
        )
    }
}

#[tokio::test]
async fn full_compact_drops_whole_turn_without_orphans_or_ciphertext_leaks() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store: Arc<dyn ThreadStore> = Arc::new(
        SqliteThreadStore::new(dir.path().join("responses-compact.db"))
            .await
            .expect("sqlite store"),
    );
    let thread_id = store
        .create_thread(ThreadMeta::new("/tmp"))
        .await
        .expect("thread");

    let mut transcript =
        MessageTranscript::new().with_persistence(store.clone(), thread_id.clone());
    let human_id = transcript.append(BaseMessage::human("调用工具看看"));
    let ai_id = transcript.append(ai_with_native_history());
    let tool_id = transcript.append(BaseMessage::tool_result("call_1", "总用量 12"));
    transcript
        .flush_persistence()
        .await
        .expect("flush before compact");

    let model = SummaryModel {
        requests: Mutex::new(Vec::new()),
    };
    let result = full_compact_inner(
        &mut transcript,
        Some(&model),
        &CompactConfig::default(),
        &dir.path().to_string_lossy(),
    )
    .await
    .expect("full compact");
    assert!(
        result.outcome().has_applied_change(),
        "full compact 必须生效"
    );
    transcript
        .flush_persistence()
        .await
        .expect("flush after compact");

    // 整轮删除：不得留下孤立工具结果
    let visible = transcript.visible_model_messages().expect("visible");
    assert_no_orphan_tool_results(&visible);
    assert!(transcript.flags(human_id).excluded);
    assert!(transcript.flags(ai_id).excluded);
    assert!(transcript.flags(tool_id).excluded);

    // 记录本体不被拆改（标记代替删除），排除后不再进入请求
    let stored = transcript.get(ai_id).expect("ai entry").message();
    let wire = serde_json::to_string(stored).expect("serialize");
    assert!(wire.contains(ENCRYPTED_REASONING), "记录必须保持完整");
    assert!(!visible.iter().any(|message| message.id() == ai_id));

    // 摘要输入：只包含可见文本，不带 reasoning 密文与来源身份
    let summary_input = {
        let requests = model.requests.lock().unwrap();
        assert_eq!(requests.len(), 1, "full compact 只请求一次摘要");
        requests[0]
            .messages
            .iter()
            .filter_map(|message| message.text_content())
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(
        !summary_input.contains(ENCRYPTED_REASONING),
        "摘要输入不得携带密文"
    );
    assert!(
        !summary_input.contains(&"cd".repeat(32)),
        "摘要输入不得携带来源摘要"
    );
    assert!(summary_input.contains("答案正文"), "摘要输入应包含可见文本");

    // SQLite 中的记录同样完整
    let loaded = store.load_messages(&thread_id).await.expect("load");
    let stored_ai = loaded
        .iter()
        .find(|message| message.id() == ai_id)
        .expect("ai message persisted");
    let stored_wire = serde_json::to_string(stored_ai).expect("serialize stored");
    assert!(stored_wire.contains(ENCRYPTED_REASONING));

    // 重新开库恢复 canonical payload + flags，不依赖热 transcript。
    drop(transcript);
    drop(store);
    let cold = SqliteThreadStore::new(dir.path().join("responses-compact.db"))
        .await
        .unwrap();
    let payloads = cold.load_payloads(&thread_id).await.unwrap();
    let flags = cold.load_message_flags(&thread_id).await.unwrap();
    let mut restored = MessageTranscript::new().with_own_payloads(payloads);
    restored.set_flags_batch(flags);
    let restored_view = restored.visible_model_messages().unwrap();
    assert_no_orphan_tool_results(&restored_view);
    assert!(!restored_view
        .iter()
        .any(|message| message.has_provider_native_history()));
    assert!(restored_view
        .iter()
        .any(|message| message.content().contains("SUMMARY_MARKER")));
    assert_eq!(
        serde_json::to_value(&visible).unwrap(),
        serde_json::to_value(&restored_view).unwrap()
    );
    let bridge = crate::agent::model_bridge::AgentModelBridge::new(Arc::new(
        peri_model::OpenAiResponsesModel::new(peri_model::OpenAiResponsesConfig::new(
            url::Url::parse("https://example.test/v1").unwrap(),
            "test-key",
            "gpt-5",
        )),
    ));
    use crate::agent::react::ReactLLM;
    let next = bridge
        .observed_provider_request_body(&restored_view, &[])
        .unwrap();
    assert!(next.to_string().contains("SUMMARY_MARKER"));
    assert!(!next["input"].to_string().contains("encrypted_content"));
}

#[test]
fn native_history_tag_constant_matches_envelope() {
    let message = ai_with_native_history();
    assert!(message.has_provider_native_history());
    assert!(serde_json::to_string(&message)
        .expect("serialize")
        .contains(RESPONSES_NATIVE_HISTORY_TAG));
}
