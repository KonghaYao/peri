use super::*;

// ─── P1-3: 语义化别名兼容 ───────────────────────────────────────────────────

/// 语义化别名（case_insensitive）应能驱动大小写不敏感搜索
#[tokio::test]
async fn test_invoke_accepts_semantic_aliases() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("test.txt"), "NEEDLE\nneedle\nNeedle").unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({
                "pattern": "NEEDLE",
                "output_mode": "content",
                "case_insensitive": true,
                "show_line_numbers": false,
                "path": "./"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("NEEDLE"), "应匹配大写: {result}");
    assert!(result.contains("needle"), "应匹配小写: {result}");
    assert!(result.contains("Needle"), "应匹配混合大小写: {result}");
}

/// 旧 CLI 风格参数（-i/-A/-B/-C/-n）必须仍然可解析（向后兼容）
#[tokio::test]
async fn test_invoke_still_accepts_cli_style_params() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("test.txt"),
        "line1\nNEEDLE\nline3\nneedle\nline5",
    )
    .unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({
                "pattern": "NEEDLE",
                "output_mode": "content",
                "-i": true,
                "-C": 1,
                "-n": true,
                "path": "./"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    // 上下文行应被包含
    assert!(result.contains("line3"), "-C=1 应输出上下文行: {result}");
    assert!(result.contains("needle"), "-i 应匹配小写: {result}");
}

/// 语义化别名与 CLI 风格同时存在时，语义化别名优先（按 invoke 中 or_else 顺序）
#[tokio::test]
async fn test_invoke_semantic_alias_takes_priority_over_cli() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("test.txt"), "needle1\nNEEDLE2\nneedle3").unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    // 两个 key 都给值，case_insensitive=false 应优先（不进行大小写不敏感搜索）
    let result = tool
        .invoke(
            serde_json::json!({
                "pattern": "NEEDLE",
                "output_mode": "content",
                "case_insensitive": false,
                "-i": true,
                "path": "./"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("NEEDLE2"), "应仍能匹配原大小写: {result}");
    // 由于 case_insensitive=false 优先，不应有 needle1/needle3
    assert!(
        !result.contains("needle1"),
        "case_insensitive=false 应优先于 -i=true，不应匹配 needle1: {result}"
    );
}

/// 语义化别名 context 应等同于 -C
#[tokio::test]
async fn test_invoke_context_alias() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("test.txt"), "before\nNEEDLE\nafter\nafter2").unwrap();
    let tool = GrepTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({
                "pattern": "NEEDLE",
                "output_mode": "content",
                "context": 2,
                "path": "./"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("before"),
        "context=2 应输出前 2 行: {result}"
    );
    assert!(
        result.contains("after2"),
        "context=2 应输出后 2 行: {result}"
    );
}
