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
    /// 模型已解析的单次输出上限；`None` = provider 不声明（H6 的只读预算接口）。
    output_limit: Option<u32>,
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
            // 夹具不声明输出上限：摘要请求不得代填 16000，也不得覆写 provider 上限。
            assert_eq!(request.max_tokens, None);
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
    assert_eq!(
        requests[0].max_tokens, None,
        "夹具未声明模型输出上限时沿用 provider 默认，不写请求预算"
    );
    assert!(requests[0]
        .messages
        .last()
        .unwrap()
        .text_content()
        .unwrap()
        .contains("1024 output tokens"));
    assert_eq!(
        requests[1].max_tokens, requests[0].max_tokens,
        "续写必须沿用首轮预算"
    );
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

// ─── H6：摘要请求预算与长度目标同源 ─────────────────────────────────────────────

/// 捕获低/高输出预算、显式摘要配置与首轮/续写请求。
///
/// 断言：请求预算始终等于模型已解析上限（不写成 `summary_max_tokens`）、续写不变、
/// 提示词中的长度目标受同一上限约束。触发条件用假 provider 记录请求验证，不靠
/// 「配置值大于 profile 值」的静态比较推断远端必然失败。
async fn budget_case(output_limit: Option<u32>, summary_max_tokens: u32) -> Vec<ModelRequest> {
    let bound = TestSession::open().await;
    let mut transcript =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    transcript.append(BaseMessage::human("ORIGINAL_TASK"));
    let model = SummaryModel::with_output_limit(
        [
            response("<summary>first", StopReason::MaxTokens),
            response(" and last</summary>", StopReason::EndTurn),
        ],
        output_limit,
    );
    let config = CompactConfig {
        summary_max_tokens,
        ..Default::default()
    };
    let result = full_compact_inner(&mut transcript, Some(&model), &config, "/tmp")
        .await
        .expect("夹具摘要必须完成");
    assert!(result.summary.unwrap().ends_with("first and last"));
    let requests = model.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2, "截断后只允许一次续写");
    requests
}

/// 提示词中声明的长度目标（提示词模板写死 `Target at most N output tokens`）。
///
/// 续写请求的最后一条是续写指令，因此按内容定位摘要指令，而不是取最后一条。
fn summary_target_of(request: &ModelRequest) -> u32 {
    let prompt = request
        .messages
        .iter()
        .filter_map(ModelMessage::text_content)
        .find(|text| text.contains("Target at most "))
        .expect("摘要指令必须声明目标");
    let after = prompt
        .split("Target at most ")
        .nth(1)
        .expect("模板必须声明目标");
    after
        .split_whitespace()
        .next()
        .expect("目标数值")
        .parse()
        .expect("目标必须是数字")
}

#[tokio::test]
async fn summary_request_keeps_low_resolved_output_limit() {
    let requests = budget_case(Some(4_096), 16_000).await;
    for request in &requests {
        assert_eq!(
            request.max_tokens,
            Some(4_096),
            "低输出预算模型上请求预算必须等于已解析上限，不能被 16000 覆写"
        );
    }
    assert_eq!(
        requests[1].max_tokens, requests[0].max_tokens,
        "续写沿用首轮预算"
    );
    assert_eq!(
        summary_target_of(&requests[0]),
        4_096 / 2,
        "长度目标受有效输出预算约束（min(summary_max_tokens, 上限) 的一半）"
    );
}

#[tokio::test]
async fn summary_request_uses_resolved_limit_when_target_is_smaller() {
    let requests = budget_case(Some(64_000), 16_000).await;
    for request in &requests {
        assert_eq!(
            request.max_tokens,
            Some(64_000),
            "目标更小时也不改写请求预算：预算是 provider 解析值的沿用"
        );
    }
    assert_eq!(
        summary_target_of(&requests[0]),
        16_000 / 2,
        "长度目标取 min(summary_max_tokens, 有效预算) 的一半"
    );
    assert_eq!(summary_target_of(&requests[1]), 16_000 / 2);
}

#[tokio::test]
async fn summary_request_without_resolved_limit_leaves_provider_default() {
    let requests = budget_case(None, 16_000).await;
    for request in &requests {
        assert_eq!(
            request.max_tokens, None,
            "无法解析上限时沿用 provider 默认，不代填 16000"
        );
    }
    assert_eq!(summary_target_of(&requests[0]), 8_000);
}

/// [回归测试] 零值不是「不限」：明确报配置错误，不静默代填任意常量。
#[tokio::test]
async fn summary_request_rejects_zero_budget() {
    for (output_limit, summary_max_tokens) in [(Some(4_096), 0), (Some(0), 16_000)] {
        let mut transcript = MessageTranscript::new();
        let original = transcript.append(BaseMessage::human("ORIGINAL_TASK"));
        let model = SummaryModel::with_output_limit(
            [response("<summary>unused</summary>", StopReason::EndTurn)],
            output_limit,
        );
        let config = CompactConfig {
            summary_max_tokens,
            ..Default::default()
        };
        let error = full_compact_inner(&mut transcript, Some(&model), &config, "/tmp")
            .await
            .unwrap_err();
        assert!(
            matches!(error, AgentError::CompactSummaryBudgetInvalid { .. }),
            "零值必须是显式配置错误；实际 {error:?}"
        );
        assert!(
            model.requests.lock().unwrap().is_empty(),
            "配置错误不得先发出摘要请求"
        );
        assert!(!transcript.flags(original).excluded);
    }
}

/// [回归测试] provider 拒绝摘要请求时保留既有 Full 错误语义，不无限重试。
#[tokio::test]
async fn summary_request_provider_rejection_stops_after_the_continuation_budget() {
    let mut transcript = MessageTranscript::new();
    let original = transcript.append(BaseMessage::human("ORIGINAL_TASK"));
    let model = SummaryModel::with_output_limit(
        [
            response("<summary>first", StopReason::MaxTokens),
            Err(peri_model::ModelError::http_status(
                400,
                "fixture",
                Some("req-compact-over-limit"),
            )),
        ],
        Some(4_096),
    );
    let error = full_compact_inner(
        &mut transcript,
        Some(&model),
        &CompactConfig::default(),
        "/tmp",
    )
    .await
    .unwrap_err();
    assert!(matches!(error, AgentError::ModelError(_)));
    let requests = model.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2, "provider 拒绝不触发第三轮");
    assert!(
        requests
            .iter()
            .all(|request| request.max_tokens == Some(4_096)),
        "两次请求都必须在不超出自身上限的预算内"
    );
    assert_eq!(model.responses.lock().unwrap().len(), 0, "不重放已失败请求");
    assert!(!transcript.flags(original).excluded);
    assert_eq!(transcript.len(), 1);
}
