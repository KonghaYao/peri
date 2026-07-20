use super::SandboxedWriteTool;
use peri_agent::tools::BaseTool;

fn make_tool(dir: &tempfile::TempDir, allowed: Vec<&str>) -> SandboxedWriteTool {
    let cwd = dir.path().to_str().unwrap().to_string();
    for d in &allowed {
        std::fs::create_dir_all(dir.path().join(d)).unwrap();
    }
    SandboxedWriteTool::new(cwd, allowed.iter().map(|s| s.to_string()).collect()).unwrap()
}

// ─── P0: name / description / parameters ────────────────────────────

#[test]
fn test_sandboxed_write_tool_name() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox"]);
    assert_eq!(tool.name(), "Write");
}

#[test]
fn test_sandboxed_write_tool_description_contains_restriction() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox", "output"]);
    let desc = tool.description();
    assert!(
        desc.contains("Restriction"),
        "desc should mention restriction: {}",
        desc
    );
    assert!(
        desc.contains("sandbox"),
        "desc should list sandbox dirs: {}",
        desc
    );
    assert!(
        desc.contains("output"),
        "desc should list output dir: {}",
        desc
    );
    assert!(
        desc.contains("atomic write"),
        "desc should include original Write description"
    );
}

#[test]
fn test_sandboxed_write_parameters_delegates_to_inner() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox"]);
    let params = tool.parameters();
    assert!(
        params["properties"]["append"].is_object(),
        "SandboxedWrite should include append param"
    );
    assert!(params["required"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v.as_str() == Some("file_path")));
}

// ─── P0: path validation ────────────────────────────────────────────

#[tokio::test]
async fn test_sandboxed_write_invoke_normal() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["plans"]);
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "plans/report.md", "content": "# Report"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("Wrote 1 line"), "unexpected: {}", result);
    let content = std::fs::read_to_string(dir.path().join("plans/report.md")).unwrap();
    assert_eq!(content, "# Report");
}

#[tokio::test]
async fn test_sandboxed_write_invoke_absolute_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox"]);
    let abs = dir.path().join("outside.txt");
    let result = tool
        .invoke(
            serde_json::json!({"file_path": abs.to_str().unwrap(), "content": "evil"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err(), "absolute path should be rejected");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("Absolute"),
        "error should mention absolute: {}",
        err
    );
}

#[tokio::test]
async fn test_sandboxed_write_invoke_traversal_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox"]);
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "sandbox/../outside.txt", "content": "evil"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err(), ".. traversal should be rejected");
}

#[tokio::test]
async fn test_sandboxed_write_invoke_outside_dir_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox"]);
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "other/outside.txt", "content": "nope"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err(), "path outside sandbox should be rejected");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("outside") || err.contains("allowed"),
        "error should indicate restriction: {}",
        err
    );
}

// ─── P1: append support ─────────────────────────────────────────────

#[tokio::test]
async fn test_sandboxed_write_append_mode() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["plans"]);
    // First write
    tool.invoke(
        serde_json::json!({"file_path": "plans/chunked.md", "content": "line1\n"}),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();
    // Append
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "plans/chunked.md", "content": "line2\n", "append": true}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("Appended"),
        "should use append mode: {}",
        result
    );
    let content = std::fs::read_to_string(dir.path().join("plans/chunked.md")).unwrap();
    assert_eq!(content, "line1\nline2\n");
}

// ─── P0: guard unit tests ───────────────────────────────────────────

#[test]
fn test_validate_sandbox_path_relative_ok() {
    use super::super::sandbox_guard::validate_sandbox_path;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sandbox")).unwrap();
    let sandbox_root = dir.path().join("sandbox").canonicalize().unwrap();
    let result = validate_sandbox_path(
        &dir.path().to_string_lossy(),
        "sandbox/hello.md",
        &[sandbox_root],
    );
    assert!(result.is_ok(), "relative path should pass: {:?}", result.err());
}

#[test]
fn test_validate_sandbox_path_absolute_rejected() {
    use super::super::sandbox_guard::{validate_sandbox_path, SandboxPathError};
    let result = validate_sandbox_path("/tmp", "/etc/passwd", &[]);
    assert!(result.is_err());
    match result.unwrap_err() {
        SandboxPathError::AbsolutePath { .. } => {}
        other => panic!("expected AbsolutePath, got: {:?}", other),
    }
}
