//! Builtin MCP 注册表（**纯数据**，契约层）。
//!
//! 本模块只声明「已实现的 builtin 实例 / 原始工具名 / 模型面 effective name /
//! 逐工具 direct / 逐工具 prompt 声明模板 / 关闭策略键 / 保留实例名」，外加两个
//! 纯查表函数（[`find`] 与 [`original_tool_name_of_effective`]）。effective name 的
//! **计算**（sanitize + 模板）在 `peri-middlewares/src/mcp/builtin/`，本 crate 不得
//! 复刻 sanitize 规则、不得拼接或切分 `mcp__` 字符串：
//! 规则一份实现（middlewares），字面量一份声明（本文件），两者由
//! `cargo test -p peri-middlewares --lib -- mcp::builtin::tests` 的字面量测试锁定。
//!
//! 三分类概念（不要混淆）：
//! - **已实现实例**：[`BUILTIN_MCP_INSTANCES`]（wave 1 = `web` / `artifact`），
//!   可被 `TransportConfig::Builtin` 解析、可参与默认层注入。
//! - **保留实例名**：[`BUILTIN_RESERVED_INSTANCE_NAMES`] = 已实现实例 +
//!   后续波次预留（`cron` / `lsp` / `workspace`）。用户配置不得用 `command`/`url`
//!   接管任一保留名（加载期 typed error），否则会按名字反查继承「按原始名判定」
//!   的审批结果，静默移除 `mcp__*` 审批门。
//! - **归一表**：[`original_tool_name_of_effective`] 只索引本文件的冻结字面量，
//!   未命中（未知 / 外部 `mcp__*`）返回 `None` ⇒ 消费点沿用既有保守语义。

/// builtin 实例内的单个工具声明。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinMcpTool {
    /// 原始工具名（如 `WebSearch`）：配置、readiness 与 `system_mcp_tools`
    /// 都在**原始名**上精确匹配。
    pub original_name: &'static str,
    /// 冻结的模型面 effective name 字面量（如 `mcp__web__WebSearch`）。
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
        effective_name: "mcp__web__WebSearch",
        direct: true,
        prompt_declaration: Some(
            "Look up current information beyond your knowledge → `{{name}}` ({{title}}). Query the web for recent or external facts.",
        ),
    },
    BuiltinMcpTool {
        original_name: "WebFetch",
        effective_name: "mcp__web__WebFetch",
        direct: true,
        prompt_declaration: Some(
            "Fetch a URL you have reason to trust → `{{name}}` ({{title}}). Retrieve URLs with `{{name}}`, never `curl` via Bash.",
        ),
    },
];

/// `artifact` 实例的工具声明。
const ARTIFACT_TOOLS: &[BuiltinMcpTool] = &[BuiltinMcpTool {
    original_name: "artifact",
    effective_name: "mcp__artifact__artifact",
    direct: true,
    prompt_declaration: Some(
        "Share generated HTML/Markdown as a public link → `{{name}}` ({{title}}). \
         The link expires in 7 or 30 days; use for dashboards, reports, and prototypes you want to share.",
    ),
}];

/// 已实现的 builtin 实例（wave 1 = `web` / `artifact`）。
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
];

/// 保留实例名：已实现实例 + 后续波次预留（`cron` / `lsp` / `workspace`）。
///
/// 预留在本批次**不实现、不注入**；用户配置为任一保留名声明 `command` 或 `url`
/// 一律是加载期 typed error。
pub const BUILTIN_RESERVED_INSTANCE_NAMES: &[&str] =
    &["web", "artifact", "cron", "lsp", "workspace"];

/// 按实例身份解析**已实现**实例；预留但未实现的名字（如 `workspace`）返回 `None`。
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
/// 本函数**不得**包含 sanitize、大小写折叠或 `mcp__` 反拆逻辑。
pub fn original_tool_name_of_effective(effective: &str) -> Option<&'static str> {
    BUILTIN_MCP_INSTANCES
        .iter()
        .flat_map(|instance| instance.tools.iter())
        .find(|tool| tool.effective_name == effective)
        .map(|tool| tool.original_name)
}

#[cfg(test)]
#[path = "builtin_mcp_test.rs"]
mod tests;
