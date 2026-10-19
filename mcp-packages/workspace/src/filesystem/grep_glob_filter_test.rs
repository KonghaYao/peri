//! `glob` 过滤器的用户可见契约：工具描述推荐的 `*.{ts,tsx}` 必须可用，
//! 非法 glob 必须报错而不是静默放宽成「搜索全部文件」。
use super::*;

/// 夹具：三种扩展名、同一标记，用于区分 glob 是否真的生效。
fn write_ext_fixture(dir: &std::path::Path) {
    std::fs::write(dir.join("a.ts"), "NEEDLE ts").unwrap();
    std::fs::write(dir.join("b.tsx"), "NEEDLE tsx").unwrap();
    std::fs::write(dir.join("c.js"), "NEEDLE js").unwrap();
}

async fn grep_files(dir: &std::path::Path, glob: &str) -> Result<String, String> {
    let tool = GrepTool::new(dir.to_str().unwrap());
    tool.invoke(
        serde_json::json!({
            "pattern": "NEEDLE",
            "glob": glob,
            "output_mode": "files_with_matches",
            "path": "./"
        }),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .map_err(|error| error.to_string())
}

/// 工具描述推荐的 brace 交替（`*.{ts,tsx}`）应展开为多个 pattern，而不是按字面匹配。
#[tokio::test]
async fn test_grep_glob_brace_alternation_matches_every_alternative() {
    let dir = tempfile::tempdir().unwrap();
    write_ext_fixture(dir.path());
    let result = grep_files(dir.path(), "*.{ts,tsx}").await.unwrap();
    assert!(result.contains("a.ts"), "应命中 .ts 文件: {result}");
    assert!(result.contains("b.tsx"), "应命中 .tsx 文件: {result}");
    assert!(!result.contains("c.js"), "不应命中 .js 文件: {result}");
}

/// 单扩展名 glob 是既有行为，不得被交替展开改写。
#[tokio::test]
async fn test_grep_glob_single_extension_still_filters() {
    let dir = tempfile::tempdir().unwrap();
    write_ext_fixture(dir.path());
    let result = grep_files(dir.path(), "*.ts").await.unwrap();
    assert!(result.contains("a.ts"), "应命中 a.ts: {result}");
    assert!(!result.contains("b.tsx"), "不应命中 b.tsx: {result}");
    assert!(!result.contains("c.js"), "不应命中 c.js: {result}");
}

/// 非法 glob（字符类未闭合）必须报错：静默丢弃过滤器会把「搜到 0 个」伪装成
/// 「过滤后没有匹配」，让调用方以为过滤生效。
#[tokio::test]
async fn test_grep_unclosed_bracket_glob_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    write_ext_fixture(dir.path());
    let error = grep_files(dir.path(), "[unclosed").await.unwrap_err();
    assert!(error.contains("[unclosed"), "诊断应指出非法 glob: {error}");
}

/// 非法 glob（交替未闭合）必须报错，不能退化成字面匹配返回空结果。
#[tokio::test]
async fn test_grep_unclosed_brace_glob_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    write_ext_fixture(dir.path());
    let error = grep_files(dir.path(), "*.{ts,tsx").await.unwrap_err();
    assert!(error.contains("*.{ts,tsx"), "诊断应指出非法 glob: {error}");
}

/// 合法 glob 没有命中文件 ≠ 非法 glob：仍是正常空结果，不是工具错误。
#[tokio::test]
async fn test_grep_valid_glob_without_match_reports_no_matches() {
    let dir = tempfile::tempdir().unwrap();
    write_ext_fixture(dir.path());
    let result = grep_files(dir.path(), "*.go").await.unwrap();
    assert!(result.contains("No matches found"), "应是空结果: {result}");
}
