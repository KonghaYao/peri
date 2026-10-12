use super::*;

fn response(text: &str, stop_reason: StopReason) -> ModelResponse {
    ModelResponse::new(ModelMessage::assistant_text(text), stop_reason, None, None).unwrap()
}

fn classify(text: &str, stop_reason: StopReason) -> AgentResult<SummaryStep> {
    classify_summary_step(&response(text, stop_reason), "")
}

fn completed(step: SummaryStep) -> String {
    match step {
        SummaryStep::Complete(text) => text,
        other => panic!("期望可提交摘要，实际 {other:?}"),
    }
}

fn draft(step: SummaryStep) -> (String, SummaryRecoveryReason) {
    match step {
        SummaryStep::Draft { chunk, reason } => (chunk, reason),
        other => panic!("期望可回填草稿，实际 {other:?}"),
    }
}

fn empty_reason(step: SummaryStep) -> SummaryRecoveryReason {
    match step {
        SummaryStep::Empty { reason } => reason,
        other => panic!("期望空响应分类，实际 {other:?}"),
    }
}

// ─── 完整摘要与跨片段闭合 ────────────────────────────────────────────────────────

#[test]
fn classify_accepts_closed_summary_even_at_output_limit() {
    for stop_reason in [StopReason::EndTurn, StopReason::MaxTokens] {
        let text = completed(classify("<summary>complete</summary>", stop_reason).unwrap());
        assert!(text.ends_with("complete"));
    }
}

#[test]
fn classify_completes_across_responses_without_inserting_separators() {
    // 夹具同时覆盖正文 Unicode 与跨响应拆开的控制标签。
    let first = classify("<sum", StopReason::MaxTokens).unwrap();
    let (chunk, reason) = draft(first);
    assert_eq!(chunk, "<sum");
    assert_eq!(reason, SummaryRecoveryReason::OutputLimitTruncated);
    let step = classify_summary_step(
        &response("mary>保留决策。</summary>", StopReason::EndTurn),
        &chunk,
    )
    .unwrap();
    assert!(completed(step).ends_with("保留决策。"));
}

#[test]
fn classify_prefers_the_summary_closed_by_the_current_response() {
    // 模型改写全文时以本次闭合的摘要为准，不与先前片段拼接。
    let step = classify_summary_step(
        &response("<summary>full rewrite</summary>", StopReason::MaxTokens),
        "<summary>stale draft",
    )
    .unwrap();
    let text = completed(step);
    assert!(text.ends_with("full rewrite"));
    assert!(!text.contains("stale draft"));
}

#[test]
fn classify_accepts_plain_text_summary_without_control_tags() {
    let text = completed(classify("plain summary text", StopReason::EndTurn).unwrap());
    assert!(text.ends_with("plain summary text"));
}

#[test]
fn classify_collapses_repeated_blank_lines() {
    let text = completed(classify("line1\n\n\n\n\nline2", StopReason::EndTurn).unwrap());
    assert!(text.ends_with("line1\n\nline2"), "应折叠连续空行：{text}");
}

// ─── 未完成：可回填草稿 ─────────────────────────────────────────────────────────

#[test]
fn classify_keeps_truncated_body_as_draft() {
    let (chunk, reason) = draft(classify("<summary>first", StopReason::MaxTokens).unwrap());
    assert_eq!(chunk, "<summary>first");
    assert_eq!(reason, SummaryRecoveryReason::OutputLimitTruncated);
}

#[test]
fn classify_does_not_treat_unfinished_end_turn_as_output_limit() {
    for (text, accumulated) in [("<summary>first", ""), (" second", "<summary>first")] {
        let (chunk, reason) = draft(
            classify_summary_step(&response(text, StopReason::EndTurn), accumulated).unwrap(),
        );
        assert_eq!(chunk, text);
        assert_eq!(
            reason,
            SummaryRecoveryReason::UnclosedSummary,
            "EndTurn 未闭合不是输出上限截断：{text}"
        );
    }
}

#[test]
fn classify_resumes_an_unclosed_draft_without_restarting_it() {
    // 一次 EndTurn 未闭合后仍可在剩余预算内补全，不立即返回协议错误。
    let (first_chunk, _) = draft(classify("<summary>first", StopReason::MaxTokens).unwrap());
    let step = classify_summary_step(
        &response(" second</summary>", StopReason::EndTurn),
        &first_chunk,
    )
    .unwrap();
    assert!(completed(step).ends_with("first second"));
}

// ─── 空响应 ────────────────────────────────────────────────────────────────────

#[test]
fn classify_reports_empty_response_for_blank_or_hidden_only_output() {
    for (text, stop_reason) in [
        ("", StopReason::MaxTokens),
        ("   ", StopReason::MaxTokens),
        ("<analysis>thinking only</analysis>", StopReason::EndTurn),
        ("<analysis>thinking only</analysis>", StopReason::MaxTokens),
        (
            "<thinking><summary>hidden</summary></thinking>",
            StopReason::MaxTokens,
        ),
        (
            "<think><summary>hidden</summary></think>",
            StopReason::EndTurn,
        ),
        ("<summary>   </summary>", StopReason::MaxTokens),
    ] {
        let reason = empty_reason(classify(text, stop_reason).unwrap());
        assert_eq!(reason, SummaryRecoveryReason::EmptyResponse, "{text}");
    }
}

