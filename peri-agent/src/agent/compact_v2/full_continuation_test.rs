//! 摘要截断恢复：已生成正文进入续写请求，完成前不替换历史。

use crate::agent::compact_v2::config::CompactConfig;

use crate::agent::compact_v2::full::summary::SummaryRecoveryReason;
use crate::error::AgentError;
use crate::messages::BaseMessage;
use crate::session::test_resources::TestSession;
use crate::session::transcript::MessageTranscript;
use peri_model::ModelMessage;
use peri_model::ModelRequest;
use peri_model::{
    ContentBlock, Model, ModelCapabilities, ModelResponse, ModelResult, ModelStream, StopReason,
};
use std::collections::VecDeque;
use std::sync::Mutex;
use tokio_util::sync::CancellationToken;

struct SummaryModel {
    requests: Mutex<Vec<ModelRequest>>,
    responses: Mutex<VecDeque<ModelResult<ModelResponse>>>,
    /// 模型已解析的单次输出上限；`None` = provider 不声明（H6 的只读预算接口）。
    output_limit: Option<u32>,
    /// 摘要模型自己的已解析上下文窗口；`None` = 元数据未知。
    context_window: Option<u32>,
}

impl SummaryModel {
    fn new(responses: impl IntoIterator<Item = ModelResult<ModelResponse>>) -> Self {
        Self::with_output_limit(responses, None)
    }

    fn with_output_limit(
        responses: impl IntoIterator<Item = ModelResult<ModelResponse>>,
        output_limit: Option<u32>,
    ) -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            responses: Mutex::new(responses.into_iter().collect()),
            output_limit,
            context_window: None,
        }
    }

    fn with_budget(
        responses: impl IntoIterator<Item = ModelResult<ModelResponse>>,
        output_limit: Option<u32>,
        context_window: Option<u32>,
    ) -> Self {
        Self {
            context_window,
            ..Self::with_output_limit(responses, output_limit)
        }
    }
}

#[async_trait::async_trait]
impl Model for SummaryModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }

    fn output_token_limit(&self) -> Option<u32> {
        self.output_limit
    }

    fn context_window(&self) -> Option<u32> {
        self.context_window
    }

    async fn stream(&self, _: ModelRequest, _: CancellationToken) -> ModelResult<ModelStream> {
        unreachable!("Full uses complete")
    }

    async fn complete(
        &self,
        request: ModelRequest,
        _: CancellationToken,
    ) -> ModelResult<ModelResponse> {
        self.requests
            .lock()
            .expect("self requests mutex must remain unpoisoned throughout the test")
            .push(request);
        self.responses
            .lock()
            .expect("self responses mutex must remain unpoisoned throughout the test")
            .pop_front()
            .expect("self responses fixture must contain the expected element")
    }
}

fn response(text: &str, stop: StopReason) -> ModelResult<ModelResponse> {
    ModelResponse::try_new(ModelMessage::assistant_text(text), stop, None, None)
}

/// 派生请求里回填的 assistant 正文片段（按出现顺序）。
fn assistant_texts(request: &ModelRequest) -> Vec<String> {
    request
        .messages
        .iter()
        .filter(|message| matches!(message, ModelMessage::Assistant { .. }))
        .filter_map(|message| message.text_content())
        .collect()
}

