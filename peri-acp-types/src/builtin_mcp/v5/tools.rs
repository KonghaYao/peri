//! v5 能力工具声明（顺序即 `tools/list` 声明顺序）。
//!
//! 这是 builtin 工具名的**唯一手写声明**：`model_name` 与 `legacy_name` 由宏从
//! 同一组字面量派生，`validate_capabilities` 再按命名规则复核。迁移期旧名只用于
//! 历史展示/匹配，不是执行别名。

use crate::tools::ContextRetention;

use super::CapabilityTool;

/// direct 工具：模型面与迁移期旧名都使用原名。
macro_rules! direct {
    ($original:literal, $retention:expr, $completes:literal, $prompt:expr) => {
        CapabilityTool {
            original_name: $original,
            model_name: $original,
            legacy_name: $original,
            direct: true,
            model_visible: true,
            advertised: true,
            prompt_declaration: $prompt,
            context_retention: $retention,
            completes_on_response: $completes,
        }
    };
}

/// deferred 工具：v5 模型面 `mcp__peri__<tool>`，迁移期旧名 `mcp__<实例>__<tool>`。
macro_rules! deferred {
    ($instance:literal, $original:literal, $visible:literal, $retention:expr, $completes:literal) => {
        CapabilityTool {
            original_name: $original,
            model_name: concat!("mcp__peri__", $original),
            legacy_name: concat!("mcp__", $instance, "__", $original),
            direct: false,
            model_visible: $visible,
            advertised: true,
            prompt_declaration: None,
            context_retention: $retention,
            completes_on_response: $completes,
        }
    };
}

/// 仅宿主调用的工具：可执行、可路由，但不进入 wire `tools/list` 与模型面。
macro_rules! host_only {
    ($instance:literal, $original:literal, $retention:expr, $completes:literal) => {
        CapabilityTool {
            original_name: $original,
            model_name: concat!("mcp__peri__", $original),
            legacy_name: concat!("mcp__", $instance, "__", $original),
            direct: false,
            model_visible: false,
            advertised: false,
            prompt_declaration: None,
            context_retention: $retention,
            completes_on_response: $completes,
        }
    };
}

pub(super) const WORKSPACE_CAPABILITY_TOOLS: &[CapabilityTool] = &[
    direct!(
        "Read",
        ContextRetention::Recomputable,
        true,
        Some(
            "Read a file → `{{name}}` ({{title}}). Use `{{name}}` for file content, not `cat`/`head`/`tail`.",
        )
    ),
    direct!(
        "Write",
        ContextRetention::SideEffectReceipt,
        true,
        Some(
            "Write a file → `{{name}}` (full contents). Use `{{name}}` for writing files, not `echo >`/`sed`/`awk`.",
        )
    ),
    direct!(
        "Edit",
        ContextRetention::SideEffectReceipt,
        true,
        Some(
            "Edit a file → `{{name}}` (targeted diff). Use `{{name}}` for targeted edits, not `echo >`/`sed`/`awk`.",
        )
    ),
    direct!(
        "Glob",
        ContextRetention::Recomputable,
        true,
        Some(
            "Find files by name → `{{name}}` (e.g. `**/*.rs`, `*.config.json`). Use `{{name}}` for name search, not `Bash` with `find`; never `{{name}}(\"*\")`/`{{name}}(\"**/*\")` — that dumps the whole tree — list directories via `folder_operations` or `Bash ls`.",
        )
    ),
    direct!(
        "Grep",
        ContextRetention::Recomputable,
        true,
        Some(
            "Search file contents → `{{name}}` (regex, fast, scoped). Use `{{name}}` for content search, not `grep`/`rg`.",
        )
    ),
    direct!(
        "folder_operations",
        ContextRetention::Recomputable,
        true,
        Some(
            "List a directory / check structure → `{{name}}` (atomic, cross-platform, structured). Prefer `{{name}}` when entries are needed as data, `Bash ls -la` for quick one-shot human-readable output; avoid `mkdir`/`test -d` via `Bash`.",
        )
    ),
    direct!(
        "Bash",
        ContextRetention::PreviewOnly,
        false,
        Some(
            "Run a shell command → `{{name}}` ({{title}}). Prefer the purpose-built tools above when applicable: they give structured output and enforce permission rules.",
        )
    ),
    // 宿主恢复面（session-durability）：按 owner 查询存活 Shell 任务。
    // 仅由 handler 直接应答，不进入 wire 目录与模型面。
    host_only!(
        "workspace",
        "shell_task_list",
        ContextRetention::Recomputable,
        true
    ),
];

