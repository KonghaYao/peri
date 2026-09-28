use crate::error_suggest::context::{ErrorContext, ToolRegistrySnapshot};
use crate::error_suggest::registry::ErrorSuggester;
use crate::error_suggest::suggesters::regex_suggester::RegexSuggester;

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
        ErrorContext::new(
            tool_name,
            &self.input,
            err,
            std::path::Path::new("."),
            &self.snap,
        )
    }
}

#[test]
fn test_regex_suggester_only_for_grep() {
    let holder = CtxHolder::new(serde_json::json!({}));
    let ctx = holder.ctx("Read", "Error: regex parse error");
    assert!(RegexSuggester.suggest(&ctx).is_none());
}

#[test]
fn test_regex_suggester_recognizes_unclosed_paren() {
    let holder = CtxHolder::new(serde_json::json!({ "pattern": "(foo" }));
    let err = "Error: regex parse error: unclosed group, expected ')', POS: 4";
    let ctx = holder.ctx("Grep", err);
    let result = RegexSuggester.suggest(&ctx);
    assert!(result.is_some());
    let sug = result.unwrap();
    assert!(sug.summary.contains("regex") || sug.summary.contains("正则"));
}

#[test]
fn test_regex_suggester_skips_non_regex_errors() {
    let holder = CtxHolder::new(serde_json::json!({ "pattern": "foo" }));
    let ctx = holder.ctx("Grep", "Error: Search path does not exist: /tmp/none");
    assert!(RegexSuggester.suggest(&ctx).is_none());
}

/// N3 归一：`mcp__workspace__Grep` 必须与裸名同门槛。
#[test]
fn test_regex_matches_workspace_effective_name() {
    let holder = CtxHolder::new(serde_json::json!({ "pattern": "(foo" }));
    let err = "Error: regex parse error: unclosed group, expected ')', POS: 4";
    let ctx = holder.ctx("mcp__workspace__Grep", err);
    let sug = RegexSuggester
        .suggest(&ctx)
        .expect("effective name `mcp__workspace__Grep` 必须命中 Grep 门槛");
    assert!(sug.summary.contains("regex"));
}

/// 反例（保守语义）：未注册实例的 `mcp__foo__Grep` 未命中归一表 ⇒ 门槛不命中。
#[test]
fn test_regex_skips_unregistered_mcp_instance() {
    let holder = CtxHolder::new(serde_json::json!({ "pattern": "(foo" }));
    let err = "Error: regex parse error: unclosed group, expected ')', POS: 4";
    let ctx = holder.ctx("mcp__foo__Grep", err);
    assert!(
        RegexSuggester.suggest(&ctx).is_none(),
        "未注册实例的名字不得命中 Grep 门槛"
    );
}
