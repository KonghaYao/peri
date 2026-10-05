//! Builtin MCP 注册表（**纯数据**，契约层）。
//!
//! 本模块只声明「已实现的 builtin 实例 / 原始工具名 / 模型面 effective name /
//! 逐工具 direct / 逐工具 prompt 声明模板 / 关闭策略键 / 保留实例名」，外加两个
//! 纯查表函数（[`find`] 与 [`original_tool_name_of_effective`]）。direct 工具使用原名，deferred effective name 的
//! **计算**（sanitize + 模板）在 `peri-middlewares/src/mcp/builtin/`，本 crate 不得
//! 复刻 sanitize 规则；历史名字归一只匹配本文件已声明的实例与原始名：
//! 规则一份实现（middlewares），字面量一份声明（本文件），两者由
//! `cargo test -p peri-middlewares --lib -- mcp::builtin::tests` 的字面量测试锁定。
//!
//! 三分类概念（不要混淆）：
//! - **已实现实例**：[`BUILTIN_MCP_INSTANCES`]（wave 1 = `web` / `artifact`，
//!   wave 2 增 `cron`，wave 3 增 `workspace`），可被
//!   `TransportConfig::Builtin` 解析、可参与默认层注入。
//! - **保留实例名**：[`BUILTIN_RESERVED_INSTANCE_NAMES`] = 已实现实例 +
//!   后续波次预留。当前四个保留名全部已实现（无「预留但未实现」的名字）。
//!   用户配置不得用 `command`/`url` 接管任一保留名（加载期 typed error），
//!   否则会按名字反查继承「按原始名判定」的审批结果，静默移除 `mcp__*` 审批门。
//! - **归一表**：[`original_tool_name_of_effective`] 只索引本文件的冻结字面量，
//!   未命中（未知 / 外部 `mcp__*`）返回 `None` ⇒ 消费点沿用既有保守语义。

/// builtin 实例内的单个工具声明。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinMcpTool {
    /// 原始工具名（如 `WebSearch`）：配置、readiness 与 `system_mcp_tools`
    /// 都在**原始名**上精确匹配。
    pub original_name: &'static str,
    /// 模型面名称（direct 使用原名，如 `WebSearch`）。
    /// 必须与 `mcp::builtin::effective_tool_name()` 的输出逐字相等（由
    /// `mcp::builtin::tests` 锁定；本 crate 不复刻该计算）。
    pub effective_name: &'static str,
    /// 直连性声明：是否在类型化 bridge 构造点直接进入模型 tools 参数。
    /// 与实例的 `system_mcp_tools` 集合必须一致（同一来源）。
    pub direct: bool,
    /// 提示词层声明模板（逐字搬运既有 `BaseTool::prompt_declaration()` 文本）；
    /// `{{name}}` / `{{title}}` 的渲染仍由 `tool_search/declaration.rs` 负责。
    pub prompt_declaration: Option<&'static str>,
}

/// 一个已实现的 builtin MCP 实例声明。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinMcpInstance {
    /// server name / 配置 key / 目录键（`web` / `artifact`）。
    pub name: &'static str,
    /// `TransportConfig::Builtin.instance` 身份（当前恒等于 [`Self::name`]；
    /// 独立字段以便将来改 server name 而不改身份）。
    pub instance: &'static str,
    /// MetaHarness 关闭键（`WebMiddleware` / `ArtifactMiddleware`）；
    /// 与 `BUILTIN_INSTANCE_POLICY_KEYS` 集合相等。
    pub policy_key: &'static str,
    /// 实例工具表（非空；名字与 effective name 在实例内唯一）。
    pub tools: &'static [BuiltinMcpTool],
}

/// `web` 实例的工具声明（顺序即声明段顺序）。
const WEB_TOOLS: &[BuiltinMcpTool] = &[
    BuiltinMcpTool {
        original_name: "WebSearch",
        effective_name: "WebSearch",
        direct: true,
        prompt_declaration: Some(
            "Look up current information beyond your knowledge → `{{name}}` ({{title}}). Query the web for recent or external facts.",
        ),
    },
    BuiltinMcpTool {
        original_name: "WebFetch",
        effective_name: "WebFetch",
        direct: true,
        prompt_declaration: Some(
            "Fetch a URL you have reason to trust → `{{name}}` ({{title}}). Retrieve URLs with `{{name}}`, never `curl` via Bash.",
        ),
    },
];

