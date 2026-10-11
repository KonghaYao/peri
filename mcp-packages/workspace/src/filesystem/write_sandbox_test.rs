use super::WriteSandboxTool;
use peri_agent::tools::BaseTool;

/// 从错误消息中提取 draft_id
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
            serde_json::json!({"file_path": "sandbox/hello.md", "content": "# Plan"}),
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
        serde_json::json!({"file_path": "sandbox/v2.md", "content": "v2"}),
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
            serde_json::json!({"file_path": "sandbox/../outside.txt", "content": "evil"}),
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
            serde_json::json!({"file_path": abs.to_str().unwrap(), "content": "evil"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err(), "绝对路径应被拒绝");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("绝对"), "错误消息应说明拒绝原因: {}", err);
}

/// 无沙箱前缀的相对路径按“沙箱内相对路径”解释（历史失败样本：LLM 写 'other/outside.txt'
/// 曾被直接拒绝；现在解析为 <沙箱根>/other/outside.txt，仍约束在沙箱内）。
#[tokio::test]
async fn test_write_sandbox_bare_relative_path_resolves_inside_sandbox() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox"]);
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "other/outside.txt", "content": "nope"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("Wrote 1 line"), "应写入成功: {result}");
    let content = std::fs::read_to_string(dir.path().join("sandbox/other/outside.txt")).unwrap();
    assert_eq!(content, "nope");
}

/// 裸文件名（无沙箱前缀）按沙箱内相对路径解释——历史高失败率场景的回归
/// （explorer/verification subagent 按 agent prompt 写 'report.md' 曾被拒）。
#[tokio::test]
async fn test_write_sandbox_bare_name_resolves_inside_sandbox() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox"]);
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "report.md", "content": "# Report"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("Wrote 1 line"),
        "裸文件名应写入成功: {result}"
    );
    let content = std::fs::read_to_string(dir.path().join("sandbox/report.md")).unwrap();
    assert_eq!(content, "# Report");
}

/// 子目录相对路径同样按沙箱内相对解释（agent prompt 历史示例 'subdir/report.md'）
#[tokio::test]
async fn test_write_sandbox_bare_subdir_resolves_inside_sandbox() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox"]);
    tool.invoke(
        serde_json::json!({"file_path": "sub/design.md", "content": "# Design"}),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();
    let content = std::fs::read_to_string(dir.path().join("sandbox/sub/design.md")).unwrap();
    assert_eq!(content, "# Design");
}

/// 多沙箱：裸路径按声明顺序落到第一个沙箱
#[tokio::test]
async fn test_write_sandbox_bare_name_uses_first_sandbox() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["plans", "output"]);
    tool.invoke(
        serde_json::json!({"file_path": "x.md", "content": "v"}),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();
    assert!(dir.path().join("plans/x.md").exists(), "应落到第一个沙箱");
    assert!(
        !dir.path().join("output/x.md").exists(),
        "不应落到第二个沙箱"
    );
}

/// fallback 解释同样受 symlink 逃逸防护约束
#[cfg(unix)]
#[tokio::test]
async fn test_write_sandbox_bare_path_symlink_escape_rejected() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("outside_dir")).unwrap();
    let tool = make_tool(&dir, vec!["sandbox"]);
    std::os::unix::fs::symlink(
        dir.path().join("outside_dir"),
        dir.path().join("sandbox/sub"),
    )
    .unwrap();
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "sub/evil.txt", "content": "bypass"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err(), "fallback 解释下 symlink 逃逸仍应被拒绝");
}

#[tokio::test]
async fn test_write_sandbox_symlink_escape_rejected() {
    let dir = tempfile::tempdir().unwrap();
    // 在沙箱外写入恶意文件
    std::fs::write(dir.path().join("outside.txt"), "evil").unwrap();
    #[cfg(unix)]
    {
        let tool = make_tool(&dir, vec!["sandbox"]);
        std::os::unix::fs::symlink(
            dir.path().join("outside.txt"),
            dir.path().join("sandbox/escape_link.txt"),
        )
        .unwrap();
        let result = tool
            .invoke(
                serde_json::json!({"file_path": "sandbox/escape_link.txt", "content": "bypass"}),
                peri_agent::tools::ToolContext::new(&[], "."),
            )
            .await;
        assert!(result.is_err(), "symlink 逃逸应被拒绝");
    }
}

