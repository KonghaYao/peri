//! 摘要截断恢复：已生成正文进入续写请求，完成前不替换历史。

use super::*;
use crate::error::AgentError;
use crate::session::test_resources::TestSession;
use peri_model::{
    ContentBlock, Model, ModelCapabilities, ModelResponse, ModelResult, ModelStream, StopReason,
};
use std::collections::VecDeque;
use std::sync::Mutex;

struct SummaryModel {
    requests: Mutex<Vec<ModelRequest>>,
    responses: Mutex<VecDeque<ModelResult<ModelResponse>>>,
}

impl SummaryModel {
    fn new(responses: impl IntoIterator<Item = ModelResult<ModelResponse>>) -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            responses: Mutex::new(responses.into_iter().collect()),
        }
    }
}

#[async_trait::async_trait]
impl Model for SummaryModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }

    async fn stream(&self, _: ModelRequest, _: CancellationToken) -> ModelResult<ModelStream> {
        unreachable!("Full uses complete")
    }

    async fn complete(
        &self,
        request: ModelRequest,
        _: CancellationToken,
    ) -> ModelResult<ModelResponse> {
        self.requests.lock().unwrap().push(request);
        self.responses.lock().unwrap().pop_front().unwrap()
    }
}

fn response(text: &str, stop: StopReason) -> ModelResult<ModelResponse> {
    ModelResponse::new(ModelMessage::assistant_text(text), stop, None, None)
}

/// [回归测试] MaxTokens 后不能丢掉已经付费生成的摘要，应续写并一次提交完整结果。
#[tokio::test]
async fn full_continuation_preserves_chunks_and_commits_complete_summary() {
    let bound = TestSession::open().await;
    let mut transcript =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    let original = transcript.append(BaseMessage::human("ORIGINAL_TASK"));
    transcript.flush_persistence().await.unwrap();
    // 同时覆盖正文 Unicode 与跨响应拆开的控制标签，不能插入分隔符破坏原文。
    let model = SummaryModel::new([
        response("<sum", StopReason::MaxTokens),
        response(
            "mary>保留决策，继续未完成工作。</summary>",
            StopReason::EndTurn,
        ),
    ]);
    let result = full_compact_inner(
        &mut transcript,
        Some(&model),
        &CompactConfig::default(),
        "/tmp",
    )
    .await
    .unwrap();
    assert!(result
        .summary
        .unwrap()
        .ends_with("保留决策，继续未完成工作。"));
    assert!(transcript.flags(original).excluded);
    {
        let requests = model.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        for request in requests.iter() {
            assert_eq!(request.max_tokens, Some(16_000));
            assert!(request.tools.is_empty());
            assert!(serde_json::to_string(request)
                .unwrap()
                .contains("ORIGINAL_TASK"));
        }
        assert_eq!(
            &requests[1].messages[..requests[0].messages.len()],
            &requests[0].messages
        );
        assert_eq!(
            requests[1].messages[requests[0].messages.len()],
            ModelMessage::assistant_text("<sum")
        );
        assert_eq!(requests[1].messages.len(), requests[0].messages.len() + 2);
        assert_eq!(requests[1].max_tokens, requests[0].max_tokens);
        let continuation_prompt = requests[1].messages.last().unwrap().text_content().unwrap();
        assert!(continuation_prompt.contains("cut off by the output token limit"));
        assert!(continuation_prompt.contains("There will be no further continuation"));
    }
    let stored = bound
        .resources
        .load_session_snapshot(&bound.thread_id)
        .await
        .unwrap();
    assert_eq!(
        stored.payloads.len(),
        2,
        "只有原文和完整摘要，不持久化半截响应"
    );
    assert!(stored.flags[&original].excluded);
    assert!(stored.payloads[1]
        .as_message()
        .unwrap()
        .content()
        .contains("保留决策，继续未完成工作。"));
}

