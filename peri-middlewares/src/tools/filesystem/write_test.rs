//! Tests for write

use super::*;

/// 从错误消息中提取 draft_id(消息中 draft 信息在开头,hint 内可能出现两次)
/// 仅被 #[cfg(unix)] 测试使用,需同步 cfg 以免非 unix 平台报 dead_code
#[cfg(unix)]
fn extract_draft_id(err: &str) -> String {
    let re = regex::Regex::new(r"draft_[0-9a-f-]+").unwrap();
    re.find(err).unwrap().as_str().to_string()
}

/// 将目录权限改为只读(0o444),用于注入 tmp 写入失败
#[cfg(unix)]
fn make_readonly(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444)).unwrap();
}

/// 将目录权限还原为可写(0o755)
#[cfg(unix)]
fn make_writable(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[tokio::test]
async fn test_write_file_creates_new() {
    let dir = tempfile::tempdir().unwrap();
    let tool = WriteFileTool::new(dir.path().to_str().unwrap());
    tool.invoke(
        serde_json::json!({"file_path": "new.txt", "content": "hello"}),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();
    let content = std::fs::read_to_string(dir.path().join("new.txt")).unwrap();
    assert_eq!(content, "hello");
}

#[tokio::test]
async fn test_write_file_overwrites_existing() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "old").unwrap();
    let tool = WriteFileTool::new(dir.path().to_str().unwrap());
    tool.invoke(
        serde_json::json!({"file_path": "f.txt", "content": "new"}),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();
    let content = std::fs::read_to_string(dir.path().join("f.txt")).unwrap();
    assert_eq!(content, "new");
}

#[tokio::test]
async fn test_write_file_creates_parent_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let tool = WriteFileTool::new(dir.path().to_str().unwrap());
    tool.invoke(
        serde_json::json!({"file_path": "sub/dir/file.txt", "content": "deep"}),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();
    assert!(dir.path().join("sub/dir/file.txt").exists());
}

#[tokio::test]
async fn test_write_file_missing_content_param() {
    let dir = tempfile::tempdir().unwrap();
    let tool = WriteFileTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "f.txt"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err(), "missing content should return Err");
}

#[tokio::test]
async fn test_write_file_success_message() {
    let dir = tempfile::tempdir().unwrap();
    let tool = WriteFileTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "msg.txt", "content": "x"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("Wrote 1 line"),
        "unexpected message: {result}"
    );
}

#[tokio::test]
async fn test_write_file_multiline_message() {
    let dir = tempfile::tempdir().unwrap();
    let tool = WriteFileTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "multi.txt", "content": "a\nb\nc"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("Wrote 3 lines"),
        "unexpected message: {result}"
    );
}

#[tokio::test]
async fn test_write_file_no_tmp_residual() {
    let dir = tempfile::tempdir().unwrap();
    let tool = WriteFileTool::new(dir.path().to_str().unwrap());
    tool.invoke(
        serde_json::json!({"file_path": "clean.txt", "content": "data"}),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();
    // 原子写入后不应残留任何 .tmp.* 临时文件
    let tmp_files: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("clean.tmp."))
        .collect();
    assert!(tmp_files.is_empty(), "临时文件应在 rename 后被清除");
    assert!(dir.path().join("clean.txt").exists());
}