#[test]
fn classify_never_exposes_hidden_reasoning_as_summary_text() {
    let response = ModelResponse::new(
        ModelMessage::assistant(
            vec![
                ContentBlock::reasoning("SECRET"),
                ContentBlock::text("<summary>first"),
            ],
            vec![],
        ),
        StopReason::MaxTokens,
        None,
        None,
    )
    .unwrap();
    let (chunk, _) = draft(classify_summary_step(&response, "").unwrap());
    assert!(!chunk.contains("SECRET"));
    assert!(chunk.contains("first"));
}

#[test]
fn classify_does_not_accept_a_closing_tag_that_only_exists_in_reasoning() {
    let response = ModelResponse::new(
        ModelMessage::assistant(
            vec![ContentBlock::reasoning("<summary>hidden</summary>")],
            vec![],
        ),
        StopReason::EndTurn,
        None,
        None,
    )
    .unwrap();
    let step = classify_summary_step(&response, "").unwrap();
    assert_eq!(
        empty_reason(step),
        SummaryRecoveryReason::EmptyResponse,
        "reasoning 里的闭合标签不能当作有效完成"
    );
}

// ─── 不可恢复的协议错误 ─────────────────────────────────────────────────────────

#[test]
fn classify_rejects_tool_calls_in_either_response() {
    let tool_response = ModelResponse::new(
        ModelMessage::assistant(
            vec![ContentBlock::text("<summary>unsafe</summary>")],
            vec![peri_model::ToolCall::new(
                "call",
                "Bash",
                peri_model::JsonObject::default(),
            )],
        ),
        StopReason::MaxTokens,
        None,
        None,
    )
    .unwrap();
    assert!(matches!(
        classify_summary_step(&tool_response, ""),
        Err(AgentError::CompactIncompleteResponse {
            stop_reason: StopReason::ToolUse
        })
    ));
    assert!(matches!(
        classify_summary_step(&tool_response, "<summary>first"),
        Err(AgentError::CompactIncompleteResponse {
            stop_reason: StopReason::ToolUse
        })
    ));
}

#[test]
fn classify_rejects_unbalanced_control_tags() {
    for text in ["</analysis>", "<analysis>private</thinking>"] {
        assert!(
            matches!(
                classify(text, StopReason::MaxTokens),
                Err(AgentError::CompactIncompleteResponse {
                    stop_reason: StopReason::MaxTokens
                })
            ),
            "控制标签不成对是协议错误：{text}"
        );
    }
}

#[test]
fn classify_rejects_abnormal_stop_reasons() {
    let stop_reason = StopReason::Other {
        value: "content_filtered".into(),
    };
    assert!(matches!(
        classify("<summary>unsafe</summary>", stop_reason),
        Err(AgentError::CompactIncompleteResponse {
            stop_reason: StopReason::Other { .. }
        })
    ));
}

// ─── 思考块过滤（原 postprocess 用例）────────────────────────────────────────────

#[test]
fn classify_strips_analysis_and_extracts_the_summary_body() {
    let text = completed(
        classify(
            "<analysis>some analysis</analysis><summary>the summary</summary>",
            StopReason::EndTurn,
        )
        .unwrap(),
    );
    assert!(text.ends_with("the summary"));
    assert!(!text.contains("<analysis>"));
    assert!(!text.contains("some analysis"));
}

#[test]
fn classify_drops_text_outside_the_summary_tags() {
    let text = completed(
        classify(
            "prefix text <summary>real summary content</summary> suffix",
            StopReason::EndTurn,
        )
        .unwrap(),
    );
    assert!(text.ends_with("real summary content"));
    assert!(!text.contains("<summary>"));
    assert!(!text.contains("prefix text"));
    assert!(!text.contains("suffix"));
}

#[test]
fn classify_keeps_body_after_nested_reasoning() {
    let text = completed(
        classify(
            "<thinking>hidden <analysis>nested hidden</analysis></thinking><summary>保留结论与 analysis 方法。</summary><analysis>unfinished tail",
            StopReason::EndTurn,
        )
        .unwrap(),
    );
    assert!(text.ends_with("保留结论与 analysis 方法。"));
    assert!(!text.contains("hidden"));
    assert!(!text.contains("unfinished"));
}

#[test]
fn classify_preserves_plain_text_and_similar_tag_names() {
    let body = "analysis and thinking are ordinary words; <analysis_notes>保留正文</analysis_notes> &lt;analysis&gt;";
    let text = completed(classify(body, StopReason::EndTurn).unwrap());
    assert!(text.ends_with(body), "只识别精确控制标签，不剥除相近正文");
}
