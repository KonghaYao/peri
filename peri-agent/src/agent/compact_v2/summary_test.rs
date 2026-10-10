use super::*;

fn response(text: &str, stop_reason: StopReason) -> ModelResponse {
    ModelResponse::new(ModelMessage::assistant_text(text), stop_reason, None, None).unwrap()
}

fn completed(assembly: SummaryAssembly) -> (String, bool) {
    match assembly {
        SummaryAssembly::Complete { text, truncated } => (text, truncated),
        SummaryAssembly::Continue => panic!("expected completed assembly"),
    }
}

#[test]
fn assembly_requires_only_one_continuation_and_preserves_exact_chunk_boundary() {
    let first = response("<sum", StopReason::MaxTokens);
    assert!(matches!(
        assemble_summary(&first, None).unwrap(),
        SummaryAssembly::Continue
    ));
    let second = response("mary>保留决策。</summary>", StopReason::EndTurn);
    let before = (first.clone(), second.clone());
    let (text, truncated) = completed(assemble_summary(&first, Some(&second)).unwrap());
    assert!(text.ends_with("保留决策。"));
    assert!(!truncated);
    assert_eq!((first.clone(), second.clone()), before);
    assert_eq!(
        completed(assemble_summary(&first, Some(&second)).unwrap()),
        (text, truncated)
    );
}

#[test]
fn assembly_keeps_two_limited_outputs_and_marks_omitted_tail() {
    let first = response("<summary>first ", StopReason::MaxTokens);
    let second = response("second unfinished", StopReason::MaxTokens);
    let (text, truncated) = completed(assemble_summary(&first, Some(&second)).unwrap());
    assert!(text.contains("first second unfinished"));
    assert!(text.contains("remaining tail was omitted"));
    assert!(!text.contains("<summary>"));
    assert!(truncated);
}

#[test]
fn assembly_accepts_closed_summary_even_at_output_limit() {
    let first = response("<summary>complete</summary>", StopReason::MaxTokens);
    let (text, truncated) = completed(assemble_summary(&first, None).unwrap());
    assert!(text.ends_with("complete"));
    assert!(!truncated);
    let first = response("<summary>com", StopReason::MaxTokens);
    let second = response("plete</summary>", StopReason::MaxTokens);
    let (text, truncated) = completed(assemble_summary(&first, Some(&second)).unwrap());
    assert!(text.ends_with("complete"));
    assert!(!truncated);
}

#[test]
fn assembly_rejects_hidden_or_empty_summary_without_truncation_fallback() {
    for text in [
        "",
        "   ",
        "<analysis><summary>hidden</summary></analysis>",
        "<thinking><summary>hidden</summary></thinking>",
        "<think><summary>hidden</summary></think>",
        "<summary>   </summary>",
    ] {
        let first = response(text, StopReason::MaxTokens);
        let second = response("", StopReason::MaxTokens);
        assert!(assemble_summary(&first, Some(&second)).is_err(), "{text}");
    }
}

#[test]
fn assembly_never_replays_hidden_reasoning_into_summary() {
    let first = ModelResponse::new(
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
    let second = response(" second", StopReason::MaxTokens);
    let (text, _) = completed(assemble_summary(&first, Some(&second)).unwrap());
    assert!(text.contains("first second"));
    assert!(!text.contains("SECRET"));
}

#[test]
fn assembly_rejects_tool_calls_in_either_response() {
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
        assemble_summary(&tool_response, None),
        Err(AgentError::CompactIncompleteResponse {
            stop_reason: StopReason::ToolUse
        })
    ));
    let first = response("<summary>first", StopReason::MaxTokens);
    assert!(matches!(
        assemble_summary(&first, Some(&tool_response)),
        Err(AgentError::CompactIncompleteResponse {
            stop_reason: StopReason::ToolUse
        })
    ));
}

#[test]
fn assembly_does_not_treat_unfinished_end_turn_as_output_limit() {
    let first = response("<summary>first", StopReason::MaxTokens);
    for tail in ["", "   ", " second"] {
        let second = response(tail, StopReason::EndTurn);
        assert!(matches!(
            assemble_summary(&first, Some(&second)),
            Err(AgentError::CompactIncompleteResponse {
                stop_reason: StopReason::MaxTokens
            })
        ));
    }
}

#[test]
fn test_postprocess_summary_removes_analysis() {
    let raw = "<analysis>some analysis</analysis><summary>the summary</summary>";
    let result = postprocess_summary(raw).unwrap();
    assert!(!result.contains("<analysis>"));
}

#[test]
fn test_postprocess_summary_extracts_summary() {
    let raw = "prefix text <summary>real summary content</summary> suffix";
    let result = postprocess_summary(raw).unwrap();
    assert!(result.contains("real summary content"));
    assert!(!result.contains("<summary>"));
    assert!(!result.contains("prefix text"));
}

#[test]
fn test_postprocess_summary_no_tags() {
    let raw = "plain summary text";
    let result = postprocess_summary(raw).unwrap();
    assert!(result.contains("plain summary text"));
}

#[test]
fn test_postprocess_summary_collapses_newlines() {
    let raw = "line1\n\n\n\n\nline2";
    let result = postprocess_summary(raw).unwrap();
    assert!(!result.contains("\n\n\n"), "应折叠连续空行");
}

#[test]
fn test_postprocess_summary_rejects_reasoning_only_and_malformed_blocks() {
    for raw in [
        "<analysis><analysis>private</analysis></analysis>",
        "<analysis>first</analysis> <thinking>second</thinking>",
        "<thinking><analysis>private</analysis></thinking>",
        "<analysis><thinking>unfinished",
        "<think>unfinished",
        "</analysis>",
        "<analysis>private</thinking>",
    ] {
        assert!(
            postprocess_summary(raw).is_none(),
            "思考标签不得变成可提交摘要：{raw}"
        );
    }
}

#[test]
fn test_postprocess_summary_preserves_body_after_nested_reasoning() {
    let raw = "<thinking>hidden <analysis>nested hidden</analysis></thinking><summary>保留结论与 analysis 方法。</summary><analysis>unfinished tail";
    let summary = postprocess_summary(raw).unwrap();
    assert!(summary.ends_with("保留结论与 analysis 方法。"));
    assert!(!summary.contains("hidden"));
    assert!(!summary.contains("unfinished"));
}

#[test]
fn test_postprocess_summary_preserves_plain_text_and_similar_tag_names() {
    let body = "analysis and thinking are ordinary words; <analysis_notes>保留正文</analysis_notes> &lt;analysis&gt;";
    let summary = postprocess_summary(body).unwrap();
    assert!(
        summary.ends_with(body),
        "只识别精确控制标签，不剥除相近正文"
    );
}