#[tokio::test]
async fn test_write_sandbox_parent_symlink_escape_rejected() {
    let dir = tempfile::tempdir().unwrap();
    // sandbox/sub 是外部目录的 symlink
    std::fs::create_dir_all(dir.path().join("outside_dir")).unwrap();
    #[cfg(unix)]
    {
        let tool = make_tool(&dir, vec!["sandbox"]);
        std::os::unix::fs::symlink(
            dir.path().join("outside_dir"),
            dir.path().join("sandbox/sub"),
        )
        .unwrap();
        let result = tool
            .invoke(
                serde_json::json!({"file_path": "sandbox/sub/evil.txt", "content": "bypass"}),
                peri_agent::tools::ToolContext::new(&[], "."),
            )
            .await;
        assert!(result.is_err(), "父目录 symlink 逃逸应被拒绝");
    }
}

#[test]
fn test_write_sandbox_description_contains_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox", "output"]);
    let desc = tool.description();
    assert!(desc.contains("sandbox"));
    assert!(desc.contains("output"));
    assert!(desc.contains("Write a file ONLY into your sandbox directories"));
}

#[test]
fn test_write_sandbox_empty_allowed_dirs_ok() {
    let cwd = tempfile::tempdir().unwrap();
    let result = WriteSandboxTool::new(cwd.path().to_str().unwrap().to_string(), vec![]);
    // 空白名单应可构造（不注入时不报错）
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_write_sandbox_multi_dir() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["plans", "output"]);
    tool.invoke(
        serde_json::json!({"file_path": "plans/design.md", "content": "# Design"}),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();
    tool.invoke(
        serde_json::json!({"file_path": "output/result.json", "content": "{\"ok\": true}"}),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();
    assert!(dir.path().join("plans/design.md").exists());
    assert!(dir.path().join("output/result.json").exists());
}

/// [回归测试] 沙箱目录不存在时构造应自动创建，而非失败。
/// 对应 spec/issues/2026-07-20-plan-agent-writesandbox-not-found.md
#[test]
fn test_write_sandbox_auto_create_dir() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap().to_string();
    // 不预创建沙箱目录——WriteSandboxTool::new 应自动创建
    assert!(!dir.path().join("plans").exists(), "开始前沙箱目录不应存在");
    let result = WriteSandboxTool::new(cwd, vec!["plans".into()]);
    assert!(result.is_ok(), "目录不存在时构造应成功: {:?}", result.err());
    // 验证目录确实被创建
    assert!(
        dir.path().join("plans").is_dir(),
        "构造后沙箱目录应被自动创建"
    );
    // 验证可正常写入
    let tool = result.unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(tool.invoke(
        serde_json::json!({"file_path": "plans/test.md", "content": "# Auto created"}),
        peri_agent::tools::ToolContext::new(&[], "."),
    ))
    .unwrap();
    let content = std::fs::read_to_string(dir.path().join("plans/test.md")).unwrap();
    assert_eq!(content, "# Auto created");
}

/// [回归测试] 错误消息中的允许目录应使用原始相对路径，而非 canonicalized 绝对路径。
/// 对应 spec/issues/2026-07-20-writesandbox-still-confused-with-write.md 修复 #2
#[tokio::test]
async fn test_write_sandbox_error_displays_relative_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["plans"]);
    // '..' 穿越被拒——错误消息中的允许目录应为原始相对路径
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "plans/../bare.md", "content": "test"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    // 错误消息应包含相对路径 "plans"，而非 canonicalized 绝对路径
    assert!(
        err.contains("\"plans\""),
        "错误消息应展示相对路径 'plans'，而非绝对路径: {}",
        err
    );
    // 不应包含 tempdir 的绝对路径
    let abs_path = dir.path().display().to_string();
    assert!(
        !err.contains(&abs_path),
        "错误消息不应包含绝对路径 '{}': {}",
        abs_path,
        err
    );
}

// ===== 失败草稿恢复机制测试(决策 6) =====

#[cfg(unix)]
#[tokio::test]
async fn test_write_sandbox_tmp_failure_saves_draft() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap().to_string();
    let tool = WriteSandboxTool::new(cwd, vec!["sandbox".into()]).unwrap();
    // 沙箱目录只读 → tmp 写入失败
    make_readonly(&dir.path().join("sandbox"));
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "sandbox/f.txt", "content": "hello\nworld"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let err = result.unwrap_err().to_string();
    assert!(err.contains("内容草稿已保存"), "应含中文草稿提示: {err}");
    assert!(err.contains("draft_"), "应含 draft_id: {err}");
    assert!(err.contains("2 行"), "应含行数: {err}");
    assert!(err.contains("11 字节"), "应含字节数: {err}");
    assert!(!err.contains("hello"), "错误消息不应展示正文: {err}");
}

