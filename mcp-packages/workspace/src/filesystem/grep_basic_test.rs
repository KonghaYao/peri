use super::*;

#[tokio::test]
async fn test_grep_hit() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("test.txt"),
        "needle in a haystack\nother line",
    )
    .unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"pattern": "needle", "output_mode": "content", "path": "./"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("needle"), "should find needle: {result}");
}

#[tokio::test]
async fn test_grep_no_match() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("test.txt"), "haystack only").unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"pattern": "zzz_not_here", "output_mode": "content", "path": "./"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("No matches found"),
        "should report no match: {result}"
    );
}

#[tokio::test]
async fn test_grep_missing_pattern() {
    let dir = tempfile::tempdir().unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("Missing required parameter 'pattern'"),
        "should report missing pattern: {err_msg}"
    );
}

#[tokio::test]
async fn test_grep_regex() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("test.txt"), "needle123\nneedle456").unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"pattern": "needle[0-9]+", "output_mode": "content", "path": "./"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("needle"), "regex should match: {result}");
}

#[test]
fn test_grep_description_extended() {
    let tool = GrepTool::new("/tmp");
    let desc = tool.description();
    assert!(desc.contains("regex"), "description 应提及正则支持");
    assert!(
        desc.contains("Output modes:"),
        "description 应包含 Output modes 段落"
    );
    assert!(desc.len() > 200, "description 应为扩展后的多段落文本");
}

#[tokio::test]
async fn test_grep_files_only() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "needle here\nother line").unwrap();
    std::fs::write(dir.path().join("b.txt"), "no match here").unwrap();
    std::fs::write(dir.path().join("c.txt"), "needle again").unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    let result = tool
            .invoke(serde_json::json!({"pattern": "needle", "output_mode": "files_with_matches", "path": "./"}), peri_agent::tools::ToolContext::new(&[], "."))
            .await
            .unwrap();
    assert!(result.contains("a.txt"), "should find a.txt: {result}");
    assert!(result.contains("c.txt"), "should find c.txt: {result}");
    assert!(
        !result.contains("needle here"),
        "should not include line content: {result}"
    );
}

#[tokio::test]
async fn test_grep_count() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "needle\nneedle\nneedle").unwrap();
    std::fs::write(dir.path().join("b.txt"), "needle once").unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"pattern": "needle", "output_mode": "count", "path": "./"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("a.txt:3"),
        "a.txt should have 3 matches: {result}"
    );
    assert!(
        result.contains("b.txt:1"),
        "b.txt should have 1 match: {result}"
    );
}

#[tokio::test]
async fn test_grep_case_insensitive() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("test.txt"), "NEEDLE\nneedle\nNeedle").unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    let result = tool
            .invoke(serde_json::json!({"pattern": "NEEDLE", "output_mode": "content", "-i": true, "path": "./"}), peri_agent::tools::ToolContext::new(&[], "."))
            .await
            .unwrap();
    assert!(
        result.contains("NEEDLE"),
        "should match uppercase: {result}"
    );
    assert!(
        result.contains("needle"),
        "should match lowercase: {result}"
    );
    assert!(
        result.contains("Needle"),
        "should match mixed case: {result}"
    );
}

#[tokio::test]
async fn test_grep_glob_filter() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("test.txt"), "needle in txt").unwrap();
    std::fs::write(dir.path().join("test.rs"), "needle in rs").unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    let result = tool
            .invoke(serde_json::json!({"pattern": "needle", "output_mode": "content", "glob": "*.txt", "path": "./"}), peri_agent::tools::ToolContext::new(&[], "."))
            .await
            .unwrap();
    assert!(result.contains("test.txt"), "should find in .txt: {result}");
    assert!(
        !result.contains("test.rs"),
        "should not find in .rs: {result}"
    );
}

#[tokio::test]
async fn test_grep_type_filter() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("test.txt"), "needle in txt").unwrap();
    std::fs::write(dir.path().join("test.rs"), "needle in rs").unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({
                "pattern": "needle",
                "output_mode": "content",
                "type": "rust",
                "path": "./"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("test.rs"), "should find in .rs: {result}");
    assert!(
        !result.contains("test.txt"),
        "should not find in .txt with type=rust: {result}"
    );
}

#[test]
fn test_grep_tool_name() {
    let tool = GrepTool::new("/tmp");
    assert_eq!(tool.name(), "Grep");
}

