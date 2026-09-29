use super::*;

// === Task 4 新增测试 ===

#[tokio::test]
async fn test_grep_multiline() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("test.txt"), "foo\nbar\nbaz").unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({
                "pattern": "foo.*bar",
                "multiline": true,
                "output_mode": "content",
                "path": "./"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("foo"), "multiline 应匹配跨行模式: {result}");
}

#[tokio::test]
async fn test_grep_line_number_off() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("test.txt"), "needle here").unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({
                "pattern": "needle",
                "-n": false,
                "output_mode": "content",
                "path": "./"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    // line_number=false 格式为 "path: content"（无行号），不含 "path:num: content" 的双冒号模式
    assert!(
        !result.contains("test.txt:1:"),
        "line_number=false 时不应含行号: {result}"
    );
}

#[tokio::test]
async fn test_grep_whole_word() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("test.txt"), "test testing tested").unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    // whole_word=true 应只匹配独立单词 "test"
    let result_word = tool
        .invoke(
            serde_json::json!({
                "pattern": "test",
                "whole_word": true,
                "output_mode": "content",
                "path": "./"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result_word.contains("test testing tested"),
        "whole_word=true 应匹配包含独立 test 的行: {result_word}"
    );
    // whole_word=false 时同一行也应匹配
    let result_no_word = tool
        .invoke(
            serde_json::json!({
                "pattern": "test",
                "whole_word": false,
                "output_mode": "content",
                "path": "./"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result_no_word.contains("test testing tested"),
        "whole_word=false 也应匹配该行: {result_no_word}"
    );
}

#[tokio::test]
async fn test_grep_invert_match() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("test.txt"), "foo\nbar\nbaz\nfoo2").unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({
                "pattern": "foo",
                "invert_match": true,
                "output_mode": "content",
                "path": "./"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        !result.contains("foo"),
        "invert_match=true 不应输出匹配行: {result}"
    );
    assert!(
        result.contains("bar"),
        "invert_match=true 应输出不匹配行: {result}"
    );
}

#[tokio::test]
async fn test_grep_fixed_strings() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("test.txt"), "[ERROR] something\n[INFO] ok").unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({
                "pattern": "[ERROR]",
                "fixed_strings": true,
                "output_mode": "content",
                "path": "./"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("[ERROR]"),
        "fixed_strings=true 应匹配字面 [ERROR]: {result}"
    );
    assert!(
        !result.contains("[INFO]"),
        "fixed_strings=true 不应匹配 [INFO]: {result}"
    );
}

#[tokio::test]
async fn test_grep_asymmetric_context() {
    let dir = tempfile::tempdir().unwrap();
    let lines = [
        "line1 before\n",
        "line2 before\n",
        "needle match\n",
        "line4 after\n",
    ];
    std::fs::write(dir.path().join("test.txt"), lines.join("")).unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({
                "pattern": "needle",
                "-B": 2,
                "-A": 0,
                "output_mode": "content",
                "path": "./"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("line1 before"),
        "应包含前 2 行上下文: {result}"
    );
    assert!(
        result.contains("line2 before"),
        "应包含前 2 行上下文: {result}"
    );
}

#[tokio::test]
async fn test_grep_files_without_matches() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "needle here").unwrap();
    std::fs::write(dir.path().join("b.txt"), "no match here").unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({
                "pattern": "needle",
                "output_mode": "files_without_matches",
                "path": "./"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("b.txt"), "应列出无匹配的文件: {result}");
    assert!(!result.contains("a.txt"), "不应列出有匹配的文件: {result}");
}

#[tokio::test]
async fn test_grep_output_mode_default() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("test.txt"), "needle here").unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({
                "pattern": "needle",
                "path": "./"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("needle"),
        "不传 output_mode 时应默认为 content 模式: {result}"
    );
}
