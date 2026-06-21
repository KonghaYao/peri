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
