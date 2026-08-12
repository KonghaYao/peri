use super::*;

#[tokio::test]
async fn test_folder_create() {
    let dir = tempfile::tempdir().unwrap();
    let tool = FolderOperationsTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"operation": "create", "folder_path": "newdir"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("created successfully"),
        "unexpected: {result}"
    );
    assert!(dir.path().join("newdir").is_dir());
}

#[tokio::test]
async fn test_folder_create_recursive() {
    let dir = tempfile::tempdir().unwrap();
    let tool = FolderOperationsTool::new(dir.path().to_str().unwrap());
    tool.invoke(
        serde_json::json!({"operation": "create", "folder_path": "a/b/c"}),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();
    assert!(dir.path().join("a/b/c").is_dir());
}

#[tokio::test]
async fn test_folder_exists_true() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("existing")).unwrap();
    let tool = FolderOperationsTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"operation": "exists", "folder_path": "existing"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("Folder exists"),
        "should report exists: {result}"
    );
}

#[tokio::test]
async fn test_folder_exists_false() {
    let dir = tempfile::tempdir().unwrap();
    let tool = FolderOperationsTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"operation": "exists", "folder_path": "ghost"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("does not exist"),
        "should report missing: {result}"
    );
}

#[tokio::test]
async fn test_folder_list() {
    let dir = tempfile::tempdir().unwrap();
    let subdir = dir.path().join("listed");
    std::fs::create_dir(&subdir).unwrap();
    std::fs::write(subdir.join("file.txt"), "hello").unwrap();
    let tool = FolderOperationsTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"operation": "list", "folder_path": "listed"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("file.txt"),
        "should list file.txt: {result}"
    );
}

#[tokio::test]
async fn test_folder_list_truncation_keeps_files() {
    let dir = tempfile::tempdir().unwrap();
    let subdir = dir.path().join("bigdir");
    std::fs::create_dir(&subdir).unwrap();
    // 创建超过 MAX_LIST_ENTRIES 的子目录
    for i in 0..600 {
        std::fs::create_dir(subdir.join(format!("d{}", i))).unwrap();
    }
    // 同时创建一些文件
    for i in 0..5 {
        std::fs::write(subdir.join(format!("f{}.txt", i)), "x").unwrap();
    }
    let tool = FolderOperationsTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"operation": "list", "folder_path": "bigdir"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    // 文件不应被全部丢弃
    assert!(
        result.contains("f0.txt") || result.contains("f1.txt"),
        "截断后应保留部分文件: {result}"
    );
    assert!(result.contains("truncated"), "应显示截断提示: {result}");
}

#[tokio::test]
async fn test_folder_list_truncation_persists_full_output() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..510 {
        std::fs::write(dir.path().join(format!("file_{}.txt", i)), "").unwrap();
    }
    let tool = FolderOperationsTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({
                "operation": "list",
                "folder_path": dir.path().to_str().unwrap()
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("Output truncated"),
        "应显示截断信息: {result}"
    );
    assert!(
        result.contains("Read tool"),
        "应包含 Read tool 提示: {result}"
    );
    assert!(
        result.contains("peri-tool-output-"),
        "应包含文件路径: {result}"
    );
}

#[test]
fn test_description_extended() {
    let tool = FolderOperationsTool::new("/tmp");
    let desc = tool.description();
    assert!(
        desc.contains("create") && desc.contains("list") && desc.contains("exists"),
        "description 应提及三种操作"
    );
    assert!(desc.len() > 200, "description 应为扩展后的多段落文本");
}

#[tokio::test]
async fn test_deep_scan_depth_1() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("README.md"), "").unwrap();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(root.join("src").join("main.rs"), "").unwrap();
    let tool = FolderOperationsTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"operation": "deep_scan", "folder_path": "proj", "max_depth": 1}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("README.md"),
        "should show root file: {result}"
    );
    assert!(result.contains("src"), "should show root dir: {result}");
    // depth=1 -> 不应进入 src/
    assert!(
        !result.contains("main.rs"),
        "depth=1 should not show nested files: {result}"
    );
}

#[tokio::test]
async fn test_deep_scan_depth_2() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("app");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("tests")).unwrap();
    std::fs::write(root.join("Cargo.toml"), "").unwrap();
    std::fs::write(root.join("src").join("main.rs"), "").unwrap();
    std::fs::write(root.join("tests").join("test.rs"), "").unwrap();
    let tool = FolderOperationsTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"operation": "deep_scan", "folder_path": "app", "max_depth": 2}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("Cargo.toml"), "should show root: {result}");
    assert!(
        result.contains("main.rs"),
        "depth=2 should show nested: {result}"
    );
    assert!(
        result.contains("test.rs"),
        "should show other subdir: {result}"
    );
}

#[tokio::test]
async fn test_deep_scan_skips_ignored_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("pkg");
    std::fs::create_dir_all(root.join("node_modules").join("lodash")).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("package.json"), "").unwrap();
    std::fs::write(
        root.join("node_modules").join("lodash").join("index.js"),
        "",
    )
    .unwrap();
    std::fs::write(root.join("src").join("app.ts"), "").unwrap();
    let tool = FolderOperationsTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"operation": "deep_scan", "folder_path": "pkg", "max_depth": 3}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("package.json"),
        "should show root: {result}"
    );
    assert!(result.contains("app.ts"), "should show src: {result}");
    assert!(
        !result.contains("lodash") && !result.contains("index.js"),
        "should skip node_modules contents: {result}"
    );
}

#[tokio::test]
async fn test_deep_scan_nonexistent_folder() {
    let dir = tempfile::tempdir().unwrap();
    let tool = FolderOperationsTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"operation": "deep_scan", "folder_path": "ghost_dir"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("Folder not found"),
        "should report missing: {err_msg}"
    );
}

/// 浮点 max_depth 必须显式报错，不得被 as_u64() 静默吞掉回退默认值 3
#[tokio::test]
async fn test_deep_scan_fractional_max_depth_rejected() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("scanme")).unwrap();
    let tool = FolderOperationsTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"operation": "deep_scan", "folder_path": "scanme", "max_depth": 2.5}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("non-negative integer"),
        "浮点 max_depth 应报错而非静默回退: {err_msg}"
    );
}
