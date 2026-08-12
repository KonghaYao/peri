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

/// 无相似候选时：不得硬凑 "Did you mean"（如 xy → xylophone），
/// 应回退到环境类兜底诊断。
#[test]
fn test_bash_no_similar_candidate_falls_back_to_env_diagnosis() {
    let holder = CtxHolder::new(serde_json::json!({
        "command": "xx_q1w2e3_not_a_real_cmd_xx",
    }));
    let err = "zsh:1: command not found: xx_q1w2e3_not_a_real_cmd_xx\n[Exit code: 127]";
    let ctx = holder.ctx("Bash", err);
    let sug = BashCommandSuggester
        .suggest(&ctx)
        .expect("command not found 应产生建议（兜底文案）");
    assert!(
        !sug.summary.contains("Did you mean"),
        "无相似候选时不应硬凑 Did you mean: {}",
        sug.summary
    );
    assert!(
        sug.summary.contains("not found in PATH"),
        "兜底文案应说明命令不存在: {}",
        sug.summary
    );
    assert!(
        sug.summary.contains("environment"),
        "兜底文案应给出环境类诊断: {}",
        sug.summary
    );
}

/// 真实丢字符拼错（carg→cargo，分数 ≥90）应给出 Did you mean 候选。
/// 本仓库开发/CI 环境必有 rust toolchain，cargo 一定在 PATH。
#[test]
fn test_bash_real_typo_suggests_similar_command() {
    let cargo_available = std::process::Command::new("which")
        .arg("cargo")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !cargo_available {
        return;
    }
    let holder = CtxHolder::new(serde_json::json!({
        "command": "carg build",
    }));
    let err = "zsh:1: command not found: carg\n[Exit code: 127]";
    let ctx = holder.ctx("Bash", err);
    let sug = BashCommandSuggester
        .suggest(&ctx)
        .expect("拼错命令应产生建议");
    assert!(
        sug.summary.contains("Did you mean"),
        "高分拼错候选应给出 Did you mean: {}",
        sug.summary
    );
    assert!(
        sug.summary.contains("cargo"),
        "应点名相似命令 cargo: {}",
        sug.summary
    );
    assert!(
        !sug.summary.contains("lli-child-target"),
        "首字符不同的稀疏子序列噪声（lli-child-target）应被剔除: {}",
        sug.summary
    );
}