#[tokio::test]
async fn test_write_file_error_propagates() {
    let dir = tempfile::tempdir().unwrap();
    // 在只读目录上写入应返回 Err
    let readonly_dir = dir.path().join("readonly");
    std::fs::create_dir(&readonly_dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&readonly_dir, std::fs::Permissions::from_mode(0o444)).unwrap();
    }
    let tool = WriteFileTool::new(readonly_dir.to_str().unwrap());
    let _result = tool
        .invoke(
            serde_json::json!({"file_path": "sub/nope.txt", "content": "x"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    #[cfg(unix)]
    assert!(_result.is_err(), "写入只读目录应返回 Err");
}

#[test]
fn test_description_extended() {
    let tool = WriteFileTool::new("/tmp");
    let desc = tool.description();
    assert!(desc.contains("Usage:"), "description 应包含 Usage 段落");
    assert!(desc.contains("atomic write"), "description 应提及原子写入");
    assert!(desc.len() > 200, "description 应为扩展后的多段落文本");
}

#[test]
#[allow(non_snake_case)]
fn test_tool_name_is_Write() {
    let tool = WriteFileTool::new("/tmp");
    assert_eq!(tool.name(), "Write");
}

#[tokio::test]
async fn test_write_append_to_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("log.txt"), "line1\n").unwrap();
    let tool = WriteFileTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "log.txt", "content": "line2\n", "append": true}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    let content = std::fs::read_to_string(dir.path().join("log.txt")).unwrap();
    assert_eq!(content, "line1\nline2\n");
    assert!(
        result.contains("Appended 1 line"),
        "unexpected message: {result}"
    );
    assert!(
        result.contains("file total: 2 lines"),
        "应包含总行数: {result}"
    );
}

#[tokio::test]
async fn test_write_append_creates_new_file() {
    let dir = tempfile::tempdir().unwrap();
    let tool = WriteFileTool::new(dir.path().to_str().unwrap());
    tool.invoke(serde_json::json!({"file_path": "new_append.txt", "content": "first line\n", "append": true}), peri_agent::tools::ToolContext::new(&[], "."))
        .await
        .unwrap();
    let content = std::fs::read_to_string(dir.path().join("new_append.txt")).unwrap();
    assert_eq!(content, "first line\n");
}

#[tokio::test]
async fn test_write_append_multiline() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "a\n").unwrap();
    let tool = WriteFileTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "f.txt", "content": "b\nc\nd\n", "append": true}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    let content = std::fs::read_to_string(dir.path().join("f.txt")).unwrap();
    assert_eq!(content, "a\nb\nc\nd\n");
    assert!(
        result.contains("Appended 3 lines"),
        "unexpected message: {result}"
    );
    assert!(
        result.contains("file total: 4 lines"),
        "应包含总行数: {result}"
    );
}

#[tokio::test]
async fn test_write_append_false_overwrites() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "old content").unwrap();
    let tool = WriteFileTool::new(dir.path().to_str().unwrap());
    tool.invoke(
        serde_json::json!({"file_path": "f.txt", "content": "new", "append": false}),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();
    let content = std::fs::read_to_string(dir.path().join("f.txt")).unwrap();
    assert_eq!(content, "new", "append=false 应覆写文件");
}

#[tokio::test]
async fn test_write_append_sequential_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let tool = WriteFileTool::new(dir.path().to_str().unwrap());
    tool.invoke(
        serde_json::json!({"file_path": "chunked.txt", "content": "chunk1\n"}),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();
    tool.invoke(
        serde_json::json!({"file_path": "chunked.txt", "content": "chunk2\n", "append": true}),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();
    tool.invoke(
        serde_json::json!({"file_path": "chunked.txt", "content": "chunk3\n", "append": true}),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();
    let content = std::fs::read_to_string(dir.path().join("chunked.txt")).unwrap();
    assert_eq!(content, "chunk1\nchunk2\nchunk3\n");
}

#[tokio::test]
async fn test_write_append_creates_parent_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let tool = WriteFileTool::new(dir.path().to_str().unwrap());
    tool.invoke(
        serde_json::json!({"file_path": "sub/dir/file.txt", "content": "deep\n", "append": true}),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();
    let content = std::fs::read_to_string(dir.path().join("sub/dir/file.txt")).unwrap();
    assert_eq!(content, "deep\n");
}

// ===== 失败草稿恢复机制测试(决策 6) =====

#[cfg(unix)]
#[tokio::test]
async fn test_write_tmp_failure_saves_draft() {
    let dir = tempfile::tempdir().unwrap();
    let readonly = dir.path().join("readonly");
    std::fs::create_dir(&readonly).unwrap();
    make_readonly(&readonly);
    let tool = WriteFileTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "readonly/f.txt", "content": "hello\nworld"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let err = result.unwrap_err().to_string();
    assert!(err.contains("A draft was saved"), "应含草稿提示: {err}");
    assert!(err.contains("draft_"), "应含 draft_id: {err}");
    assert!(err.contains("2 lines"), "应含行数: {err}");
    assert!(err.contains("11 bytes"), "应含字节数: {err}");
    assert!(!err.contains("hello"), "错误消息不应展示正文: {err}");
}

#[cfg(unix)]
#[tokio::test]
async fn test_write_create_dir_failure_saves_draft() {
    let dir = tempfile::tempdir().unwrap();
    let readonly = dir.path().join("readonly");
    std::fs::create_dir(&readonly).unwrap();
    make_readonly(&readonly);
    let tool = WriteFileTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "readonly/sub/f.txt", "content": "hello\nworld"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("Error creating parent directory"),
        "应含目录创建错误: {err}"
    );
    assert!(err.contains("A draft was saved"), "应含草稿提示: {err}");
    assert!(err.contains("draft_"), "应含 draft_id: {err}");
    assert!(!err.contains("hello"), "错误消息不应展示正文: {err}");
    // 还原权限后 from_draft 恢复成功,内容 == content 原文
    make_writable(&readonly);
    let draft_id = extract_draft_id(&err);
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "readonly/sub/f.txt", "from_draft": draft_id}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_ok(), "恢复应成功: {:?}", result.err());
    let content = std::fs::read_to_string(dir.path().join("readonly/sub/f.txt")).unwrap();
    assert_eq!(content, "hello\nworld", "草稿内容应为 content 原文");
}

