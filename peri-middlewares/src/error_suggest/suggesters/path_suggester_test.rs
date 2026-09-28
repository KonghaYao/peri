use crate::error_suggest::context::{ErrorContext, ToolRegistrySnapshot};
use crate::error_suggest::registry::ErrorSuggester;
use crate::error_suggest::suggesters::path_suggester::PathSuggester;
use std::collections::HashSet;
use std::fs;

/// 持有 input/snapshot，让 ErrorContext 借用稳定
struct CtxHolder {
    input: serde_json::Value,
    snap: ToolRegistrySnapshot,
}

impl CtxHolder {
    fn new(input: serde_json::Value) -> Self {
        Self {
            input,
            snap: ToolRegistrySnapshot {
                all_tool_names: HashSet::new(),
                subagent_types: HashSet::new(),
            },
        }
    }

    fn ctx<'a>(
        &'a self,
        tool_name: &'a str,
        err: &'a str,
        cwd: &'a std::path::Path,
    ) -> ErrorContext<'a> {
        ErrorContext::new(tool_name, &self.input, err, cwd, &self.snap)
    }
}

#[test]
fn test_path_suggester_skips_non_path_tools() {
    let holder = CtxHolder::new(serde_json::json!({}));
    let cwd = std::path::Path::new(".");
    let ctx = holder.ctx("Bash", "Error: command not found", cwd);
    let result = PathSuggester.suggest(&ctx);
    assert!(result.is_none());
}

#[test]
fn test_path_suggester_skips_non_path_errors() {
    let holder = CtxHolder::new(serde_json::json!({ "file_path": "/nonexistent" }));
    let cwd = std::path::Path::new(".");
    let ctx = holder.ctx("Read", "Error: permission denied", cwd);
    let result = PathSuggester.suggest(&ctx);
    assert!(result.is_none());
}

/// N3 归一：`mcp__workspace__*` 走白名单，且参数键必须按**归一后**名字取——
/// `Glob` 用 `path`，其余文件工具用 `file_path`（不归一会取错字段 ⇒ 无建议）。
#[test]
fn test_path_suggester_matches_workspace_effective_names() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    fs::write(base.join("main.rs"), "fn main() {}").unwrap();
    let missing = base.join("maiin.rs");
    let err = format!("Error: Search path does not exist: {}", missing.display());

    // Glob 形态：路径参数键是 `path`
    let holder = CtxHolder::new(serde_json::json!({
        "path": missing.to_string_lossy().to_string(),
    }));
    let ctx = holder.ctx("mcp__workspace__Glob", &err, base);
    let sug = PathSuggester
        .suggest(&ctx)
        .expect("effective name `mcp__workspace__Glob` 应命中白名单并取 `path` 键");
    assert!(
        sug.summary.contains("main.rs"),
        "maiin.rs 的最佳候选应为 main.rs，实际：{}",
        sug.summary
    );

    // Read 形态：路径参数键是 `file_path`
    let holder = CtxHolder::new(serde_json::json!({
        "file_path": missing.to_string_lossy().to_string(),
    }));
    let ctx = holder.ctx("mcp__workspace__Read", &err, base);
    assert!(
        PathSuggester.suggest(&ctx).is_some(),
        "effective name `mcp__workspace__Read` 应命中白名单并取 `file_path` 键"
    );
}

/// 反例（保守语义）：未注册实例的 `mcp__foo__Read` 未命中归一表 ⇒ 白名单不命中。
#[test]
fn test_path_suggester_skips_unregistered_mcp_instance() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    fs::write(base.join("main.rs"), "").unwrap();
    let missing = base.join("maiin.rs");
    let holder = CtxHolder::new(serde_json::json!({
        "file_path": missing.to_string_lossy().to_string(),
    }));
    let err = format!("Error: File not found at {}", missing.display());
    let ctx = holder.ctx("mcp__foo__Read", &err, base);
    assert!(
        PathSuggester.suggest(&ctx).is_none(),
        "未注册实例的名字不得命中路径工具白名单"
    );
}