#[cfg(unix)]
#[tokio::test]
async fn test_write_sandbox_from_draft_restores() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap().to_string();
    let tool = WriteSandboxTool::with_draft(cwd, vec!["sandbox".into()], true).unwrap();
    make_readonly(&dir.path().join("sandbox"));
    let err = tool
        .invoke(
            serde_json::json!({"file_path": "sandbox/f.txt", "content": "hello\nworld"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    let draft_id = extract_draft_id(&err);
    // 还原权限后 from_draft 恢复
    make_writable(&dir.path().join("sandbox"));
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "sandbox/f.txt", "from_draft": draft_id}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("Wrote 2 lines"), "恢复消息: {result}");
    let content = std::fs::read_to_string(dir.path().join("sandbox/f.txt")).unwrap();
    assert_eq!(content, "hello\nworld");
}

#[cfg(unix)]
#[tokio::test]
async fn test_write_sandbox_from_draft_runs_full_validation() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap().to_string();
    let tool = WriteSandboxTool::with_draft(cwd, vec!["sandbox".into()], true).unwrap();
    make_readonly(&dir.path().join("sandbox"));
    let err = tool
        .invoke(
            serde_json::json!({"file_path": "sandbox/a.txt", "content": "payload"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    let draft_id = extract_draft_id(&err);
    // from_draft 恢复必须走完整校验链:穿越路径被拒,而非草稿错误
    let err = tool
        .invoke(
            serde_json::json!({"file_path": "sandbox/../outside.txt", "from_draft": draft_id}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("拒绝"), "应命中校验链: {err}");
    assert!(!err.contains("草稿"), "不应是草稿错误: {err}");
    // 草稿未被消费,原路径仍可恢复
    make_writable(&dir.path().join("sandbox"));
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "sandbox/a.txt", "from_draft": draft_id}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_ok(), "原路径应仍可恢复: {:?}", result.err());
    let content = std::fs::read_to_string(dir.path().join("sandbox/a.txt")).unwrap();
    assert_eq!(content, "payload");
}

#[tokio::test]
async fn test_write_sandbox_requires_content_or_from_draft() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap().to_string();
    let tool = WriteSandboxTool::with_draft(cwd, vec!["sandbox".into()], true).unwrap();
    let err = tool
        .invoke(
            serde_json::json!({"file_path": "sandbox/f.txt"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("必须提供"), "缺参数文案: {err}");
}

/// 回归（互斥劫持）：同时携带 content 与 from_draft 时 content 优先写入成功
/// （原「互斥报错」导致文件无法落盘、模型被迫重输出一遍内容）。
#[tokio::test]
async fn test_write_sandbox_content_and_from_draft_mutually_exclusive() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap().to_string();
    let tool = WriteSandboxTool::with_draft(cwd, vec!["sandbox".into()], true).unwrap();
    let result = tool
        .invoke(
            serde_json::json!({
                "file_path": "sandbox/f.txt",
                "content": "x",
                "from_draft": "draft_00000000-0000-7000-0000-000000000000"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("x"), "应以 content 优先写入: {result}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("sandbox/f.txt")).unwrap(),
        "x",
        "文件应落盘 content 内容"
    );
}

/// 回归（占位符）：from_draft 填占位符等同未提供，content 生效
#[tokio::test]
async fn test_write_sandbox_from_draft_placeholder_uses_content() {
    for placeholder in ["", "__omit__"] {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().to_str().unwrap().to_string();
        let tool = WriteSandboxTool::with_draft(cwd, vec!["sandbox".into()], true).unwrap();
        let result = tool
            .invoke(
                serde_json::json!({
                    "file_path": "sandbox/f.txt",
                    "content": "real",
                    "from_draft": placeholder,
                }),
                peri_agent::tools::ToolContext::new(&[], "."),
            )
            .await
            .unwrap();
        assert!(
            result.contains("Wrote"),
            "占位符 from_draft {:?} 应等同未提供并写入成功: {}",
            placeholder,
            result
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("sandbox/f.txt")).unwrap(),
            "real",
            "占位符 from_draft {:?} 时文件应落盘 content 内容",
            placeholder
        );
    }
}

#[tokio::test]
async fn test_write_sandbox_from_draft_unknown_degrades() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap().to_string();
    let tool = WriteSandboxTool::with_draft(cwd, vec!["sandbox".into()], true).unwrap();
    let err = tool
        .invoke(
            serde_json::json!({
                "file_path": "sandbox/f.txt",
                "from_draft": "draft_00000000-0000-7000-0000-000000000000"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("不存在或已失效"), "未知草稿降级文案: {err}");
}

#[cfg(unix)]
#[tokio::test]
async fn test_write_sandbox_draft_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap().to_string();
    let tool = WriteSandboxTool::with_draft(cwd, vec!["sandbox".into()], false).unwrap();
    make_readonly(&dir.path().join("sandbox"));
    let err = tool
        .invoke(
            serde_json::json!({"file_path": "sandbox/f.txt", "content": "hello"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(!err.contains("draft_"), "禁用时不应存草稿: {err}");
}
