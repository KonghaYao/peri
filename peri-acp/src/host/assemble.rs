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

/// 进程内 host 的本地 Apps 授权层（仅 opt-in 装配注入；边界见模块文档）。
#[path = "assemble/local_mcp_apps_authorization.rs"]
mod local_mcp_apps_authorization;

/// 本地授权的唯一构造入口（装配与同 crate 测试共用；`crate` 外不可见）。
pub(crate) use local_mcp_apps_authorization::local_in_process_authorizer;

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

/// 进程内 host 的 MCP Apps 装配变体（TUI 本地 opt-in 的唯一落点）。
///
/// 与 [`assemble_server_config`] 的唯一差别是 `apps_enabled`：
/// - `false`（默认）与 [`assemble_server_config`] 逐字段相同：profile 关闭、
///   连接准入门关闭、不注入 entitlement 层；
/// - `true` 同时满足三道闸中的后两道——打开
///   `AcpServerConfig::mcp_apps_connection_enabled`（连接准入），并注入进程内
///   本地授权层（`session_authorization`）；第一道闸（Apps profile）由
///   `deployment_profile(true)` 协商出 HTML MIME。
///
/// **不得**为了 Apps 去置 `stdio_command_filter`：那是 stdio 部署的命令过滤语义
/// （过滤 rewind/clear），TUI 必须保持 `false`。安全边界见
/// `local_mcp_apps_authorization`（仅进程内 MPSC 传输、默认拒绝、不用于可被
/// 外部客户端接入的部署）。
pub async fn assemble_server_config_with_local_mcp_apps(
    input: HostAssemblyInput,
    apps_enabled: bool,
) -> AcpServerConfig {
    // 第一道闸复用同一 profile 派生（与 stdio 变体同源：`deployment_profile`），
    // 避免第二份 MIME 协商实现；本函数只额外打开进程内 host 的两道本地闸。
    let mut cfg = assemble_server_config_with_mcp_apps(input, apps_enabled).await;
    // 复用不变量：本地 Apps 变体永远不得继承 stdio 的命令过滤语义（rewind/clear
    // 必须保持 TUI 既有行为）。若未来 stdio 变体改动波及此处，debug 构建即暴露。
    debug_assert!(
        !cfg.stdio_command_filter,
        "local Apps variant must not enable stdio command filtering"
    );
    if apps_enabled {
        cfg.mcp_apps_connection_enabled = true;
        cfg.session_authorization = Some(local_in_process_authorizer());
    }
    cfg
}

pub(crate) use host_capabilities::assemble_server_config_with_mcp_profile;

#[cfg(test)]
#[path = "assemble_test.rs"]
mod tests;

#[path = "assemble/host_capabilities.rs"]
mod host_capabilities;