#[test]
fn test_path_suggester_returns_candidates_for_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    fs::write(base.join("main.rs"), "fn main() {}").unwrap();
    fs::write(base.join("lib.rs"), "").unwrap();
    fs::write(base.join("mainold.rs"), "").unwrap();

    let holder = CtxHolder::new(serde_json::json!({
        "file_path": base.join("maiin.rs").to_string_lossy().to_string(),
    }));
    let err = format!(
        "Error: File not found at {}",
        base.join("maiin.rs").display()
    );
    let ctx = holder.ctx("Read", &err, base);
    let result = PathSuggester.suggest(&ctx);
    assert!(result.is_some(), "应该返回建议");
    let sug = result.unwrap();
    assert!(sug.summary.contains("Did you mean"));
    assert!(
        sug.summary.contains("main.rs"),
        "maiin.rs 的最佳候选应该是 main.rs（编辑距离最近），实际：{}",
        sug.summary
    );
}

#[test]
fn test_path_suggester_handles_relative_path() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    fs::create_dir_all(base.join("src")).unwrap();
    fs::write(base.join("src").join("lib.rs"), "").unwrap();

    let holder = CtxHolder::new(serde_json::json!({
        "file_path": "src/lb.rs",
    }));
    let err = "Error: File not found at src/lb.rs";
    let ctx = holder.ctx("Read", err, base);
    let result = PathSuggester.suggest(&ctx);
    assert!(result.is_some());
    assert!(result.unwrap().summary.contains("lib.rs"));
}

#[test]
fn test_path_suggester_no_candidates_returns_none() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    let holder = CtxHolder::new(serde_json::json!({
        "file_path": "totally_different.xyz",
    }));
    let err = "Error: File not found at totally_different.xyz";
    let ctx = holder.ctx("Read", err, base);
    let result = PathSuggester.suggest(&ctx);
    assert!(result.is_none(), "无候选时应返回 None");
}

#[test]
fn test_path_suggester_perf_under_50ms_in_large_dir() {
    use std::time::Instant;

    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();

    // 创建 200 个文件
    for i in 0..200 {
        std::fs::write(base.join(format!("file_{i:03}.rs")), "").unwrap();
    }

    let holder = CtxHolder::new(serde_json::json!({
        "file_path": base.join("fle_100.rs").to_string_lossy().to_string(),
    }));
    let err = format!(
        "Error: File not found at {}",
        base.join("fle_100.rs").display()
    );

    let start = Instant::now();
    let result = PathSuggester.suggest(&holder.ctx("Read", &err, base));
    let elapsed = start.elapsed();

    assert!(result.is_some());
    assert!(elapsed.as_millis() < 50, "应该 < 50ms，实际: {elapsed:?}");
}

/// [回归测试] 真实 Edit 的文本匹配失败不应给已存在的文件添加路径纠错。
#[tokio::test]
async fn test_edit_missing_text_has_no_path_suggestion_but_missing_file_does() {
    use crate::tools::filesystem::EditFileTool;
    use peri_agent::tools::{BaseTool, ToolContext};
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("dup.txt"), "actual text\n").unwrap();
    let tool = EditFileTool::new(dir.path().to_str().unwrap());
    for (file, expects_path_hint) in [("dup.txt", false), ("dupp.txt", true)] {
        let holder = CtxHolder::new(
            serde_json::json!({"file_path":file, "old_string":"absent text", "new_string":"replacement"}),
        );
        let error = tool
            .invoke(holder.input.clone(), ToolContext::new(&[], "."))
            .await
            .unwrap_err();
        let projected = &error
            .downcast_ref::<crate::tools::failure::ToolFailure>()
            .unwrap()
            .recovery;
        for name in ["Edit", "mcp__workspace__Edit"] {
            let suggestion = PathSuggester.suggest(&holder.ctx(name, projected, dir.path()));
            assert_eq!(
                suggestion.is_some(),
                expects_path_hint,
                "file={file}, error={projected}"
            );
        }
    }
    assert_eq!(
        fs::read_to_string(dir.path().join("dup.txt")).unwrap(),
        "actual text\n"
    );
}
