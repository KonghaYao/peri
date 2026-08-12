use crate::error_suggest::context::{ErrorContext, ToolRegistrySnapshot};
use crate::error_suggest::registry::ErrorSuggester;
use crate::error_suggest::suggesters::glob_pattern_suggester::GlobPatternSuggester;

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

    fn ctx<'a>(&'a self, tool_name: &'a str, err: &'a str) -> ErrorContext<'a> {
        let cwd = std::path::Path::new(".");
        ErrorContext::new(tool_name, &self.input, err, cwd, &self.snap)
    }
}

#[test]
fn test_glob_pattern_suggester_only_for_glob() {
    let holder = CtxHolder::new(serde_json::json!({}));
    let ctx = holder.ctx("Read", "Error: Pattern syntax error in \"[foo\": ...");
    assert!(GlobPatternSuggester.suggest(&ctx).is_none());
}

#[test]
fn test_glob_pattern_suggester_recognizes_syntax_error() {
    let holder = CtxHolder::new(serde_json::json!({
        "pattern": "[unclosed",
    }));
    let ctx = holder.ctx(
        "Glob",
        "Error: Pattern syntax error in \"[unclosed\": unclosed character class",
    );
    let result = GlobPatternSuggester.suggest(&ctx);
    assert!(result.is_some());
    let sug = result.unwrap();
    assert!(sug.summary.contains("Invalid glob syntax") && sug.summary.contains("Examples"));
}

#[test]
fn test_glob_pattern_suggester_skips_non_syntax_errors() {
    let holder = CtxHolder::new(serde_json::json!({
        "pattern": "*.rs",
    }));
    let ctx = holder.ctx("Glob", "Error: Directory not found: /nonexistent");
    assert!(GlobPatternSuggester.suggest(&ctx).is_none());
}