#[tokio::test]
async fn test_grep_invalid_output_mode() {
    let dir = tempfile::tempdir().unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({
                "pattern": "needle",
                "output_mode": "invalid_mode"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("Error"),
        "should report invalid output_mode: {err_msg}"
    );
}

#[tokio::test]
async fn test_grep_offset() {
    let dir = tempfile::tempdir().unwrap();
    let lines: Vec<String> = (0..10).map(|i| format!("line {} needle", i)).collect();
    std::fs::write(dir.path().join("test.txt"), lines.join("\n")).unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({
                "pattern": "needle",
                "output_mode": "content",
                "path": "./",
                "offset": 5
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        !result.contains("line 0"),
        "should skip first 5 lines: {result}"
    );
    assert!(
        result.contains("line 5"),
        "should include line 5+: {result}"
    );
}

async fn grep_page(tool: &GrepTool, offset: u64, head_limit: u64) -> String {
    tool.invoke(
        serde_json::json!({
            "pattern": "needle",
            "output_mode": "content",
            "offset": offset,
            "head_limit": head_limit
        }),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap()
}

/// [回归测试] offset 必须作用于结构化结果，而不是格式化后的字符串。
/// 旧实现只收集 head_limit 行，`head_limit=10, offset=10` 只能拿到提示行。
#[tokio::test]
async fn test_grep_pagination_pages_use_structured_offset() {
    let dir = tempfile::tempdir().unwrap();
    let lines: Vec<String> = (0..30).map(|i| format!("needle-{i:02}")).collect();
    std::fs::write(dir.path().join("test.txt"), lines.join("\n")).unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());

    let first = grep_page(&tool, 0, 10).await;
    let second = grep_page(&tool, 10, 10).await;

    for i in 0..10 {
        assert!(first.contains(&format!("needle-{i:02}")), "{first}");
        assert!(!second.contains(&format!("needle-{i:02}")), "{second}");
    }
    for i in 10..20 {
        assert!(second.contains(&format!("needle-{i:02}")), "{second}");
        assert!(!first.contains(&format!("needle-{i:02}")), "{first}");
    }
    assert!(second.contains("truncated at 10 lines"), "{second}");
    assert_ne!(first, second, "第二页必须与第一页不同");
}

/// 最后一页恰好填满 head_limit 时不应出现截断提示行。
#[tokio::test]
async fn test_grep_pagination_last_page_has_no_hint_line() {
    let dir = tempfile::tempdir().unwrap();
    let lines: Vec<String> = (0..30).map(|i| format!("needle-{i:02}")).collect();
    std::fs::write(dir.path().join("test.txt"), lines.join("\n")).unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());

    let last = grep_page(&tool, 20, 10).await;
    assert!(!last.contains("truncated at"), "{last}");
    assert_eq!(last.lines().count(), 10, "{last}");
    for i in 20..30 {
        assert!(last.contains(&format!("needle-{i:02}")), "{last}");
    }
}

/// offset 超出结果末尾：返回明确提示，不伪造行。
#[tokio::test]
async fn test_grep_pagination_offset_beyond_end() {
    let dir = tempfile::tempdir().unwrap();
    let lines: Vec<String> = (0..30).map(|i| format!("needle-{i:02}")).collect();
    std::fs::write(dir.path().join("test.txt"), lines.join("\n")).unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());

    let beyond = grep_page(&tool, 100, 10).await;
    assert_eq!(beyond, "No matches found after offset 100.");
}

/// head_limit=0（不限）时 offset 仍按结构化结果生效，且无截断提示。
#[tokio::test]
async fn test_grep_pagination_unlimited_head_limit_with_offset() {
    let dir = tempfile::tempdir().unwrap();
    let lines: Vec<String> = (0..30).map(|i| format!("needle-{i:02}")).collect();
    std::fs::write(dir.path().join("test.txt"), lines.join("\n")).unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());

    let page = grep_page(&tool, 10, 0).await;
    assert_eq!(page.lines().count(), 20, "{page}");
    assert!(
        page.contains("needle-10") && page.contains("needle-29"),
        "{page}"
    );
    assert!(!page.contains("needle-09"), "{page}");
    assert!(!page.contains("truncated at"), "{page}");
}

/// 多文件重复分页必须稳定：跨文件按路径排序，同一查询重复执行结果一致。
#[tokio::test]
async fn test_grep_pagination_multi_file_is_stable() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["a.txt", "b.txt", "c.txt"] {
        let lines: Vec<String> = (0..4).map(|i| format!("{name}-needle-{i:02}")).collect();
        std::fs::write(dir.path().join(name), lines.join("\n")).unwrap();
    }
    let tool = GrepTool::new(dir.path().to_str().unwrap());

    // 12 条结果，window=offset 2 + head_limit 10 恰好覆盖全部已收集结果，
    // 避免并行 walker 早停导致文件集合不确定。
    let first = grep_page(&tool, 2, 10).await;
    let repeated = grep_page(&tool, 2, 10).await;
    let next = grep_page(&tool, 6, 10).await;
    assert_eq!(first, repeated, "同一窗口重复分页必须稳定");
    assert_ne!(first, next, "不同窗口必须返回不同结果");
    let lines: Vec<&str> = first.lines().collect();
    assert_eq!(lines.len(), 10, "{first}");
    assert_eq!(lines[0], "a.txt:3: a.txt-needle-02", "{first}");
    assert_eq!(lines[9], "c.txt:4: c.txt-needle-03", "{first}");
}
