//! v4 引名约定锁定：**prompt 文本引用的 builtin 工具名必须与注册表一致**。
//!
//! Direct system MCP 工具向模型暴露原名；prompt 中不得继续引用其旧前缀名。
//!
//! 边界：
//! - **受管文件清单是本轮迁移改动过的 prompt 文本**（[`MANAGED_PROMPT_FILES`]）；新增
//!   引用 builtin 工具的 prompt 文件必须登记，否则不在保护面内；
//! - YAML frontmatter 在解析时剥离；本测试只检查模型看到的正文。
//! - `docs/**` / `spec/**` 是开发者文档，不属 prompt 面。

use std::path::{Path, PathBuf};

use peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES;

/// 受管 prompt 文本（相对仓库根路径，本轮迁移逐个核对并改动过的文件）。
const MANAGED_PROMPT_FILES: &[&str] = &[
    ".claude/agents/advisor.md",
    ".claude/agents/code-reviewer.md",
    ".claude/skills/blog-writer/SKILL.md",
    ".claude/skills/codebase-index/SKILL.md",
    ".claude/skills/project-maturity/SKILL.md",
    "peri-acp/prompts/sections/03_doing_tasks.md",
    "peri-acp/prompts/sections/05_using_tools.md",
    "peri-acp/prompts/sections/06_tone_style.md",
    "peri-acp/prompts/sections/07_runtime.md",
    "peri-acp/prompts/sections/10_hitl.md",
    "peri-acp/prompts/sections/11_subagent.md",
    "peri-agent/src/agent/compact_v2/descriptions/summary_system_prompt.md",
    "peri-middlewares/src/middleware/descriptions/bash.md",
    "peri-middlewares/src/middleware/descriptions/web_fetch.md",
    "peri-middlewares/src/middleware/descriptions/web_search.md",
    "peri-middlewares/src/skills/builtin/skills/ultracode/SKILL.md",
    "peri-middlewares/src/skills/builtin/skills/use-artifacts/SKILL.md",
    "peri-middlewares/src/subagent/built-in/coder.md",
    "peri-middlewares/src/subagent/built-in/explorer.md",
    "peri-middlewares/src/subagent/built-in/general-purpose.md",
    "peri-middlewares/src/subagent/built-in/plan.md",
    "peri-middlewares/src/subagent/built-in/web-researcher.md",
    "peri-middlewares/src/tools/filesystem/descriptions/edit.md",
    "peri-middlewares/src/tools/filesystem/descriptions/glob.md",
    "peri-middlewares/src/tools/filesystem/descriptions/grep.md",
    "peri-middlewares/src/tools/filesystem/descriptions/read.md",
    "peri-middlewares/src/tools/filesystem/descriptions/write.md",
];

/// 仓库根（`peri-middlewares/` 的父目录；不硬编码绝对路径）。
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate 必须位于仓库根之下")
        .to_path_buf()
}

/// 剥离 YAML frontmatter（语义与 `claude_agent_parser::parse` 一致：开闭各一行 `---`，
/// 闭合行按 `trim() == "---"` 判定）。
fn prompt_body(text: &str) -> &str {
    if !text.starts_with("---") {
        return text;
    }
    let after_open = &text[3..];
    let mut offset = 0usize;
    for line in after_open.split_inclusive('\n') {
        offset += line.len();
        if line.trim() == "---" {
            return after_open[offset..].trim_start_matches('\n');
        }
    }
    text
}

/// 逐个读取受管文件的 prompt 正文（frontmatter 已剥离）。
fn managed_bodies() -> Vec<(&'static str, String)> {
    assert!(
        !MANAGED_PROMPT_FILES.is_empty(),
        "受管清单不得为空（空清单会让本测试恒绿）"
    );
    MANAGED_PROMPT_FILES
        .iter()
        .map(|relative| {
            let path = repo_root().join(relative);
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("受管文件必须存在 {}: {error}", path.display()));
            let body = prompt_body(&text).to_string();
            assert!(
                !body.trim().is_empty(),
                "{relative} 的 prompt 正文不得为空（frontmatter 之外必须有内容）"
            );
            (*relative, body)
        })
        .collect()
}

/// Model-facing direct builtin tools use their original names in managed prompts.
#[test]
fn managed_prompt_files_use_direct_builtin_names() {
    let direct: Vec<_> = BUILTIN_MCP_INSTANCES
        .iter()
        .flat_map(|instance| instance.tools.iter().map(move |tool| (instance.name, tool)))
        .filter(|(_, tool)| tool.direct)
        .collect();
    assert!(
        !direct.is_empty(),
        "builtin direct tool set must be nonempty"
    );
    let bodies = managed_bodies();
    let mut raw_references = 0;
    for (relative, body) in bodies {
        for (instance, tool) in &direct {
            assert!(
                !body.contains(&format!("mcp__{instance}__{}", tool.original_name)),
                "{relative}: direct builtin tool {} must use its original name",
                tool.original_name
            );
            raw_references += body.matches(&format!("`{}`", tool.original_name)).count();
        }
    }
    assert!(
        raw_references > 0,
        "managed prompts must name direct builtin tools"
    );
}
