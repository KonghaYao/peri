//! Tests for artifact_tool

use super::*;

#[test]
fn test_artifact_tool_name() {
    let tool = ArtifactTool::new("/tmp".into());
    assert_eq!(tool.name(), "artifact");
}

#[test]
fn test_artifact_tool_description() {
    let tool = ArtifactTool::new("/tmp".into());
    assert!(tool.description().contains("HTML"));
    assert!(tool.description().contains("Markdown"));
    assert!(tool.description().contains("public URL"));
}

#[test]
fn test_artifact_tool_parameters_schema() {
    let tool = ArtifactTool::new("/tmp".into());
    let params = tool.parameters();
    // file_path 必需，支持 .html/.htm/.md
    assert_eq!(params["properties"]["file_path"]["type"], "string");
    assert!(params["properties"]["file_path"]["description"]
        .as_str()
        .unwrap()
        .contains(".md"));
    assert!(params["required"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v.as_str() == Some("file_path")));
    // ttl 可选，默认 7d
    assert_eq!(params["properties"]["ttl"]["type"], "string");
    assert!(
        params["properties"]["ttl"]["enum"]
            .as_array()
            .unwrap()
            .len()
            >= 2
    );
}

#[test]
fn test_is_markdown() {
    assert!(ArtifactTool::is_markdown(std::path::Path::new("doc.md")));
    assert!(ArtifactTool::is_markdown(std::path::Path::new("DOC.MD")));
    assert!(!ArtifactTool::is_markdown(std::path::Path::new("doc.html")));
    assert!(!ArtifactTool::is_markdown(std::path::Path::new("doc.txt")));
}

#[test]
fn test_md_to_html_basic() {
    let md = "# Hello\n\nThis is **bold** and *italic*.";
    let html = ArtifactTool::md_to_html(md);
    // 应包含 HTML 文档结构
    assert!(html.contains("<!DOCTYPE html>"));
    assert!(html.contains("<h1>Hello</h1>"));
    assert!(html.contains("<strong>bold</strong>"));
    assert!(html.contains("<em>italic</em>"));
}

#[test]
fn test_md_to_html_table() {
    let md = "| A | B |\n| - | - |\n| 1 | 2 |";
    let html = ArtifactTool::md_to_html(md);
    assert!(html.contains("<table>"));
    assert!(html.contains("<th>A</th>"));
    assert!(html.contains("<td>2</td>"));
}

#[test]
fn test_md_to_html_code_block() {
    let md = "```rust\nlet x = 1;\n```";
    let html = ArtifactTool::md_to_html(md);
    assert!(html.contains("<pre>"));
    assert!(html.contains("<code"));
    assert!(html.contains("let x = 1;"));
}

#[tokio::test]
async fn test_invoke_file_not_found() {
    let tool = ArtifactTool::new("/tmp".into());
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "/nonexistent/file.html", "ttl": "7d"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("not found") || err.contains("exist"));
}

#[tokio::test]
async fn test_invoke_non_html_extension() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("test.txt");
    let mut f = std::fs::File::create(&file_path).unwrap();
    f.write_all(b"hello").unwrap();

    let tool = ArtifactTool::new(dir.path().to_string_lossy().to_string());
    let result = tool
        .invoke(
            serde_json::json!({"file_path": file_path.to_string_lossy(), "ttl": "7d"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("HTML") || err.contains("Markdown"));
}

#[tokio::test]
async fn test_invoke_file_too_large() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("large.html");
    let mut f = std::fs::File::create(&file_path).unwrap();
    // 写入超过 10MB 的数据
    let chunk = vec![b'a'; 1024 * 1024]; // 1MB
    for _ in 0..11 {
        f.write_all(&chunk).unwrap();
    }

    let tool = ArtifactTool::new(dir.path().to_string_lossy().to_string());
    let result = tool
        .invoke(
            serde_json::json!({"file_path": file_path.to_string_lossy(), "ttl": "7d"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("too large") || err.contains("10MB") || err.contains("exceeds"));
}

#[test]
fn test_validate_md_extension_allowed() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("readme.md");
    let mut f = std::fs::File::create(&file_path).unwrap();
    f.write_all(b"# Hello").unwrap();

    let tool = ArtifactTool::new(dir.path().to_string_lossy().to_string());
    // .md 扩展名应通过校验
    let result = tool.validate_file(&file_path);
    assert!(
        result.is_ok(),
        ".md 文件应通过扩展名校验，实际: {:?}",
        result.err()
    );
}

#[test]
fn test_validate_html_extension_allowed() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("index.html");
    let mut f = std::fs::File::create(&file_path).unwrap();
    f.write_all(b"<html></html>").unwrap();

    let tool = ArtifactTool::new(dir.path().to_string_lossy().to_string());
    let result = tool.validate_file(&file_path);
    assert!(result.is_ok(), ".html 文件应通过扩展名校验");

    // .htm 也应该通过
    let file_path2 = dir.path().join("index.htm");
    let mut f2 = std::fs::File::create(&file_path2).unwrap();
    f2.write_all(b"<html></html>").unwrap();
    let result2 = tool.validate_file(&file_path2);
    assert!(result2.is_ok(), ".htm 文件应通过扩展名校验");
}