/// `artifact` 实例的工具声明。
const ARTIFACT_TOOLS: &[BuiltinMcpTool] = &[BuiltinMcpTool {
    original_name: "artifact",
    effective_name: "artifact",
    direct: true,
    prompt_declaration: Some(
        "Share generated HTML/Markdown as a public link → `{{name}}` ({{title}}). \
         The link expires in 7 or 30 days; use for dashboards, reports, and prototypes you want to share.",
    ),
}];

/// `cron` 实例的工具声明（顺序即声明段顺序）。
///
/// 三个工具一律 deferred（`direct: false`）且无提示词声明模板（A4：声明段零变化）；
/// `cron_register` / `cron_list` / `cron_remove` 的在用执行实现是
/// `mcp-packages/cron/src/tools.rs` 的同名 `BaseTool`，由 `CronMcpServer` 映射。
const CRON_TOOLS: &[BuiltinMcpTool] = &[
    BuiltinMcpTool {
        original_name: "cron_register",
        effective_name: "mcp__cron__cron_register",
        direct: false,
        prompt_declaration: None,
    },
    BuiltinMcpTool {
        original_name: "cron_list",
        effective_name: "mcp__cron__cron_list",
        direct: false,
        prompt_declaration: None,
    },
    BuiltinMcpTool {
        original_name: "cron_remove",
        effective_name: "mcp__cron__cron_remove",
        direct: false,
        prompt_declaration: None,
    },
];

/// `workspace` 实例的工具声明（7 项；顺序即声明段顺序）。
///
/// wave 3：7 个本地工具由 middleware 直供迁移为实例提供，实例内**包装**既有
/// `BaseTool` 实现（AW3-02），因此 `is_direct()` 恒真的 7 项一律 `direct: true`
/// （AW3-03：迁移前即在首个 LLM 请求的直连工具表内）。
/// `prompt_declaration` 逐字搬运各工具实现的 `BaseTool::prompt_declaration()`
/// （6 个文件工具在 `mcp-packages/workspace/src/filesystem/{read,write,edit,glob,grep,folder}.rs`，
/// `Bash` 在 `mcp-packages/workspace/src/terminal.rs`）；`{{name}}` / `{{title}}`
/// 的渲染仍归 `tool_search/declaration.rs`，本表不得改写模板文本。
const WORKSPACE_TOOLS: &[BuiltinMcpTool] = &[
    BuiltinMcpTool {
        original_name: "Read",
        effective_name: "Read",
        direct: true,
        prompt_declaration: Some(
            "Read a file → `{{name}}` ({{title}}). Use `{{name}}` for file content, not `cat`/`head`/`tail`.",
        ),
    },
    BuiltinMcpTool {
        original_name: "Write",
        effective_name: "Write",
        direct: true,
        prompt_declaration: Some(
            "Write a file → `{{name}}` (full contents). Use `{{name}}` for writing files, not `echo >`/`sed`/`awk`.",
        ),
    },
    BuiltinMcpTool {
        original_name: "Edit",
        effective_name: "Edit",
        direct: true,
        prompt_declaration: Some(
            "Edit a file → `{{name}}` (targeted diff). Use `{{name}}` for targeted edits, not `echo >`/`sed`/`awk`.",
        ),
    },
    BuiltinMcpTool {
        original_name: "Glob",
        effective_name: "Glob",
        direct: true,
        prompt_declaration: Some(
            "Find files by name → `{{name}}` (e.g. `**/*.rs`, `*.config.json`). Use `{{name}}` for name search, not `Bash` with `find`; never `{{name}}(\"*\")`/`{{name}}(\"**/*\")` — that dumps the whole tree — list directories via `folder_operations` or `Bash ls`.",
        ),
    },
    BuiltinMcpTool {
        original_name: "Grep",
        effective_name: "Grep",
        direct: true,
        prompt_declaration: Some(
            "Search file contents → `{{name}}` (regex, fast, scoped). Use `{{name}}` for content search, not `grep`/`rg`.",
        ),
    },
    BuiltinMcpTool {
        original_name: "folder_operations",
        effective_name: "folder_operations",
        direct: true,
        prompt_declaration: Some(
            "List a directory / check structure → `{{name}}` (atomic, cross-platform, structured). Prefer `{{name}}` when entries are needed as data, `Bash ls -la` for quick one-shot human-readable output; avoid `mkdir`/`test -d` via `Bash`.",
        ),
    },
    BuiltinMcpTool {
        original_name: "Bash",
        effective_name: "Bash",
        direct: true,
        prompt_declaration: Some(
            "Run a shell command → `{{name}}` ({{title}}). Prefer the purpose-built tools above when applicable: they give structured output and enforce permission rules.",
        ),
    },
];

