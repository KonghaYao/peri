//! ACP Host 装配——TUI / print / stdio 三路径共用的 host 装配函数。
//!
//! 3.0 目标（`docs/top-level.md` §7/§8）：ACP Host = 部署单元，由 cli/TUI 作为
//! 部署装配点启动；客户端只经 ACP 拿数据。本模块收拢三处此前各自复制的主机
//! 装配（`launch.rs` 内嵌 server 装配、`cli_print.rs` 业务装配、stdio init），
//! 避免装配逻辑漂移。中间件链序事实源仍为 Agent 层 session 工厂
//! （ARC-MIDDLEWARE-001）：本模块只组装 hook 组（顺序与迁移前一致），
//! 不参与链序蓝本。

use std::sync::Arc;

#[path = "hook_groups.rs"]
mod hook_groups;

use parking_lot::RwLock;
use peri_acp_types::command::command_route::RouteEntry;
use peri_acp_types::cron::CronSchedulerPort;
use peri_acp_types::hooks::{RegisteredHook, SettingsHooksPort};
use peri_acp_types::mcp::McpSubscriptionPort;
use peri_acp_types::permission::SharedPermissionMode;
use peri_acp_types::plugin::{PluginLoadResult, PluginManagerPort};
use peri_acp_types::ports::{AgentCatalogPort, McpPoolPort, ToolSearchPort};
use peri_acp_types::session_resources::{SessionResources, SessionStoreShutdownPort};
use peri_acp_types::skills::SkillRoot;

use crate::provider::{LlmProvider, PeriConfig};
use crate::session::SessionManager;

use super::task_scope::{HostTaskKind, HostTaskOwner, HostTaskOwnerKind};
use super::AcpServerConfig;

pub(crate) use host_capabilities::WorkspaceAssembly;

pub use host_capabilities::HostCapabilities;

pub(crate) use host_capabilities::discover_prepared_plugins;

pub use host_capabilities::PreparedPlugins;

pub use host_capabilities::HostAssemblyInput;

#[cfg(target_os = "emscripten")]
pub use host_capabilities::WasmHostAssemblyInput;

#[cfg(target_os = "emscripten")]
pub use host_capabilities::assemble_wasm_server_config;

pub(super) use host_capabilities::build_session_end_task;

/// 组装 settings hook 组（实现见 `host/hook_groups.rs`：plugin → global →
/// project → local，顺序即迁移前 TUI/print/stdio 三处一致的既有顺序）。
pub use hook_groups::assemble_hook_groups;

pub use host_capabilities::build_session_manager;

pub use host_capabilities::assemble_server_config;

pub use host_capabilities::assemble_server_config_with_capabilities;

pub use host_capabilities::assemble_server_config_with_mcp_apps;

pub(crate) use host_capabilities::assemble_server_config_with_mcp_profile;

#[cfg(test)]
#[path = "assemble_test.rs"]
mod tests;

#[path = "assemble/host_capabilities.rs"]
mod host_capabilities;
