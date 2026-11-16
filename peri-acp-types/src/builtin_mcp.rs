//! Builtin MCP 能力声明（**纯数据**，契约层）。
//!
//! **唯一手写声明源**是 [`v5::BUILTIN_V5_CAPABILITIES`]：能力、工具、模型面名、
//! 迁移期旧名、资源面、宿主注入、执行绑定与旧关闭键都只在那里声明。本模块只保留
//! 两类派生物：
//!
//! - **迁移期 v4 兼容视图**：[`BUILTIN_MCP_INSTANCES`] / [`BUILTIN_RESERVED_INSTANCE_NAMES`]
//!   由能力表投影，供尚未切换的六实例注入、关闭集与 standalone 路径使用；M4 清理时
//!   与 v4 路径一并删除。
//! - **旧名非执行归一**：[`original_tool_name_of_effective`] 与
//!   [`legacy_display_name_of`] 只服务旧 transcript / allowlist / 展示匹配；旧前缀永远
//!   不是执行别名。v5 模型面名查询走 [`model_tool_name_of`]。
//!
//! 名字**计算**（MCP sanitize + `mcp__{server}__{tool}` 模板）只在
//! `peri-middlewares/src/mcp/tool_bridge.rs`；本 crate 只声明字面量与命名规则校验，
//! 两者由 `peri-middlewares` 的 `mcp::builtin::tests` 对齐。

use std::sync::LazyLock;

use v5::CapabilityTool;

/// 迁移期 v4 工具视图（字段与 v5 声明一致；`legacy_name` 只用于旧展示/匹配）。
pub type BuiltinMcpTool = CapabilityTool;

/// 迁移期 v4 实例视图：由 [`BUILTIN_V5_CAPABILITIES`] 投影，不是第二份声明。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinMcpInstance {
    /// 能力名 / v4 server name（两者在迁移期同值）。
    pub name: &'static str,
    /// `TransportConfig::Builtin.instance` 身份（迁移期等于能力名）。
    pub instance: &'static str,
    /// 是否按直接发起会话隔离后台任务与资源。
    pub task_owner_scoped: bool,
    /// 能力工具表（含 host-only 工具；v4 消费方按 `advertised` 过滤）。
    pub tools: &'static [BuiltinMcpTool],
}

/// 迁移期 v4 实例视图（顺序即能力表顺序）。
///
/// **兼容期专用**：默认注入、关闭集与 standalone 仍消费它；所有字段都来自
/// [`BUILTIN_V5_CAPABILITIES`]，不另存字面量。M4 删除。
pub static BUILTIN_MCP_INSTANCES: LazyLock<Vec<BuiltinMcpInstance>> = LazyLock::new(|| {
    BUILTIN_V5_CAPABILITIES
        .iter()
        .map(|capability| BuiltinMcpInstance {
            name: capability.name,
            instance: capability.legacy_instance,
            task_owner_scoped: capability.host_injection.task_owner_scoped,
            tools: capability.tools,
        })
        .collect()
});

/// 迁移期保留实例名（由能力表 `legacy_instance` 派生）。
///
/// 用户配置不得用 `command`/`url` 接管任一保留名（加载期 typed error）。
pub static BUILTIN_RESERVED_INSTANCE_NAMES: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    BUILTIN_V5_CAPABILITIES
        .iter()
        .map(|capability| capability.legacy_instance)
        .collect()
});

/// 按迁移期旧实例名解析 v4 兼容视图；未登记返回 `None`。
///
/// v5 消费方应使用 [`v5::capability`] 取能力声明；本函数只服务兼容期路径与旧测试。
pub fn find(instance: &str) -> Option<&'static BuiltinMcpInstance> {
    BUILTIN_MCP_INSTANCES
        .iter()
        .find(|candidate| candidate.instance == instance || candidate.name == instance)
}

/// 名字是否为迁移期保留实例名。
pub fn is_reserved_instance_name(name: &str) -> bool {
    BUILTIN_RESERVED_INSTANCE_NAMES.contains(&name)
}

pub use v5::{
    capability, capability_of_legacy_instance, capability_of_original_tool,
    capability_of_resource_uri, capability_tool, is_reserved_server_name, is_v5_server_name,
    BUILTIN_V5_CAPABILITIES, BUILTIN_V5_CAPABILITIES_ENV, BUILTIN_V5_SERVER_NAME,
};

/// 名字是否为 builtin server（迁移期六实例或 v5 单出口 `peri`）。
pub fn is_builtin_server_name(name: &str) -> bool {
    is_v5_server_name(name) || find(name).is_some()
}

/// 旧模型面名字 → 原始工具名（纯查表）：**只用于展示 / 过滤匹配，不是执行别名**。
///
/// 接受三类精确拼写，均不折叠大小写：
/// 1. v5 模型面名：direct 原名、deferred `mcp__peri__<original>`；
/// 2. 迁移期旧名：`mcp__<legacy_instance>__<original>`（direct 工具亦可有此历史拼写）；
/// 3. direct 原名（新旧同名）。
///
/// 未命中（未知或外部 `mcp__*`）返回 `None`，消费点沿用既有保守语义。
pub fn original_tool_name_of_effective(effective: &str) -> Option<&'static str> {
    for capability in BUILTIN_V5_CAPABILITIES {
        for tool in capability.tools {
            if effective == tool.model_name || effective == tool.legacy_name {
                return Some(tool.original_name);
            }
            if let Some(rest) = effective
                .strip_prefix("mcp__")
                .and_then(|rest| rest.strip_prefix(capability.legacy_instance))
                .and_then(|rest| rest.strip_prefix("__"))
            {
                if rest == tool.original_name {
                    return Some(tool.original_name);
                }
            }
        }
    }
    None
}

/// 迁移期旧模型面名字查询（展示 / 匹配）：`(旧实例, 原始名)` → 旧名。
///
/// 未命中返回 `None`；调用方兜底为不含工具名的表述，不得回落裸名。v5 模型面
/// 名字请用 [`model_tool_name_of`]。
pub fn legacy_display_name_of(instance: &str, original_name: &str) -> Option<&'static str> {
    find(instance)?
        .tools
        .iter()
        .find(|tool| tool.original_name == original_name)
        .map(|tool| tool.legacy_name)
}

/// v5 模型面名字查询：`(能力名, 原始名)` → `mcp__peri__*` 或 direct 原名。
///
/// 面向模型/用户的指引文案在 v5 路径使用本函数；迁移期路径仍用
/// [`legacy_display_name_of`] 以匹配模型实际看到的旧名。
pub fn model_tool_name_of(capability_name: &str, original_name: &str) -> Option<&'static str> {
    v5::capability(capability_name)?
        .tools
        .iter()
        .find(|tool| tool.original_name == original_name)
        .map(|tool| tool.model_name)
}

/// v5 单出口能力声明源（默认装配尚未切换；迁移期由本模块投影为 v4 实例视图）。
pub mod v5;

#[cfg(test)]
#[path = "builtin_mcp_test.rs"]
mod tests;
