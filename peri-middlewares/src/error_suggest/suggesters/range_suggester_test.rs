use crate::error_suggest::context::{ErrorContext, ToolRegistrySnapshot};
use crate::error_suggest::registry::ErrorSuggester;
use crate::error_suggest::suggesters::range_suggester::RangeSuggester;

struct CtxHolder {
    input: serde_json::Value,
    snap: ToolRegistrySnapshot,
}

impl CtxHolder {
    fn new(input: serde_json::Value) -> Self {
        Self {
            input,
            snap: ToolRegistrySnapshot::default(),
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
fn test_range_suggester_only_for_read() {
    let holder = CtxHolder::new(serde_json::json!({}));
    let cwd = std::path::Path::new(".");
    let ctx = holder.ctx(
        "Edit",
        "Error: offset 100 exceeds file length (50 lines)",
        cwd,
    );
    assert!(RangeSuggester.suggest(&ctx).is_none());
}

#[test]
fn test_range_suggester_recognizes_offset_error() {
    let holder = CtxHolder::new(serde_json::json!({
        "file_path": "/tmp/foo.rs",
        "offset": 100,
        "limit": 10,
    }));
    let cwd = std::path::Path::new(".");
    let ctx = holder.ctx(
        "Read",
        "Error: offset 100 exceeds file length (50 lines)",
        cwd,
    );
    let result = RangeSuggester.suggest(&ctx);
    assert!(result.is_some());
    let sug = result.unwrap();
    assert_eq!(
        sug.summary,
        "Omit offset to read from the beginning. If targeting a known location, use only an observed line number in 1..=50; do not guess."
    );
}

#[test]
fn test_range_suggester_does_not_duplicate_self_correcting_read_error() {
    let holder = CtxHolder::new(serde_json::json!({
        "file_path": "/tmp/foo.rs",
        "offset": 100,
    }));
    let cwd = std::path::Path::new(".");
    let ctx = holder.ctx(
        "Read",
        "Error: offset 100 exceeds file length (50 lines). Valid offsets are 1..=50; omit offset to read from the beginning. Do not guess another offset or use offset to probe the file end.",
        cwd,
    );
    assert!(
        RangeSuggester.suggest(&ctx).is_none(),
        "新版 Read 错误已自带恢复动作，不应追加重复建议"
    );
}

#[test]
fn test_range_suggester_skips_non_range_errors() {
    let holder = CtxHolder::new(serde_json::json!({
        "file_path": "/tmp/foo.rs",
    }));
    let cwd = std::path::Path::new(".");
    let ctx = holder.ctx("Read", "Error: File not found", cwd);
    assert!(RangeSuggester.suggest(&ctx).is_none());
}

/// N3 归一：`mcp__workspace__Read` 必须与裸名同门槛（且给出一致的恢复动作）。
#[test]
fn test_range_matches_workspace_effective_name() {
    let holder = CtxHolder::new(serde_json::json!({
        "file_path": "/tmp/foo.rs",
        "offset": 100,
        "limit": 10,
    }));
    let cwd = std::path::Path::new(".");
    let ctx = holder.ctx(
        "mcp__workspace__Read",
        "Error: offset 100 exceeds file length (50 lines)",
        cwd,
    );
    let sug = RangeSuggester
        .suggest(&ctx)
        .expect("effective name `mcp__workspace__Read` 必须命中 Read 门槛");
    assert_eq!(
        sug.summary,
        "Omit offset to read from the beginning. If targeting a known location, use only an observed line number in 1..=50; do not guess."
    );
}

/// 反例（保守语义）：未注册实例的 `mcp__foo__Read` 未命中归一表 ⇒ 门槛不命中。
#[test]
fn test_range_skips_unregistered_mcp_instance() {
    let holder = CtxHolder::new(serde_json::json!({
        "file_path": "/tmp/foo.rs",
        "offset": 100,
    }));
    let cwd = std::path::Path::new(".");
    let ctx = holder.ctx(
        "mcp__foo__Read",
        "Error: offset 100 exceeds file length (50 lines)",
        cwd,
    );
    assert!(
        RangeSuggester.suggest(&ctx).is_none(),
        "未注册实例的名字不得命中 Read 门槛"
    );
}
