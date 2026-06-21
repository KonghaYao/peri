// 测试通过 include! 嵌入 artifact_tool.rs 的 #[cfg(test)] mod tests 块，
// tests 块有 `use super::*;`，因此 ArtifactTool 等类型已自动导入。

#[test]
fn test_artifact_tool_name() {
    let tool = ArtifactTool::new("/tmp".into());
    assert_eq!(tool.name(), "artifact");
}

#[test]
fn test_artifact_tool_description() {
    let tool = ArtifactTool::new("/tmp".into());
    assert!(tool.description().contains("HTML"));
    assert!(tool.description().contains("public URL"));
}

#[test]
fn test_artifact_tool_parameters_schema() {
    let tool = ArtifactTool::new("/tmp".into());
    let params = tool.parameters();
    // file_path 必需
    assert_eq!(params["properties"]["file_path"]["type"], "string");
    assert!(params["required"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v.as_str() == Some("file_path")));
    // ttl 可选，默认 7d
    assert_eq!(params["properties"]["ttl"]["type"], "string");
    assert!(params["properties"]["ttl"]["enum"]
        .as_array()
        .unwrap()
        .len()
        >= 2);
}

#[tokio::test]
async fn test_invoke_file_not_found() {
    let tool = ArtifactTool::new("/tmp".into());
    let result = tool
        .invoke(serde_json::json!({"file_path": "/nonexistent/file.html", "ttl": "7d"}), peri_agent::tools::ToolContext::new(&[], "."))
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
        .invoke(serde_json::json!({"file_path": file_path.to_string_lossy(), "ttl": "7d"}), peri_agent::tools::ToolContext::new(&[], "."))
        .await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("HTML"));
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
        .invoke(serde_json::json!({"file_path": file_path.to_string_lossy(), "ttl": "7d"}), peri_agent::tools::ToolContext::new(&[], "."))
        .await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("too large") || err.contains("10MB") || err.contains("exceeds"));
}