/// 已实现的 builtin 实例（wave 1 = `web` / `artifact`；wave 2 增 `cron`；
/// wave 3 增 `workspace`）。
///
/// 这是「实例 / 原始工具名 / effective name / 逐工具 `direct` / `prompt_declaration` /
/// 关闭键」的**唯一事实源**：关闭集判定、默认层注入、`system_mcp_tools`、归一表与
/// 全部相关测试都从它派生或与它对齐。
pub const BUILTIN_MCP_INSTANCES: &[BuiltinMcpInstance] = &[
    BuiltinMcpInstance {
        name: "web",
        instance: "web",
        policy_key: "WebMiddleware",
        tools: WEB_TOOLS,
    },
    BuiltinMcpInstance {
        name: "artifact",
        instance: "artifact",
        policy_key: "ArtifactMiddleware",
        tools: ARTIFACT_TOOLS,
    },
    BuiltinMcpInstance {
        name: "cron",
        instance: "cron",
        policy_key: "CronMiddleware",
        tools: CRON_TOOLS,
    },
    BuiltinMcpInstance {
        name: "workspace",
        instance: "workspace",
        policy_key: "WorkspaceMiddleware",
        tools: WORKSPACE_TOOLS,
    },
];

/// 保留实例名：已实现实例 + 后续波次预留。
///
/// 当前四个保留名全部已实现（无「预留但未实现」的名字）；用户配置为任一
/// 保留名声明 `command` 或 `url` 一律是加载期 typed error。
pub const BUILTIN_RESERVED_INSTANCE_NAMES: &[&str] = &["web", "artifact", "cron", "workspace"];

/// 按实例身份解析**已实现**实例；未登记的名字（含后续波次的预留名）返回 `None`。
pub fn find(instance: &str) -> Option<&'static BuiltinMcpInstance> {
    BUILTIN_MCP_INSTANCES
        .iter()
        .find(|candidate| candidate.instance == instance)
}

/// 名字是否为保留实例名（已实现或预留）。
pub fn is_reserved_instance_name(name: &str) -> bool {
    BUILTIN_RESERVED_INSTANCE_NAMES.contains(&name)
}

/// effective name → 原始工具名（IF-D15，A4）：**纯查表**，只索引冻结字面量。
///
/// 这是全部按名消费点（判定型 / 匹配型）的唯一归一入口：命中返回原始名，
/// 未命中（未知或外部 `mcp__*`）返回 `None`，消费点沿用既有保守语义。
/// 仅识别声明表中实例与原始名的精确组合；不做 sanitize 或大小写折叠。
pub fn original_tool_name_of_effective(effective: &str) -> Option<&'static str> {
    BUILTIN_MCP_INSTANCES.iter().find_map(|instance| {
        instance.tools.iter().find_map(|tool| {
            // Persisted transcripts and user filters may still contain the previous
            // namespaced spelling. This is normalization, never an execution alias.
            let namespaced = effective
                .strip_prefix("mcp__")
                .and_then(|name| name.strip_prefix(instance.name))
                .and_then(|name| name.strip_prefix("__"));
            (tool.effective_name == effective || namespaced == Some(tool.original_name))
                .then_some(tool.original_name)
        })
    })
}

/// 原始工具名 → effective name（[`original_tool_name_of_effective`] 的正方向）。
///
/// 面向模型/用户的指引文案指代 builtin 工具时**必须**用模型面名字（system direct 使用原名，deferred 保留 MCP 命名空间），因此消费点经本函数
/// 取名字，不得再写第二份字面量。命中返回冻结字面量；`instance` 未登记或该实例无此
/// 原始名时返回 `None`——消费点须兜底为**不含工具名**的等价表述，不得回落到裸名。
/// 纯查表：不含 sanitize、大小写折叠或 `mcp__` 反拆。
pub fn effective_name_of(instance: &str, original_name: &str) -> Option<&'static str> {
    find(instance)?
        .tools
        .iter()
        .find(|tool| tool.original_name == original_name)
        .map(|tool| tool.effective_name)
}

#[cfg(test)]
#[path = "builtin_mcp_test.rs"]
mod tests;
