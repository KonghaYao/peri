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
