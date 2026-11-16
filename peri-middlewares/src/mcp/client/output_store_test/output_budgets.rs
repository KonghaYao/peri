// ── M7：模型面 UTF-8 字节预算 ────────────────────────────────────────────────

/// 单行巨量输出：行数预算挡不住，必须由字节预算兜住，且提示计入预算。
#[tokio::test]
async fn single_line_output_is_bounded_by_the_byte_budget() {
    let raw = "λ".repeat(MAX_BYTES);
    assert_eq!(raw.lines().count(), 1, "前置：单行输入不触发行预算");
    let output = super::format_output(None, None, raw, false).await;
    assert!(
        output.len() <= MAX_BYTES,
        "模型面文本必须落在字节预算内: {} > {}",
        output.len(),
        MAX_BYTES
    );
    assert!(output.contains("[MCP output truncated:"));
    assert!(output.contains("bytes"), "提示必须说明触发原因: {output}");
    // 单行输入被字节预算截断后仍是完整 UTF-8（按字符边界截断，不产生半个字符）：
    // 截断点必须落在 2 字节的 λ 边界上，正文长度是偶数。
    let notice_at = output.find("[MCP output truncated:").unwrap();
    let body = &output[..notice_at];
    assert_eq!(
        body.len() % 2,
        0,
        "截断必须落在 UTF-8 字符边界: {}",
        body.len()
    );
    assert!(!output.contains('\u{FFFD}'));
}

/// 行数与字节同时超限时，提示必须把两个原因都说清。
#[tokio::test]
async fn byte_and_line_budgets_report_both_reasons() {
    let raw: String = (0..3000)
        .map(|line| format!("line-{line}-{}", "x".repeat(200)))
        .collect::<Vec<_>>()
        .join("\n");
    let total_lines = raw.lines().count();
    let total_bytes = raw.len();
    assert!(
        total_lines > MAX_LINES && total_bytes > MAX_BYTES,
        "前置：两层预算都超限"
    );
    let output = super::format_output(None, None, raw, false).await;
    assert!(output.len() <= MAX_BYTES, "len={}", output.len());
    assert!(
        output.contains(&format!("{total_lines} total lines, {total_bytes} bytes")),
        "两个触发原因都必须出现: {}",
        &output[output.len().saturating_sub(240)..]
    );
}

/// 预算内输出逐字返回：不做任何落存、不追加提示（既有语义不变）。
#[tokio::test]
async fn output_within_budget_is_returned_verbatim() {
    let raw = "short λ output\nsecond line\n".to_string();
    let output = super::format_output(None, None, raw.clone(), false).await;
    assert_eq!(output, raw);
}

/// 落存不可用时必须明说，且不得给出任何可回查地址。
#[tokio::test]
async fn byte_truncation_without_store_never_claims_a_path() {
    let raw = "y".repeat(MAX_BYTES + 1);
    let output = super::format_output(None, None, raw, true).await;
    assert!(output.contains("[MCP error output truncated:"));
    assert!(output.contains("Full output NOT saved: workspace output store not configured"));
    assert!(!output.contains("resource_uri="));
    assert!(!output.contains("path="));
    assert!(output.len() <= MAX_BYTES);
}