pub(super) const WEB_CAPABILITY_TOOLS: &[CapabilityTool] = &[
    direct!(
        "WebSearch",
        ContextRetention::Recomputable,
        true,
        Some(
            "Look up current information beyond your knowledge → `{{name}}` ({{title}}). Query the web for recent or external facts.",
        )
    ),
    direct!(
        "WebFetch",
        ContextRetention::Recomputable,
        true,
        Some(
            "Fetch a URL you have reason to trust → `{{name}}` ({{title}}). Retrieve URLs with `{{name}}`, never `curl` via Bash.",
        )
    ),
];

pub(super) const ARTIFACT_CAPABILITY_TOOLS: &[CapabilityTool] = &[direct!(
    "artifact",
    ContextRetention::SideEffectReceipt,
    true,
    Some(
        "Share generated HTML/Markdown as a public link → `{{name}}` ({{title}}). The link expires in 7 or 30 days; use for dashboards, reports, and prototypes you want to share.",
    )
)];

pub(super) const CRON_CAPABILITY_TOOLS: &[CapabilityTool] = &[
    deferred!(
        "cron",
        "cron_register",
        true,
        ContextRetention::SideEffectReceipt,
        true
    ),
    deferred!(
        "cron",
        "cron_list",
        true,
        ContextRetention::Recomputable,
        true
    ),
    deferred!(
        "cron",
        "cron_remove",
        true,
        ContextRetention::SideEffectReceipt,
        true
    ),
    deferred!(
        "cron",
        "cron_toggle",
        true,
        ContextRetention::SideEffectReceipt,
        true
    ),
    deferred!(
        "cron",
        "cron_acknowledge",
        false,
        ContextRetention::PreviewOnly,
        false
    ),
];

pub(super) const MONITOR_CAPABILITY_TOOLS: &[CapabilityTool] = &[
    deferred!(
        "monitor",
        "monitor_start",
        true,
        ContextRetention::SideEffectReceipt,
        false
    ),
    deferred!(
        "monitor",
        "monitor_read",
        true,
        ContextRetention::Recomputable,
        true
    ),
    deferred!(
        "monitor",
        "monitor_stop",
        true,
        ContextRetention::SideEffectReceipt,
        true
    ),
    deferred!(
        "monitor",
        "monitor_list",
        true,
        ContextRetention::Recomputable,
        true
    ),
];

pub(super) const WORKFLOW_CAPABILITY_TOOLS: &[CapabilityTool] = &[
    deferred!(
        "workflow",
        "Workflow",
        true,
        ContextRetention::PreviewOnly,
        false
    ),
    deferred!(
        "workflow",
        "WorkflowStatus",
        true,
        ContextRetention::Recomputable,
        true
    ),
    deferred!(
        "workflow",
        "WorkflowKill",
        true,
        ContextRetention::SideEffectReceipt,
        false
    ),
    deferred!(
        "workflow",
        "WorkflowAgentResult",
        false,
        ContextRetention::PreviewOnly,
        false
    ),
    deferred!(
        "workflow",
        "WorkflowAgentClaim",
        false,
        ContextRetention::PreviewOnly,
        false
    ),
    deferred!(
        "workflow",
        "WorkflowConsumerReady",
        false,
        ContextRetention::PreviewOnly,
        false
    ),
    deferred!(
        "workflow",
        "WorkflowCompletionAck",
        false,
        ContextRetention::PreviewOnly,
        false
    ),
    deferred!(
        "workflow",
        "WorkflowClose",
        false,
        ContextRetention::PreviewOnly,
        false
    ),
    deferred!(
        "workflow",
        "WorkflowTaskObserved",
        false,
        ContextRetention::PreviewOnly,
        false
    ),
];
