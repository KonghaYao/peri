use crate::error_suggest::context::{ErrorContext, ToolRegistrySnapshot};
use crate::error_suggest::registry::ErrorSuggester;
use crate::error_suggest::suggesters::bash_command_suggester::BashCommandSuggester;

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
fn test_bash_recognizes_command_not_found() {
    // CI/无 git 环境下 skip：which git 必须返回 exit 0
    let git_available = std::process::Command::new("which")
        .arg("git")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !git_available {
        return;
    }
    let holder = CtxHolder::new(serde_json::json!({
        "command": "gti status",
    }));
    let err = "zsh:1: command not found: gti\n[Exit code: 127]";
    let ctx = holder.ctx("Bash", err);
    let result = BashCommandSuggester.suggest(&ctx);
    assert!(result.is_some(), "应该识别 command not found + exit 127");
    let sug = result.unwrap();
    // git 应该是候选之一（如果在 PATH 中）
    assert!(
        sug.summary.contains("Did you mean")
            || sug.summary.contains("git")
            || sug.summary.contains("not found"),
        "实际：{}",
        sug.summary
    );
}

#[test]
fn test_bash_skips_non_command_errors() {
    let holder = CtxHolder::new(serde_json::json!({
        "command": "ls /nonexistent",
    }));
    let err = "ls: /nonexistent: No such file or directory\n[Exit code: 1]";
    let ctx = holder.ctx("Bash", err);
    assert!(BashCommandSuggester.suggest(&ctx).is_none());
}

#[test]
fn test_bash_skips_non_bash_tools() {
    let holder = CtxHolder::new(serde_json::json!({}));
    let err = "zsh: command not found: foo\n[Exit code: 127]";
    let ctx = holder.ctx("Read", err);
    assert!(BashCommandSuggester.suggest(&ctx).is_none());
}
