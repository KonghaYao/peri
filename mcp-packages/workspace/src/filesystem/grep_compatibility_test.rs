use super::*;

// === Task 5: multi_line 兼容性验证 ===

#[tokio::test]
async fn test_grep_multiline_with_invert_match() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("test.txt"), "foo\nbar\nbaz").unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    // multi_line + invert_match: 跨行模式匹配 foo.*baz，反转后应输出不包含跨行匹配的文件
    let result = tool
        .invoke(
            serde_json::json!({
                "pattern": "foo.*baz",
                "multiline": true,
                "invert_match": true,
                "output_mode": "content",
                "path": "./"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    // foo.*baz 跨行匹配整个文件内容，反转后应为空
    assert!(
        result.contains("No matches found"),
        "multi_line + invert_match: 跨行匹配整个文件后反转应无结果: {result}"
    );
}

#[tokio::test]
async fn test_grep_multiline_with_context() {
    let dir = tempfile::tempdir().unwrap();
    let lines = ["before1\n", "START\n", "middle\n", "END\n", "after1\n"];
    std::fs::write(dir.path().join("test.txt"), lines.join("")).unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({
                "pattern": "START.*END",
                "multiline": true,
                "-A": 1,
                "output_mode": "content",
                "path": "./"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("START"),
        "multi_line + context: 应包含 START: {result}"
    );
    assert!(
        result.contains("END"),
        "multi_line + context: 应包含 END: {result}"
    );
}

#[tokio::test]
async fn test_grep_max_depth() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("root.txt"), "needle").unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("deep.txt"), "needle").unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({
                "pattern": "needle",
                "max_depth": 1,
                "output_mode": "files_with_matches",
                "path": "./"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("root.txt"),
        "max_depth=1 应找到根目录文件: {result}"
    );
    assert!(
        !result.contains("deep.txt"),
        "max_depth=1 不应找到子目录文件: {result}"
    );
}

#[tokio::test]
async fn test_grep_truncation_persists_full_output() {
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
                "head_limit": 3
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("truncated at 3 lines"),
        "应显示截断信息: {result}"
    );
    // 落盘提示必须用**模型面名字**引导读取（当前 direct 工具使用原名）：逐字断言注册表的冻
    // 结字面量，不用查表派生期望值（同源派生会让「查询改坏」自洽通过）。
    assert!(
        result.contains("use `Read` to view complete content"),
        "应包含模型面名字的读取提示: {result}"
    );
    assert!(
        result.contains("peri-tool-output-"),
        "应包含文件路径: {result}"
    );
}
