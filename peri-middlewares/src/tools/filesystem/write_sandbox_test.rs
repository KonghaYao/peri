use super::WriteSandboxTool;
use peri_agent::tools::BaseTool;

fn make_tool(dir: &tempfile::TempDir, allowed: Vec<&str>) -> WriteSandboxTool {
    let cwd = dir.path().to_str().unwrap().to_string();
    // 先创建沙箱目录
    for d in &allowed {
        std::fs::create_dir_all(dir.path().join(d)).unwrap();
    }
    WriteSandboxTool::new(cwd, allowed.iter().map(|s| s.to_string()).collect()).unwrap()
}

#[tokio::test]
async fn test_write_sandbox_normal_create() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox"]);
    let result = tool
        .invoke(
            serde_json::json!({"path": "sandbox/hello.md", "content": "# Plan"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("Wrote 1 line"));
    let content = std::fs::read_to_string(dir.path().join("sandbox/hello.md")).unwrap();
    assert_eq!(content, "# Plan");
}

#[tokio::test]
async fn test_write_sandbox_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox"]);
    // 先写一次
    std::fs::write(dir.path().join("sandbox/v2.md"), "v1").unwrap();
    // 再覆盖写
    tool.invoke(
        serde_json::json!({"path": "sandbox/v2.md", "content": "v2"}),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();
    let content = std::fs::read_to_string(dir.path().join("sandbox/v2.md")).unwrap();
    assert_eq!(content, "v2");
}

#[tokio::test]
async fn test_write_sandbox_dotdot_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox"]);
    let result = tool
        .invoke(
            serde_json::json!({"path": "sandbox/../outside.txt", "content": "evil"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err(), ".. 穿越应被拒绝");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("sandbox/../outside.txt"),
        "错误消息应包含完整路径: {}",
        err
    );
}

#[tokio::test]
async fn test_write_sandbox_absolute_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox"]);
    let abs = dir.path().join("outside.txt");
    let result = tool
        .invoke(
            serde_json::json!({"path": abs.to_str().unwrap(), "content": "evil"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err(), "绝对路径应被拒绝");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("绝对"),
        "错误消息应说明拒绝原因: {}",
        err
    );
}

#[tokio::test]
async fn test_write_sandbox_outside_dir_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox"]);
    let result = tool
        .invoke(
            serde_json::json!({"path": "other/outside.txt", "content": "nope"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err(), "沙箱外路径应被拒绝");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("sandbox") || err.contains("沙箱"),
        "错误消息应提示沙箱限制: {}",
        err
    );
}

#[tokio::test]
async fn test_write_sandbox_symlink_escape_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox"]);
    // 在沙箱内创建指向外部文件的 symlink
    std::fs::write(dir.path().join("outside.txt"), "evil").unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            dir.path().join("outside.txt"),
            dir.path().join("sandbox/escape_link.txt"),
        )
        .unwrap();
    }
    #[cfg(not(unix))]
    {
        // Windows: symlink 测试跳过
        return;
    }
    let result = tool
        .invoke(
            serde_json::json!({"path": "sandbox/escape_link.txt", "content": "bypass"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err(), "symlink 逃逸应被拒绝");
}

#[tokio::test]
async fn test_write_sandbox_parent_symlink_escape_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox"]);
    // sandbox/sub 是外部目录的 symlink
    std::fs::create_dir_all(dir.path().join("outside_dir")).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            dir.path().join("outside_dir"),
            dir.path().join("sandbox/sub"),
        )
        .unwrap();
    }
    #[cfg(not(unix))]
    {
        return;
    }
    let result = tool
        .invoke(
            serde_json::json!({"path": "sandbox/sub/evil.txt", "content": "bypass"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err(), "父目录 symlink 逃逸应被拒绝");
}

#[test]
fn test_write_sandbox_description_contains_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox", "output"]);
    let desc = tool.description();
    assert!(desc.contains("sandbox"));
    assert!(desc.contains("output"));
    assert!(desc.contains("Write a file into your sandbox directories"));
}

#[test]
fn test_write_sandbox_empty_allowed_dirs_ok() {
    let cwd = tempfile::tempdir().unwrap();
    let result = WriteSandboxTool::new(
        cwd.path().to_str().unwrap().to_string(),
        vec![],
    );
    // 空白名单应可构造（不注入时不报错）
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_write_sandbox_multi_dir() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["plans", "output"]);
    tool.invoke(
        serde_json::json!({"path": "plans/design.md", "content": "# Design"}),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();
    tool.invoke(
        serde_json::json!({"path": "output/result.json", "content": "{\"ok\": true}"}),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();
    assert!(dir.path().join("plans/design.md").exists());
    assert!(dir.path().join("output/result.json").exists());
}