#[tokio::test]
async fn full_continuation_accepts_closed_summary_at_output_limit() {
    for responses in [
        vec![response(
            "<summary>保留工作状态。</summary>",
            StopReason::MaxTokens,
        )],
        vec![
            response("<summary>保留", StopReason::MaxTokens),
            response("工作状态。</summary>", StopReason::MaxTokens),
        ],
    ] {
        let bound = TestSession::open().await;
        let mut transcript =
            MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
        let original = transcript.append(BaseMessage::human("ORIGINAL_TASK"));
        transcript.flush_persistence().await.unwrap();
        let expected_requests = responses.len();
        let model = SummaryModel::new(responses);
        let result = full_compact_inner(
            &mut transcript,
            Some(&model),
            &CompactConfig::default(),
            "/tmp",
        )
        .await
        .unwrap();
        assert!(result.summary.unwrap().ends_with("保留工作状态。"));
        assert_eq!(model.requests.lock().unwrap().len(), expected_requests);
        assert!(transcript.flags(original).excluded);
        let stored = bound
            .resources
            .load_session_snapshot(&bound.thread_id)
            .await
            .unwrap();
        assert_eq!(stored.payloads.len(), 2);
        assert!(stored.flags[&original].excluded);
    }
}

#[tokio::test]
async fn full_continuation_keeps_two_chunks_and_never_requests_third() {
    let bound = TestSession::open().await;
    let mut transcript =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    let original = transcript.append(BaseMessage::human("ORIGINAL_TASK"));
    transcript.flush_persistence().await.unwrap();
    let model = SummaryModel::new([
        response("<summary>first", StopReason::MaxTokens),
        response(" second", StopReason::MaxTokens),
        response(" third", StopReason::MaxTokens),
    ]);
    let result = full_compact_inner(
        &mut transcript,
        Some(&model),
        &CompactConfig::default(),
        "/tmp",
    )
    .await
    .unwrap();
    let summary = result.summary.unwrap();
    assert!(summary.contains("first second"));
    assert!(summary.contains("remaining tail was omitted"));
    assert!(!summary.contains("third"));
    assert_eq!(model.requests.lock().unwrap().len(), 2);
    assert_eq!(model.responses.lock().unwrap().len(), 1);
    assert!(transcript.flags(original).excluded);
    assert!(transcript.full_compaction_committed());
    let stored = bound
        .resources
        .load_session_snapshot(&bound.thread_id)
        .await
        .unwrap();
    assert_eq!(stored.payloads.len(), 2);
    assert!(stored.flags[&original].excluded);
    assert_eq!(
        stored.payloads[0].as_message().unwrap().content(),
        "ORIGINAL_TASK"
    );
}

/// [回归测试] 只产生隐藏思考的截断没有可续接正文，不能重放空 assistant 或重新付费生成。
#[tokio::test]
async fn full_continuation_rejects_reasoning_only_truncation() {
    let mut transcript = MessageTranscript::new();
    let original = transcript.append(BaseMessage::human("ORIGINAL_TASK"));
    let model = SummaryModel::new([ModelResponse::new(
        ModelMessage::assistant(vec![ContentBlock::reasoning("private reasoning")], vec![]),
        StopReason::MaxTokens,
        None,
        None,
    )]);
    let error = full_compact_inner(
        &mut transcript,
        Some(&model),
        &CompactConfig::default(),
        "/tmp",
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error,
        AgentError::CompactIncompleteResponse {
            stop_reason: StopReason::MaxTokens
        }
    ));
    assert_eq!(model.requests.lock().unwrap().len(), 1);
    assert!(!transcript.flags(original).excluded);
}

/// [回归测试] 续写传输失败不得从头重试原摘要请求，也不得隐藏原文。
#[tokio::test]
async fn full_continuation_provider_failure_preserves_history() {
    let mut transcript = MessageTranscript::new();
    let original = transcript.append(BaseMessage::human("ORIGINAL_TASK"));
    let model = SummaryModel::new([
        response("<summary>first", StopReason::MaxTokens),
        Err(peri_model::ModelError::http_status(
            503,
            "fixture",
            None::<&str>,
        )),
    ]);
    let error = full_compact_inner(
        &mut transcript,
        Some(&model),
        &CompactConfig::default(),
        "/tmp",
    )
    .await
    .unwrap_err();
    assert!(matches!(error, AgentError::ModelError(_)));
    assert_eq!(model.requests.lock().unwrap().len(), 2);
    assert!(!transcript.flags(original).excluded);
    assert_eq!(transcript.len(), 1);
}