/// [回归测试] MaxTokens 后不能丢掉已经付费生成的摘要，应续写并一次提交完整结果。
#[tokio::test]
async fn full_continuation_preserves_chunks_and_commits_complete_summary() {
    let bound = TestSession::open().await;
    let mut transcript =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    let original = transcript.append(BaseMessage::human("ORIGINAL_TASK"));
    transcript
        .flush_persistence()
        .await
        .expect("the flush persistence fixture result is required for this assertion");
    // 同时覆盖正文 Unicode 与跨响应拆开的控制标签，不能插入分隔符破坏原文。
    let model = SummaryModel::new([
        response("<sum", StopReason::MaxTokens),
        response(
            "mary>保留决策，继续未完成工作。</summary>",
            StopReason::EndTurn,
        ),
    ]);
    let result = crate::agent::compact_v2::full::full_compact_inner(
        &mut transcript,
        Some(&model),
        &CompactConfig::default(),
        "/tmp",
    )
    .await
    .expect("the full compact inner fixture result is required for this assertion");
    assert!(result
        .summary
        .expect("the summary fixture result is required for this assertion")
        .ends_with("保留决策，继续未完成工作。"));
    assert!(
        transcript.get(original).is_none(),
        "Full 提交后原历史已从热态释放（持久化事实仍带 excluded）"
    );
    {
        let requests = model
            .requests
            .lock()
            .expect("model requests mutex must remain unpoisoned throughout the test");
        assert_eq!(requests.len(), 2);
        for request in requests.iter() {
            // 夹具不声明输出上限：摘要请求不得代填 16000，也不得覆写 provider 上限。
            assert_eq!(request.max_tokens, None);
            assert!(request.tools.is_empty());
            assert!(serde_json::to_string(request)
                .expect("to_string must accept this test fixture representation")
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
        let continuation_prompt = requests[1]
            .messages
            .last()
            .expect("requests messages fixture must contain the expected element")
            .text_content()
            .expect("the text content fixture result is required for this assertion");
        assert!(continuation_prompt.contains("cut off"));
        assert!(continuation_prompt.contains("Continue exactly from the end"));
        assert!(continuation_prompt.contains("Do not call tools"));
    }
    let stored = bound
        .resources
        .load_session_snapshot(&bound.thread_id)
        .await
        .expect("the load session snapshot fixture result is required for this assertion");
    assert_eq!(
        stored.payloads.len(),
        2,
        "只有原文和完整摘要，不持久化半截响应"
    );
    assert!(stored.flags[&original].excluded);
    assert!(stored.payloads[1]
        .as_message()
        .expect("the as message fixture result is required for this assertion")
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
        transcript
            .flush_persistence()
            .await
            .expect("the flush persistence fixture result is required for this assertion");
        let expected_requests = responses.len();
        let model = SummaryModel::new(responses);
        let result = crate::agent::compact_v2::full::full_compact_inner(
            &mut transcript,
            Some(&model),
            &CompactConfig::default(),
            "/tmp",
        )
        .await
        .expect("the full compact inner fixture result is required for this assertion");
        assert!(result
            .summary
            .expect("the summary fixture result is required for this assertion")
            .ends_with("保留工作状态。"));
        assert_eq!(
            model
                .requests
                .lock()
                .expect("model requests mutex must remain unpoisoned throughout the test")
                .len(),
            expected_requests
        );
        assert!(
            transcript.get(original).is_none(),
            "Full 提交后原历史已从热态释放（持久化事实仍带 excluded）"
        );
        let stored = bound
            .resources
            .load_session_snapshot(&bound.thread_id)
            .await
            .expect("the load session snapshot fixture result is required for this assertion");
        assert_eq!(stored.payloads.len(), 2);
        assert!(stored.flags[&original].excluded);
    }
}

/// [回归测试] 三次截断且始终未闭合：用尽预算后返回恢复耗尽，不提交半截摘要。
#[tokio::test]
async fn full_continuation_exhausts_bounded_budget_without_committing() {
    let bound = TestSession::open().await;
    let mut transcript =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    let original = transcript.append(BaseMessage::human("ORIGINAL_TASK"));
    transcript
        .flush_persistence()
        .await
        .expect("the flush persistence fixture result is required for this assertion");
    let model = SummaryModel::new([
        response("<summary>first", StopReason::MaxTokens),
        response(" second", StopReason::MaxTokens),
        response(" third", StopReason::MaxTokens),
        response(" never requested", StopReason::MaxTokens),
    ]);
    let error = crate::agent::compact_v2::full::full_compact_inner(
        &mut transcript,
        Some(&model),
        &CompactConfig::default(),
        "/tmp",
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            error,
            AgentError::CompactRecoveryExhausted {
                attempts: 3,
                last_reason: SummaryRecoveryReason::OutputLimitTruncated,
            }
        ),
        "预算耗尽必须携带真实次数与最后原因：{error:?}"
    );
    let (request_count, third) = {
        let requests = model
            .requests
            .lock()
            .expect("model requests mutex must remain unpoisoned throughout the test");
        (
            requests.len(),
            serde_json::to_string(&requests[2])
                .expect("to_string must accept this test fixture representation"),
        )
    };
    assert_eq!(request_count, 3, "绝不发起第 4 次逻辑生成");
    assert_eq!(
        model
            .responses
            .lock()
            .expect("model responses mutex must remain unpoisoned throughout the test")
            .len(),
        1,
        "多余的夹具响应不得被消费"
    );
    assert!(!transcript.flags(original).excluded);
    assert!(!transcript.full_compaction_committed());
    assert_eq!(
        transcript
            .visible_model_messages()
            .expect("the visible model messages fixture result is required for this assertion")
            .len(),
        1
    );
    let stored = bound
        .resources
        .load_session_snapshot(&bound.thread_id)
        .await
        .expect("the load session snapshot fixture result is required for this assertion");
    assert_eq!(stored.payloads.len(), 1, "不持久化半截摘要");
    assert!(stored.flags.is_empty());
    // 已有草稿只回填一次：第三次请求不得重复第二段正文。
    assert_eq!(third.matches("second").count(), 1, "{third}");
}

/// [回归测试] 截断 → EndTurn 未闭合 → 补全：中间一次不终止主循环。
#[tokio::test]
async fn full_continuation_resumes_after_unclosed_end_turn() {
    let bound = TestSession::open().await;
    let mut transcript =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    let original = transcript.append(BaseMessage::human("ORIGINAL_TASK"));
    transcript
        .flush_persistence()
        .await
        .expect("the flush persistence fixture result is required for this assertion");
    let model = SummaryModel::new([
        response("<summary>first", StopReason::MaxTokens),
        response(" second", StopReason::EndTurn),
        response(" third</summary>", StopReason::EndTurn),
    ]);
    let result = crate::agent::compact_v2::full::full_compact_inner(
        &mut transcript,
        Some(&model),
        &CompactConfig::default(),
        "/tmp",
    )
    .await
    .expect("the full compact inner fixture result is required for this assertion");
    assert!(result
        .summary
        .expect("the summary fixture result is required for this assertion")
        .ends_with("first second third"));
    assert_eq!(
        model
            .requests
            .lock()
            .expect("model requests mutex must remain unpoisoned throughout the test")
            .len(),
        3
    );
    assert!(
        transcript.get(original).is_none(),
        "Full 提交后原历史已从热态释放（持久化事实仍带 excluded）"
    );
    let stored = bound
        .resources
        .load_session_snapshot(&bound.thread_id)
        .await
        .expect("the load session snapshot fixture result is required for this assertion");
    assert_eq!(stored.payloads.len(), 2, "只提交一次完整摘要");
}

/// [回归测试] 有草稿时的一次空续写只增加恢复提示，草稿保留并可在剩余预算内完成。
#[tokio::test]
async fn full_continuation_keeps_draft_across_empty_continuation() {
    let bound = TestSession::open().await;
    let mut transcript =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    let original = transcript.append(BaseMessage::human("ORIGINAL_TASK"));
    let model = SummaryModel::new([
        response("<summary>first", StopReason::MaxTokens),
        response("", StopReason::EndTurn),
        response(" and last</summary>", StopReason::EndTurn),
    ]);
    let result = crate::agent::compact_v2::full::full_compact_inner(
        &mut transcript,
        Some(&model),
        &CompactConfig::default(),
        "/tmp",
    )
    .await
    .expect("the full compact inner fixture result is required for this assertion");
    assert!(result
        .summary
        .expect("the summary fixture result is required for this assertion")
        .ends_with("first and last"));
    let requests = model
        .requests
        .lock()
        .expect("model requests mutex must remain unpoisoned throughout the test");
    assert_eq!(requests.len(), 3);
    assert_eq!(
        assistant_texts(&requests[2]),
        vec!["<summary>first".to_owned()],
        "空续写不得丢弃或重放已有草稿"
    );
    assert!(
        transcript.get(original).is_none(),
        "Full 提交后原历史已从热态释放（持久化事实仍带 excluded）"
    );
}

/// [回归测试] 只产生隐藏思考的首轮没有可用正文：允许一次重新生成，且不注入空 assistant。
#[tokio::test]
async fn full_continuation_regenerates_after_reasoning_only_truncation() {
    let bound = TestSession::open().await;
    let mut transcript =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    let original = transcript.append(BaseMessage::human("ORIGINAL_TASK"));
    let model = SummaryModel::new([
        ModelResponse::try_new(
            ModelMessage::assistant(vec![ContentBlock::reasoning("private reasoning")], vec![]),
            StopReason::MaxTokens,
            None,
            None,
        ),
        response(
            "<summary>RECOVERED_ON_REGENERATION</summary>",
            StopReason::EndTurn,
        ),
    ]);
    let config = CompactConfig {
        summary_max_tokens: 4096,
        ..Default::default()
    };
    let result = crate::agent::compact_v2::full::full_compact_inner(
        &mut transcript,
        Some(&model),
        &config,
        "/tmp",
    )
    .await
    .expect("the full compact inner fixture result is required for this assertion");
    assert!(result
        .summary
        .expect("the summary fixture result is required for this assertion")
        .ends_with("RECOVERED_ON_REGENERATION"));
    let requests = model
        .requests
        .lock()
        .expect("model requests mutex must remain unpoisoned throughout the test");
    assert_eq!(requests.len(), 2);
    assert!(
        assistant_texts(&requests[1]).is_empty(),
        "重新生成不得回填空 assistant 或 reasoning"
    );
    let serialized = serde_json::to_string(&requests[1])
        .expect("to_string must accept this test fixture representation");
    assert!(!serialized.contains("private reasoning"));
    assert_eq!(
        summary_target_of(&requests[1]),
        (4096 / 2) / 2,
        "重新生成的目标在首轮目标上再减半"
    );
    assert_eq!(
        requests[1].max_tokens, requests[0].max_tokens,
        "provider 输出上限不变"
    );
    assert!(
        transcript.get(original).is_none(),
        "Full 提交后原历史已从热态释放（持久化事实仍带 excluded）"
    );
}

/// [回归测试] 重新生成仍无可用正文：提前结束，保留原历史。
#[tokio::test]
async fn full_continuation_stops_when_regeneration_is_also_empty() {
    let mut transcript = MessageTranscript::new();
    let original = transcript.append(BaseMessage::human("ORIGINAL_TASK"));
    let model = SummaryModel::new([
        response("", StopReason::EndTurn),
        response("   ", StopReason::EndTurn),
    ]);
    let error = crate::agent::compact_v2::full::full_compact_inner(
        &mut transcript,
        Some(&model),
        &CompactConfig::default(),
        "/tmp",
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            error,
            AgentError::CompactRecoveryExhausted {
                attempts: 2,
                last_reason: SummaryRecoveryReason::EmptyResponse,
            }
        ),
        "{error:?}"
    );
    assert_eq!(
        model
            .requests
            .lock()
            .expect("model requests mutex must remain unpoisoned throughout the test")
            .len(),
        2,
        "重新生成失败后不再空转"
    );
    assert!(!transcript.flags(original).excluded);
    assert_eq!(
        transcript
            .visible_model_messages()
            .expect("the visible model messages fixture result is required for this assertion")
            .len(),
        1
    );
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
    let error = crate::agent::compact_v2::full::full_compact_inner(
        &mut transcript,
        Some(&model),
        &CompactConfig::default(),
        "/tmp",
    )
    .await
    .unwrap_err();
    assert!(matches!(error, AgentError::ModelError(_)));
    assert_eq!(
        model
            .requests
            .lock()
            .expect("model requests mutex must remain unpoisoned throughout the test")
            .len(),
        2
    );
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
        ModelResponse::try_new(
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
    let result = crate::agent::compact_v2::full::full_compact_inner(
        &mut transcript,
        Some(&model),
        &config,
        "/tmp",
    )
    .await
    .expect("the full compact inner fixture result is required for this assertion");
    assert!(result
        .summary
        .expect("the summary fixture result is required for this assertion")
        .ends_with("first and last"));
    let requests = model
        .requests
        .lock()
        .expect("model requests mutex must remain unpoisoned throughout the test");
    assert_eq!(
        requests[0].max_tokens, None,
        "夹具未声明模型输出上限时沿用 provider 默认，不写请求预算"
    );
    assert!(requests[0]
        .messages
        .last()
        .expect("requests messages fixture must contain the expected element")
        .text_content()
        .expect("the text content fixture result is required for this assertion")
        .contains("1024 output tokens"));
    assert_eq!(
        requests[1].max_tokens, requests[0].max_tokens,
        "续写必须沿用首轮预算"
    );
    let serialized = serde_json::to_string(&requests[1])
        .expect("to_string must accept this test fixture representation");
    assert!(!serialized.contains("PRIVATE_REASONING"));
    assert!(!serialized.contains("{summary_target_tokens}"));
}

/// [回归测试] 空白或未闭合的续写不代表完整摘要，耗尽预算后仍不得提交。
#[tokio::test]
async fn full_continuation_rejects_unfinished_end_turn() {
    for (tail, expected_reason) in [
        ("", SummaryRecoveryReason::EmptyResponse),
        ("   ", SummaryRecoveryReason::EmptyResponse),
        (" still unfinished", SummaryRecoveryReason::UnclosedSummary),
    ] {
        let mut transcript = MessageTranscript::new();
        let original = transcript.append(BaseMessage::human("ORIGINAL_TASK"));
        let model = SummaryModel::new([
            response("<summary>first", StopReason::MaxTokens),
            response(tail, StopReason::EndTurn),
            response(tail, StopReason::EndTurn),
        ]);
        let error = crate::agent::compact_v2::full::full_compact_inner(
            &mut transcript,
            Some(&model),
            &CompactConfig::default(),
            "/tmp",
        )
        .await
        .unwrap_err();
        assert!(
            matches!(
                error,
                AgentError::CompactRecoveryExhausted {
                    attempts: 3,
                    last_reason,
                } if last_reason == expected_reason
            ),
            "未闭合续写不能变成成功：{error:?}"
        );
        assert_eq!(
            model
                .requests
                .lock()
                .expect("model requests mutex must remain unpoisoned throughout the test")
                .len(),
            3
        );
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
        let model = SummaryModel::new([ModelResponse::try_new(
            message,
            StopReason::EndTurn,
            None,
            None,
        )]);
        let error = crate::agent::compact_v2::full::full_compact_inner(
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
        assert_eq!(
            model
                .requests
                .lock()
                .expect("model requests mutex must remain unpoisoned throughout the test")
                .len(),
            1
        );
        assert!(!transcript.flags(original).excluded);
    }
}
include!("full_summary_budget_cases_test.rs");
