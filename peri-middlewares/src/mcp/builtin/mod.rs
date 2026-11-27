//! Builtin MCP 行为层（**唯一行为实现**）：effective name 计算、默认层注入与覆盖、
//! 注入策略、关闭集、直连性判定、声明模板查询。
//!
//! 数据侧（实例 / 原始工具名 / effective name 字面量 / 逐工具 `direct` /
//! `prompt_declaration` / 保留名表 / 归一表）在 `peri_acp_types::builtin_mcp`。
//! 名字规则由 `tool_bridge` 维护；`mcp::builtin::tests` 将实际规则与声明表的
//! effective name 字面量逐项对齐。
//!
//! 配置注入统一由 `mcp/config.rs` 完成：普通 loader 的 step 6.5 与 bare 的 workspace 子集；注入策略是**显式参数**，
//! 本模块（含 `apply_builtin_overlay`）**禁止**读 env——env 只在
//! [`builtin_injection_policy_from_env`] 读一次，由配置加载入口调用。
//!
//! Builtin handler 行为由 `mcp-packages/*` 提供，本模块保留策略和宿主生命周期。

// context：宿主装配注入的实例上下文载体（`BuiltinInstanceContext` / 实例输入 /
// 一次性注入的 typed 错误，owner H-02，W2）。它是「实例构造所需状态」的**唯一**载体：
// spawn 点只从它取 cron scheduler / cwd，不再逐参数透传；公开可见性只经
// `crate::assembly` 再导出（本模块自身仍是 `pub(crate)`）。
pub(crate) mod context;

// runtime：builtin 同进程链路（duplex + `rmcp::serve_server`）、server task 归属与
// 有界关闭（owner E-03，W2）。与两个 handler 一样是「行为」子模块，由本模块统一挂载。
pub(crate) mod runtime;

// standalone：进程内 CLI serving（`peri mcp-start <instance>`）的 handler 构造。
pub(crate) mod standalone;

// router：v5 单出口（`peri`）聚合与能力级隔离。
pub(crate) mod router;

// workspace_subscription：`workspace` 实例的默认订阅（git ref 资源；Git Watch 下沉的
// 宿主落点，D-5）。默认层注入只在本模块的 `builtin_default_entry` / 规则 2 消费它；
// 客户端消费侧（`mcp::client::subscription`）经下面的再导出判定「哪些实例的哪些 URI
// 有宿主内置提醒映射」，避免第二份实例名字面量。
mod workspace_subscription;

pub(crate) use workspace_subscription::default_subscriptions_for;

use std::collections::{BTreeSet, HashMap, HashSet};

use peri_acp_types::builtin_mcp;
use peri_acp_types::builtin_mcp::BuiltinMcpInstance;
use peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES;
use peri_acp_types::plugin::{ConfigSource, McpServerConfig};
use thiserror::Error;

#[cfg(test)]
#[path = "builtin_test.rs"]
mod tests;

// 实例名 → 独立插件 handler 的工厂；runtime 是宿主 task 的唯一入口。
mod dispatch;

mod builtin_registry;
pub(crate) use builtin_registry::apply_builtin_overlay;
pub(crate) use builtin_registry::builtin_injection_policy_from_env;
pub(crate) use builtin_registry::capability_closed;
pub(crate) use builtin_registry::closed_capabilities;
pub(crate) use builtin_registry::closed_instances;
pub(crate) use builtin_registry::enabled_capabilities;
pub(crate) use builtin_registry::is_capability_closed;
pub(crate) use builtin_registry::is_closed;
pub(crate) use builtin_registry::is_closed_builtin_source;
pub(crate) use builtin_registry::is_closed_source;
pub(crate) use builtin_registry::is_declared_direct;
pub(crate) use builtin_registry::is_declared_model_hidden;
pub(crate) use builtin_registry::is_local_resource_source;
pub(crate) use builtin_registry::is_resource_closed;
pub(crate) use builtin_registry::is_tool_closed;
pub(crate) use builtin_registry::is_workspace_source;
pub(crate) use builtin_registry::visible_required_tools;
pub(crate) use builtin_registry::visible_subscription_resources;
pub(crate) use builtin_registry::BuiltinInjectionPolicy;
pub(crate) use builtin_registry::BuiltinOverlayError;
#[cfg(test)]
pub(crate) use builtin_registry::BUILTIN_INJECTION_ENV;