#[cfg(unix)]
#[tokio::test]
async fn test_write_rename_failure_saves_draft_and_cleans_tmp() {
    let dir = tempfile::tempdir().unwrap();
    // file_path 指向已存在目录 → tmp 写入成功但 rename 失败(macOS EISDIR)
    std::fs::create_dir(dir.path().join("d")).unwrap();
    let tool = WriteFileTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "d", "content": "x\ny"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let err = result.unwrap_err().to_string();
    assert!(err.contains("draft_"), "rename 失败应存草稿: {err}");
    // 无 d.tmp.* 残留
    let tmp_files: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("d.tmp."))
        .collect();
    assert!(
        tmp_files.is_empty(),
        "tmp 文件应在存草稿后被清理: {:?}",
        tmp_files
    );
    // 移除阻碍目录后 from_draft 恢复成功,内容 == tmp 实际文本
    std::fs::remove_dir(dir.path().join("d")).unwrap();
    let draft_id = extract_draft_id(&err);
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "d", "from_draft": draft_id}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_ok(), "恢复应成功: {:?}", result.err());
    let content = std::fs::read_to_string(dir.path().join("d")).unwrap();
    assert_eq!(content, "x\ny", "草稿内容应为 tmp 实际文本");
}

#[cfg(unix)]
#[tokio::test]
async fn test_write_append_open_failure_saves_draft() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("log.txt"), "keep\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        dir.path().join("log.txt"),
        std::fs::Permissions::from_mode(0o444),
    )
    .unwrap();
    let tool = WriteFileTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "log.txt", "content": "appended\n", "append": true}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let err = result.unwrap_err().to_string();
    let draft_id = extract_draft_id(&err);
    assert!(
        err.contains("Error opening file for append"),
        "错误前缀: {err}"
    );
    // 还原权限后 from_draft 恢复,append 语义保留(非覆盖)
    std::fs::set_permissions(
        dir.path().join("log.txt"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "log.txt", "from_draft": draft_id}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_ok(), "恢复应成功: {:?}", result.err());
    let content = std::fs::read_to_string(dir.path().join("log.txt")).unwrap();
    assert_eq!(content, "keep\nappended\n", "append 语义应保留,非覆盖");
}

