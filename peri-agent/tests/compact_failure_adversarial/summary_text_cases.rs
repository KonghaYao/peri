// Responsibility-specific cases share the original test namespace and fixtures.

/// [回归测试] 讨论解析器时，闭合摘要中的思考标签字面量必须进入下一次 Reason。
#[tokio::test]
async fn test_closed_summary_preserves_literal_reasoning_tags() {
    let (bound, mut ctx, reason, _, _) = make_case(Failure::Success, 0, false, None).await;
    ctx.compact.compact_llm = Some(Arc::new(SummaryText(
        "<summary>parser strips <think> and </analysis> tags</summary>",
    )));
    let result = peri_agent::agent::stages::run_react_loop(ctx.clone(), 4).await;
    assert!(
        matches!(result, LoopResult::Completed),
        "有效正文不得被误判为空摘要：{result:?}"
    );
    assert_eq!(reason.requests.lock().len(), 1);
    let request_text = reason.requests.lock()[0]
        .iter()
        .map(|message| message.content())
        .collect::<Vec<_>>()
        .join("\n");
    // Reason 使用既有 system-reminder XML 转义；canonical 摘要仍保存原始字面量。
    assert!(
        request_text.contains("parser strips &lt;think&gt; and &lt;/analysis&gt; tags"),
        "{request_text}"
    );
    let transcript = std::mem::take(&mut *ctx.session.transcript.write());
    transcript
        .flush_persistence()
        .await
        .expect("the flush persistence fixture result is required for this assertion");
    let stored = bound
        .resources
        .load_session_snapshot(&bound.thread_id)
        .await
        .expect("the load session snapshot fixture result is required for this assertion");
    let original = stored
        .payloads
        .iter()
        .find_map(|payload| {
            payload
                .as_message()
                .filter(|message| message.content() == "original task")
        })
        .expect("the find map fixture result is required for this assertion");
    assert!(stored.flags[&original.id()].excluded);
    assert!(stored
        .payloads
        .iter()
        .any(|payload| payload.as_message().is_some_and(|message| message
            .content()
            .contains("parser strips <think> and </analysis> tags"))));
}

/// [回归测试] 思考块内的闭合 summary 是草稿，不能提交并替代历史。
#[tokio::test]
async fn test_summary_inside_reasoning_cannot_replace_history() {
    let (bound, mut ctx, reason, _, _) = make_case(Failure::Success, 0, false, None).await;
    ctx.compact.compact_llm = Some(Arc::new(SummaryText(
        "<analysis><summary>draft</summary></analysis>",
    )));
    let result = peri_agent::agent::stages::run_react_loop(ctx.clone(), 4).await;
    assert!(
        matches!(
            result,
            LoopResult::Error(AgentError::CompactRecoveryExhausted { attempts: 2, .. })
        ),
        "思考草稿不得成为摘要：{result:?}"
    );
    assert!(reason.requests.lock().is_empty());
    let transcript = std::mem::take(&mut *ctx.session.transcript.write());
    transcript
        .flush_persistence()
        .await
        .expect("the flush persistence fixture result is required for this assertion");
    let stored = bound
        .resources
        .load_session_snapshot(&bound.thread_id)
        .await
        .expect("the load session snapshot fixture result is required for this assertion");
    assert!(stored.flags.values().all(|flags| !flags.excluded));
    assert_eq!(stored.payloads.len(), 2, "只保留原始任务与本次继续输入");
}

/// [回归测试] 外层闭合摘要正文是普通文本，内部标签和外围尾部均不能污染它。
#[tokio::test]
async fn test_closed_summary_after_reasoning_preserves_body_and_ignores_draft() {
    let (_bound, mut ctx, reason, _, _) = make_case(Failure::Success, 0, false, None).await;
    ctx.compact.compact_llm = Some(Arc::new(SummaryText("<analysis><thinking><summary>draft</summary></thinking></analysis><summary>literal <analysis> </thinking> <think> </analysis> <thinking> </think> tags</summary></analysis>")));
    let result = peri_agent::agent::stages::run_react_loop(ctx, 4).await;
    assert!(
        matches!(result, LoopResult::Completed),
        "闭合正文必须优先于尾部标签：{result:?}"
    );
    let requests = reason.requests.lock();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].iter().any(|message| message
        .content()
        .contains("literal &lt;analysis&gt; &lt;/thinking&gt; &lt;think&gt; &lt;/analysis&gt; &lt;thinking&gt; &lt;/think&gt; tags")));
    assert!(requests[0]
        .iter()
        .all(|message| !message.content().contains("draft")));
}