/// [回归测试] 续写仅回灌正文，不能把未签名 thinking 或 reasoning 当作可发送上下文。
#[tokio::test]
async fn full_continuation_omits_reasoning_from_next_request() {
    let bound = TestSession::open().await;
    let mut transcript =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    transcript.append(BaseMessage::human("ORIGINAL_TASK"));
    let model = SummaryModel::new([
        ModelResponse::new(
            ModelMessage::assistant(
                vec![
                    ContentBlock::reasoning("PRIVATE_REASONING"),
                    ContentBlock::text("<summary>first"),
                ],
                vec![],
            ),
            StopReason::MaxTokens,
            None,
            None,
        ),
        response(" and last</summary>", StopReason::EndTurn),
    ]);
    let config = CompactConfig {
        summary_max_tokens: 2048,
        ..Default::default()
    };
    let result = full_compact_inner(&mut transcript, Some(&model), &config, "/tmp")
        .await
        .unwrap();
    assert!(result.summary.unwrap().ends_with("first and last"));
    let requests = model.requests.lock().unwrap();
    assert_eq!(requests[0].max_tokens, Some(2048));
    assert!(requests[0]
        .messages
        .last()
        .unwrap()
        .text_content()
        .unwrap()
        .contains("1024 output tokens"));
    let serialized = serde_json::to_string(&requests[1]).unwrap();
    assert!(!serialized.contains("PRIVATE_REASONING"));
    assert!(!serialized.contains("{summary_target_tokens}"));
}

/// [回归测试] 空白或未闭合的续写不代表完整摘要，也不能按空摘要重新生成全部内容。
#[tokio::test]
async fn full_continuation_rejects_unfinished_end_turn() {
    for tail in ["", "   ", " still unfinished"] {
        let mut transcript = MessageTranscript::new();
        let original = transcript.append(BaseMessage::human("ORIGINAL_TASK"));
        let model = SummaryModel::new([
            response("<summary>first", StopReason::MaxTokens),
            response(tail, StopReason::EndTurn),
        ]);
        let error = full_compact_inner(
            &mut transcript,
            Some(&model),
            &CompactConfig::default(),
            "/tmp",
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            AgentError::CompactIncompleteResponse {
                stop_reason: StopReason::MaxTokens
            }
        ));
        assert_eq!(model.requests.lock().unwrap().len(), 2);
        assert!(!transcript.flags(original).excluded);
        assert_eq!(transcript.len(), 1);
    }
}

/// [回归测试] 即使 stop_reason 错标为 EndTurn，带工具的摘要响应也不得提交或继续执行。
#[tokio::test]
async fn full_continuation_rejects_tools_in_response() {
    let tool = peri_model::ToolCall::new("call-1", "Bash", peri_model::JsonObject::default());
    for message in [
        ModelMessage::assistant(
            vec![ContentBlock::text("<summary>partial")],
            vec![tool.clone()],
        ),
        ModelMessage::assistant(
            vec![
                ContentBlock::text("<summary>partial"),
                ContentBlock::ToolUse { tool_call: tool },
            ],
            vec![],
        ),
    ] {
        let mut transcript = MessageTranscript::new();
        let original = transcript.append(BaseMessage::human("ORIGINAL_TASK"));
        let model =
            SummaryModel::new([ModelResponse::new(message, StopReason::EndTurn, None, None)]);
        let error = full_compact_inner(
            &mut transcript,
            Some(&model),
            &CompactConfig::default(),
            "/tmp",
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            AgentError::CompactIncompleteResponse {
                stop_reason: StopReason::ToolUse
            }
        ));
        assert_eq!(model.requests.lock().unwrap().len(), 1);
        assert!(!transcript.flags(original).excluded);
    }
}