#[cfg(unix)]
#[tokio::test]
async fn test_write_from_draft_restores_after_failure() {
    let dir = tempfile::tempdir().unwrap();
    let readonly = dir.path().join("readonly");
    std::fs::create_dir(&readonly).unwrap();
    make_readonly(&readonly);
    let tool = WriteFileTool::new(dir.path().to_str().unwrap());
    let err = tool
        .invoke(
            serde_json::json!({"file_path": "readonly/f.txt", "content": "hello\nworld"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    let draft_id = extract_draft_id(&err);
    // 还原权限后 from_draft 恢复
    make_writable(&readonly);
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "readonly/f.txt", "from_draft": draft_id}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("Wrote 2 lines"), "恢复消息: {result}");
    let content = std::fs::read_to_string(dir.path().join("readonly/f.txt")).unwrap();
    assert_eq!(content, "hello\nworld");
}

#[tokio::test]
async fn test_write_requires_content_or_from_draft() {
    let dir = tempfile::tempdir().unwrap();
    let tool = WriteFileTool::new(dir.path().to_str().unwrap());
    let err = tool
        .invoke(
            serde_json::json!({"file_path": "f.txt"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("Either 'content' or 'from_draft' must be provided"),
        "缺参数文案: {err}"
    );
}

/// 回归（互斥劫持）：LLM 同时携带 content 与 from_draft 时，content 优先、
/// 直接写入成功（原「互斥报错」会导致文件无法落盘、模型被迫重输出一遍内容）。
#[tokio::test]
async fn test_write_content_and_from_draft_mutually_exclusive() {
    let dir = tempfile::tempdir().unwrap();
    let tool = WriteFileTool::new(dir.path().to_str().unwrap());
    let result = tool
        .invoke(
            serde_json::json!({
                "file_path": "f.txt",
                "content": "x",
                "from_draft": "draft_00000000-0000-7000-0000-000000000000"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("x"), "应以 content 优先写入: {result}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
        "x",
        "文件应落盘 content 内容"
    );
}

/// 回归（占位符）：from_draft 填占位符（"" / "__omit__"）等同未提供，content 生效
#[tokio::test]
async fn test_write_from_draft_placeholder_uses_content() {
    for placeholder in ["", "__omit__"] {
        let dir = tempfile::tempdir().unwrap();
        let tool = WriteFileTool::new(dir.path().to_str().unwrap());
        let result = tool
            .invoke(
                serde_json::json!({
                    "file_path": "f.txt",
                    "content": "real",
                    "from_draft": placeholder,
                }),
                peri_agent::tools::ToolContext::new(&[], "."),
            )
            .await
            .unwrap();
        assert!(
            result.contains("Wrote 1 line"),
            "占位符 from_draft {:?} 应等同未提供并写入成功: {}",
            placeholder,
            result
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "real",
            "占位符 from_draft {:?} 时文件应落盘 content 内容",
            placeholder
        );
    }
}

/// content 为占位符、from_draft 也未提供 → 报缺参数（不把占位符当正文写入）
#[tokio::test]
async fn test_write_content_placeholder_without_from_draft_errors() {
    let dir = tempfile::tempdir().unwrap();
    let tool = WriteFileTool::new(dir.path().to_str().unwrap());
    let err = tool
        .invoke(
            serde_json::json!({"file_path": "f.txt", "content": "__omit__"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("Either 'content' or 'from_draft' must be provided"),
        "缺参数文案: {err}"
    );
    assert!(
        !dir.path().join("f.txt").exists(),
        "不应把占位符当正文写入文件"
    );
}

#[tokio::test]
async fn test_write_from_draft_unknown_degrades_gracefully() {
    let dir = tempfile::tempdir().unwrap();
    let tool = WriteFileTool::new(dir.path().to_str().unwrap());
    let err = tool
        .invoke(
            serde_json::json!({
                "file_path": "f.txt",
                "from_draft": "draft_00000000-0000-7000-0000-000000000000"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("unknown or no longer available"),
        "未知草稿降级文案: {err}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_write_from_draft_wrong_target_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let readonly = dir.path().join("readonly");
    std::fs::create_dir(&readonly).unwrap();
    make_readonly(&readonly);
    let tool = WriteFileTool::new(dir.path().to_str().unwrap());
    let err = tool
        .invoke(
            serde_json::json!({"file_path": "readonly/a.txt", "content": "payload"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    let draft_id = extract_draft_id(&err);
    // 用错误的 file_path 恢复 → 拒绝,且不产生文件
    let err = tool
        .invoke(
            serde_json::json!({"file_path": "b.txt", "from_draft": draft_id}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("different file_path"), "路径不符文案: {err}");
    assert!(!dir.path().join("b.txt").exists(), "B 不应产生文件");
    // 草稿未被消费,原路径仍可恢复
    make_writable(&readonly);
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "readonly/a.txt", "from_draft": draft_id}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_ok(), "原路径应仍可恢复: {:?}", result.err());
    let content = std::fs::read_to_string(dir.path().join("readonly/a.txt")).unwrap();
    assert_eq!(content, "payload");
}

#[cfg(unix)]
#[tokio::test]
async fn test_write_success_clears_draft() {
    let dir = tempfile::tempdir().unwrap();
    let readonly = dir.path().join("readonly");
    std::fs::create_dir(&readonly).unwrap();
    make_readonly(&readonly);
    let tool = WriteFileTool::new(dir.path().to_str().unwrap());
    let err = tool
        .invoke(
            serde_json::json!({"file_path": "readonly/a.txt", "content": "payload"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    let draft_id = extract_draft_id(&err);
    // 成功写入同 target 后草稿被清除
    make_writable(&readonly);
    tool.invoke(
        serde_json::json!({"file_path": "readonly/a.txt", "content": "success"}),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();
    let err = tool
        .invoke(
            serde_json::json!({"file_path": "readonly/a.txt", "from_draft": draft_id}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("unknown or no longer available"),
        "成功写入后旧草稿应失效: {err}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_write_draft_overwrite_invalidates_old_id() {
    let dir = tempfile::tempdir().unwrap();
    let readonly = dir.path().join("readonly");
    std::fs::create_dir(&readonly).unwrap();
    make_readonly(&readonly);
    let tool = WriteFileTool::new(dir.path().to_str().unwrap());
    // 同 target 连续失败两次
    let err1 = tool
        .invoke(
            serde_json::json!({"file_path": "readonly/a.txt", "content": "first"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    let id1 = extract_draft_id(&err1);
    let err2 = tool
        .invoke(
            serde_json::json!({"file_path": "readonly/a.txt", "content": "second"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    let id2 = extract_draft_id(&err2);
    assert_ne!(id1, id2, "两次失败应产生不同 draft_id");
    // 旧 id 立即失效,新 id 可恢复
    make_writable(&readonly);
    let err = tool
        .invoke(
            serde_json::json!({"file_path": "readonly/a.txt", "from_draft": id1}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("unknown or no longer available"),
        "旧 draft_id 应失效: {err}"
    );
    tool.invoke(
        serde_json::json!({"file_path": "readonly/a.txt", "from_draft": id2}),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();
    let content = std::fs::read_to_string(dir.path().join("readonly/a.txt")).unwrap();
    assert_eq!(content, "second");
}

#[cfg(unix)]
#[tokio::test]
async fn test_write_draft_disabled_saves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let readonly = dir.path().join("readonly");
    std::fs::create_dir(&readonly).unwrap();
    make_readonly(&readonly);
    let tool = WriteFileTool::with_draft(dir.path().to_str().unwrap(), false);
    let err = tool
        .invoke(
            serde_json::json!({"file_path": "readonly/f.txt", "content": "hello"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(!err.contains("draft_"), "禁用时不应存草稿: {err}");
    // from_draft 任意 id → unknown 降级文案
    let err = tool
        .invoke(
            serde_json::json!({
                "file_path": "f.txt",
                "from_draft": "draft_00000000-0000-7000-0000-000000000000"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("unknown or no longer available"),
        "禁用时恢复应降级: {err}"
    );
}

#[tokio::test]
async fn test_write_validation_failure_saves_no_draft() {
    let dir = tempfile::tempdir().unwrap();
    let tool = WriteFileTool::new(dir.path().to_str().unwrap());
    // 参数错误(content 未提供)不落草稿:内容未达文件系统层,无内容可恢复
    let err = tool
        .invoke(
            serde_json::json!({"file_path": "f.txt"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(!err.contains("draft_"), "参数错误不应存草稿: {err}");
}
