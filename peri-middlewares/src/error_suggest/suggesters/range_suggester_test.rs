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
