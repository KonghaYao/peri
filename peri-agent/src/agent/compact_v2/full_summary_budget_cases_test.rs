// Responsibility-specific cases share the original test namespace and fixtures.

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
    let result = crate::agent::compact_v2::full::full_compact_inner(
        &mut transcript,
        Some(&model),
        &config,
        "/tmp",
    )
    .await
    .expect("夹具摘要必须完成");
    assert!(result
        .summary
        .expect("the summary fixture result is required for this assertion")
        .ends_with("first and last"));
    let requests = model
        .requests
        .lock()
        .expect("model requests mutex must remain unpoisoned throughout the test")
        .clone();
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
    for (output_limit, summary_max_tokens, summary_max_requests) in [
        (Some(4_096), 0, 3),
        (Some(0), 16_000, 3),
        (Some(4_096), 16_000, 0),
    ] {
        let mut transcript = MessageTranscript::new();
        let original = transcript.append(BaseMessage::human("ORIGINAL_TASK"));
        let model = SummaryModel::with_output_limit(
            [response("<summary>unused</summary>", StopReason::EndTurn)],
            output_limit,
        );
        let config = CompactConfig {
            summary_max_tokens,
            summary_max_requests,
            ..Default::default()
        };
        let error = crate::agent::compact_v2::full::full_compact_inner(
            &mut transcript,
            Some(&model),
            &config,
            "/tmp",
        )
        .await
        .unwrap_err();
        assert!(
            matches!(error, AgentError::CompactSummaryBudgetInvalid { .. }),
            "零值必须是显式配置错误；实际 {error:?}"
        );
        assert!(
            model
                .requests
                .lock()
                .expect("model requests mutex must remain unpoisoned throughout the test")
                .is_empty(),
            "配置错误不得先发出摘要请求"
        );
        assert!(!transcript.flags(original).excluded);
    }
}

/// [回归测试] 次数预算 1：只运行首次生成，不发起任何恢复请求。
#[tokio::test]
async fn summary_request_single_attempt_budget_never_recovers() {
    let mut transcript = MessageTranscript::new();
    let original = transcript.append(BaseMessage::human("ORIGINAL_TASK"));
    let model = SummaryModel::new([
        response("<summary>first", StopReason::MaxTokens),
        response(" and last</summary>", StopReason::EndTurn),
    ]);
    let config = CompactConfig {
        summary_max_requests: 1,
        ..Default::default()
    };
    let error = crate::agent::compact_v2::full::full_compact_inner(
        &mut transcript,
        Some(&model),
        &config,
        "/tmp",
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            error,
            AgentError::CompactRecoveryExhausted {
                attempts: 1,
                last_reason: SummaryRecoveryReason::OutputLimitTruncated,
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
        1
    );
    assert_eq!(
        model
            .responses
            .lock()
            .expect("model responses mutex must remain unpoisoned throughout the test")
            .len(),
        1,
        "不消费备用响应"
    );
    assert!(!transcript.flags(original).excluded);
}

/// [回归测试] 估算装不下不是拒绝压缩的理由：请求照发，由恢复循环续写并合并成一份文本。
///
/// 旧实现会在启动前拒绝发送（`CompactSummaryInputExceeded`），而 Full 只在高压
/// 触发，导致压缩在最需要它的时刻必然失败并终止本轮。
#[tokio::test]
async fn summary_request_over_window_budget_is_still_sent_and_merged() {
    let bound = TestSession::open().await;
    let mut transcript =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    let original = transcript.append(BaseMessage::human("ORIGINAL_TASK"));
    // 200_000 字符 ≈ 50_000 token，远超 20_000 的窗口预算。
    let draft = "x".repeat(200_000);
    let model = SummaryModel::with_budget(
        [
            response(&format!("<summary>{draft}"), StopReason::MaxTokens),
            response(" and last</summary>", StopReason::EndTurn),
        ],
        Some(4_096),
        Some(20_000),
    );
    let result = crate::agent::compact_v2::full::full_compact_inner(
        &mut transcript,
        Some(&model),
        &CompactConfig::default(),
        "/tmp",
    )
    .await
    .expect("the full compact inner fixture result is required for this assertion");
    let summary = result.summary.expect("续写合并后必须产出摘要");
    assert!(
        summary.ends_with(" and last"),
        "{}",
        &summary[..80.min(summary.len())]
    );
    assert_eq!(
        model
            .requests
            .lock()
            .expect("model requests mutex must remain unpoisoned throughout the test")
            .len(),
        2,
        "装不下的请求必须照发并续写，不能在启动前拒绝"
    );
    assert!(
        transcript.get(original).is_none(),
        "Full 提交后原历史已从热态释放（持久化事实仍带 excluded）"
    );
}

/// [回归测试] 摘要元数据无法解析时显式跳过检查，不冒用主模型窗口、不伪造限额。
#[tokio::test]
async fn summary_request_with_unknown_metadata_keeps_provider_defaults() {
    let bound = TestSession::open().await;
    let mut transcript =
        MessageTranscript::new().with_persistence(bound.resources(), bound.thread_id.clone());
    let original = transcript.append(BaseMessage::human("ORIGINAL_TASK"));
    // 无输出上限（provider 未声明）→ 请求沿用 provider 默认，不被改写。
    let model = SummaryModel::with_budget(
        [
            response("<summary>first", StopReason::MaxTokens),
            response(" and last</summary>", StopReason::EndTurn),
        ],
        None,
        Some(1_000),
    );
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
    assert_eq!(
        model
            .requests
            .lock()
            .expect("model requests mutex must remain unpoisoned throughout the test")
            .len(),
        2
    );
    assert!(
        transcript.get(original).is_none(),
        "Full 提交后原历史已从热态释放（持久化事实仍带 excluded）"
    );
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
    let error = crate::agent::compact_v2::full::full_compact_inner(
        &mut transcript,
        Some(&model),
        &CompactConfig::default(),
        "/tmp",
    )
    .await
    .unwrap_err();
    assert!(matches!(error, AgentError::ModelError(_)));
    let requests = model
        .requests
        .lock()
        .expect("model requests mutex must remain unpoisoned throughout the test")
        .clone();
    assert_eq!(requests.len(), 2, "provider 拒绝不触发第三轮");
    assert!(
        requests
            .iter()
            .all(|request| request.max_tokens == Some(4_096)),
        "两次请求都必须在不超出自身上限的预算内"
    );
    assert_eq!(
        model
            .responses
            .lock()
            .expect("model responses mutex must remain unpoisoned throughout the test")
            .len(),
        0,
        "不重放已失败请求"
    );
    assert!(!transcript.flags(original).excluded);
    assert_eq!(transcript.len(), 1);
}
