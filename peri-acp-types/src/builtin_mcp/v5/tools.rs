//! v5 能力工具声明（顺序即 `tools/list` 声明顺序）。
//!
//! 与 v4 工具表逐项一致，由 `v5_test.rs` 的等价性测试锁定。

use crate::tools::ContextRetention;

use super::CapabilityTool;

const fn direct_tool(
    original_name: &'static str,
    context_retention: ContextRetention,
    completes_on_response: bool,
    prompt_declaration: Option<&'static str>,
) -> CapabilityTool {
    CapabilityTool {
        original_name,
        model_name: original_name,
        direct: true,
        model_visible: true,
        prompt_declaration,
        context_retention,
        completes_on_response,
    }
}

const fn deferred_tool(
    original_name: &'static str,
    model_name: &'static str,
    model_visible: bool,
    context_retention: ContextRetention,
    completes_on_response: bool,
) -> CapabilityTool {
    CapabilityTool {
        original_name,
        model_name,
        direct: false,
        model_visible,
        prompt_declaration: None,
        context_retention,
        completes_on_response,
    }
}

pub(super) const WORKSPACE_CAPABILITY_TOOLS: &[CapabilityTool] = &[
    direct_tool(
        "Read",
        ContextRetention::Recomputable,
        true,
        Some(
            "Read a file → `{{name}}` ({{title}}). Use `{{name}}` for file content, not `cat`/`head`/`tail`.",
        ),
    ),
    direct_tool(
        "Write",
        ContextRetention::SideEffectReceipt,
        true,
        Some(
            "Write a file → `{{name}}` (full contents). Use `{{name}}` for writing files, not `echo >`/`sed`/`awk`.",
        ),
    ),
    direct_tool(
        "Edit",
        ContextRetention::SideEffectReceipt,
        true,
        Some(
            "Edit a file → `{{name}}` (targeted diff). Use `{{name}}` for targeted edits, not `echo >`/`sed`/`awk`.",
        ),
    ),
    direct_tool(
        "Glob",
        ContextRetention::Recomputable,
        true,
        Some(
            "Find files by name → `{{name}}` (e.g. `**/*.rs`, `*.config.json`). Use `{{name}}` for name search, not `Bash` with `find`; never `{{name}}(\"*\")`/`{{name}}(\"**/*\")` — that dumps the whole tree — list directories via `folder_operations` or `Bash ls`.",
        ),
    ),
    direct_tool(
        "Grep",
        ContextRetention::Recomputable,
        true,
        Some(
            "Search file contents → `{{name}}` (regex, fast, scoped). Use `{{name}}` for content search, not `grep`/`rg`.",
        ),
    ),
    direct_tool(
        "folder_operations",
        ContextRetention::Recomputable,
        true,
        Some(
            "List a directory / check structure → `{{name}}` (atomic, cross-platform, structured). Prefer `{{name}}` when entries are needed as data, `Bash ls -la` for quick one-shot human-readable output; avoid `mkdir`/`test -d` via `Bash`.",
        ),
    ),
    direct_tool(
        "Bash",
        ContextRetention::PreviewOnly,
        false,
        Some(
            "Run a shell command → `{{name}}` ({{title}}). Prefer the purpose-built tools above when applicable: they give structured output and enforce permission rules.",
        ),
    ),
];

pub(super) const WEB_CAPABILITY_TOOLS: &[CapabilityTool] = &[
    direct_tool(
        "WebSearch",
        ContextRetention::Recomputable,
        true,
        Some(
            "Look up current information beyond your knowledge → `{{name}}` ({{title}}). Query the web for recent or external facts.",
        ),
    ),
    direct_tool(
        "WebFetch",
        ContextRetention::Recomputable,
        true,
        Some(
            "Fetch a URL you have reason to trust → `{{name}}` ({{title}}). Retrieve URLs with `{{name}}`, never `curl` via Bash.",
        ),
    ),
];

pub(super) const ARTIFACT_CAPABILITY_TOOLS: &[CapabilityTool] = &[direct_tool(
    "artifact",
    ContextRetention::SideEffectReceipt,
    true,
    Some(
        "Share generated HTML/Markdown as a public link → `{{name}}` ({{title}}). The link expires in 7 or 30 days; use for dashboards, reports, and prototypes you want to share.",
    ),
)];

pub(super) const CRON_CAPABILITY_TOOLS: &[CapabilityTool] = &[
    deferred_tool(
        "cron_register",
        "mcp__peri__cron_register",
        true,
        ContextRetention::SideEffectReceipt,
        true,
    ),
    deferred_tool(
        "cron_list",
        "mcp__peri__cron_list",
        true,
        ContextRetention::Recomputable,
        true,
    ),
    deferred_tool(
        "cron_remove",
        "mcp__peri__cron_remove",
        true,
        ContextRetention::SideEffectReceipt,
        true,
    ),
    deferred_tool(
        "cron_toggle",
        "mcp__peri__cron_toggle",
        true,
        ContextRetention::SideEffectReceipt,
        true,
    ),
    deferred_tool(
        "cron_acknowledge",
        "mcp__peri__cron_acknowledge",
        false,
        ContextRetention::PreviewOnly,
        false,
    ),
];

pub(super) const MONITOR_CAPABILITY_TOOLS: &[CapabilityTool] = &[
    deferred_tool(
        "monitor_start",
        "mcp__peri__monitor_start",
        true,
        ContextRetention::SideEffectReceipt,
        false,
    ),
    deferred_tool(
        "monitor_read",
        "mcp__peri__monitor_read",
        true,
        ContextRetention::Recomputable,
        true,
    ),
    deferred_tool(
        "monitor_stop",
        "mcp__peri__monitor_stop",
        true,
        ContextRetention::SideEffectReceipt,
        true,
    ),
    deferred_tool(
        "monitor_list",
        "mcp__peri__monitor_list",
        true,
        ContextRetention::Recomputable,
        true,
    ),
];

pub(super) const WORKFLOW_CAPABILITY_TOOLS: &[CapabilityTool] = &[
    deferred_tool(
        "Workflow",
        "mcp__peri__Workflow",
        true,
        ContextRetention::PreviewOnly,
        false,
    ),
    deferred_tool(
        "WorkflowStatus",
        "mcp__peri__WorkflowStatus",
        true,
        ContextRetention::Recomputable,
        true,
    ),
    deferred_tool(
        "WorkflowKill",
        "mcp__peri__WorkflowKill",
        true,
        ContextRetention::SideEffectReceipt,
        false,
    ),
    deferred_tool(
        "WorkflowAgentResult",
        "mcp__peri__WorkflowAgentResult",
        false,
        ContextRetention::PreviewOnly,
        false,
    ),
    deferred_tool(
        "WorkflowAgentClaim",
        "mcp__peri__WorkflowAgentClaim",
        false,
        ContextRetention::PreviewOnly,
        false,
    ),
    deferred_tool(
        "WorkflowConsumerReady",
        "mcp__peri__WorkflowConsumerReady",
        false,
        ContextRetention::PreviewOnly,
        false,
    ),
    deferred_tool(
        "WorkflowCompletionAck",
        "mcp__peri__WorkflowCompletionAck",
        false,
        ContextRetention::PreviewOnly,
        false,
    ),
    deferred_tool(
        "WorkflowClose",
        "mcp__peri__WorkflowClose",
        false,
        ContextRetention::PreviewOnly,
        false,
    ),
    deferred_tool(
        "WorkflowTaskObserved",
        "mcp__peri__WorkflowTaskObserved",
        false,
        ContextRetention::PreviewOnly,
        false,
    ),
];
