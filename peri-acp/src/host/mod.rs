//! ACP Host — transport-agnostic request handler（自 peri-tui 迁出归位）。
//!
//! Accepts any [`crate::transport::AcpTransport`] implementation (mpsc for TUI, stdio for IDE),
//! builds and executes ReAct agents, and pushes [`agent_client_protocol::schema::v1::SessionUpdate`] notifications
//! back through the transport. ACP Host = 部署单元（`docs/top-level.md` §7/§19）：
//! 由 cli/TUI 作为部署装配点启动，TUI 进程不再持有控制面。
//!
//! Prompt execution is owned by a background task. Sessions are shared via
//! `Arc<tokio::sync::Mutex<HashMap>>`.
//!
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Arc;

pub use crate::session::state_builders::{
    apply_profile_effort, apply_thinking_effort, build_config_options, build_mode_state,
    parse_permission_mode,
};
use peri_acp_types::command::command_route::RouteEntry;
use peri_acp_types::cron::CronSchedulerPort;
use peri_acp_types::hooks::SettingsHooksPort;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::permission::SharedPermissionMode;
use peri_acp_types::plugin::PluginManagerPort;
use peri_acp_types::ports::{AgentCatalogPort, McpPoolPort, McpTaskOwnerPort, ToolSearchPort};
use tokio_util::sync::CancellationToken;

use crate::provider::{LlmProvider, PeriConfig};
use history_window::HistoryWindowCoordinator;

pub mod assemble;
pub(crate) mod compact_config;
mod connection_context;
use connection_context as connection;
mod lifecycle;
mod workspace;
/// session 级交互装配（idle 准入规则链 / 交互门取用）；见模块文档。
mod workspace_interaction;
#[cfg(not(target_os = "emscripten"))]
mod workspace_resources;
pub use lifecycle::{spawn_acp_server, AcpHostHandle, AcpHostShutdownReport};
// 关闭报告的遥测载荷：`AcpHostShutdownReport::TelemetryFailed` 携带的终态与计数快照。
// 部署面（TUI）必须能命名它们，才能把「遥测未收敛」与业务失败分开，而调用方不该为此
// 直依赖 peri-controller / langfuse-client（同 `lib.rs` 的 LangfuseSessionLike re-export）。
pub use langfuse_client::DeliveryCounts;
pub use peri_controller::langfuse::LangfuseShutdownReport;
mod activation;
mod continuation;
pub mod controller_ports;
mod response_diagnostics;
use response_diagnostics as diagnostics;
mod execution;
#[cfg(test)]
#[path = "executor_flow_test.rs"]
mod executor_flow_tests;
mod history_page;
/// 连接级 History V2 协议服务（H2）：窗口、幂等、cursor、预算与后端 port。
mod history_service;
/// History V2 own 历史适配器（H3a1）：复用窗口读取端口，不开新表/列。
mod history_v2_adapter;
/// `session/load` 的 History V2 initial 准入（H4b）：生命周期装配前解析与拒绝。
mod history_v2_load;
/// `peri.historyWindowV1` 的 Host coordinator 与 open/page/seek 路由。
mod history_window;
/// session-owned 交互路由（按可信归属选审批 endpoint；投递失败原因的诚实产出点：
/// `NoOnlineEndpoint` / `OwnerUnmatched` / `AttachmentDetached`，见 `interaction_router.rs`）。
mod interaction_router;
/// 连接级实时投递注册表（F2a）：暂停缓冲、序号与关闭语义。
pub(crate) mod live_delivery;
/// URL 模式 MCP elicitation 的会话级投递面（端口 → 本会话交互 router）。
mod mcp_url_elicitation;

/// MCP Apps facade（七个 `peri/mcp/apps/*` 方法）与请求账本（设计 4a §1.3/§4.4）。
mod apps_facade;
mod apps_ledger;
#[cfg(test)]
#[path = "executor_flow_beta_flag_test.rs"]
mod executor_flow_beta_flag_tests;
mod mcp_apps;
#[cfg(test)]
#[path = "mcp_apps_idle_test.rs"]
mod mcp_apps_idle_tests;
// V-02（W4）的 host seam 断言：首个 LLM 请求的三个冻结 effective name、能力关闭的
// 首个请求面、`PERI_MCP_BUILTIN=off` 语义、启动 fatal 投影、BLOCKED 缺口复证。
// 模块名参与 `cargo test` 过滤（`host::mcp_v4_builtin`），故不沿用 `mod tests`。
#[cfg(test)]
#[path = "mcp_v4_builtin_test.rs"]
mod mcp_v4_builtin;
#[cfg(test)]
#[path = "mcp_v4_startup_test.rs"]
mod mcp_v4_startup_tests;
// wave 1 的 host 侧 wire 夹具（主 plan A10）：node 脚本含 wire 日志与 `tools/call`
// 分支 + 复刻的工具调用 model 替身。owner 序列 V-06（W0 建 + 录迁移前基线）→
// V-02（W4 复用）。模块名参与 `cargo test` 过滤（`host::mcp_v4_wire_fixture`），
// 故不沿用 `mod tests`。文件名必须保留 `_test.rs` 后缀：本夹具经引用业务 crate
// （`peri_middlewares` / `peri_model`）验证行为，靠 `scripts/check-layer-imports.sh`
// 的测试文件豁免（`*_test.rs`）才不构成越层 import。
#[cfg(test)]
#[path = "mcp_v4_wire_fixture_test.rs"]
mod mcp_v4_wire_fixture;
// wave 2 的**终态**用例（H-04 / V-03）：自带「生产同构的 builtin host」夹具 —— pool →
// `BuiltinInstanceContext` 注入 → `run_initialize`（A33 的顺序），并驱动真实装配面
// （`assemble_server_config`）验证配置合并早于 handler 构造、多 cwd 退化登记与
// host shutdown 有界关闭。
#[cfg(test)]
#[path = "mcp_v4_wave2_test.rs"]
mod mcp_v4_wave2;
mod notify;
mod oauth_delivery;
mod ownership;
mod prediction;
mod prediction_projection;
mod prepared_session_inputs;
use prepared_session_inputs as prepared;
/// 宿主遥测装配（暂存能力注入 + 关闭结果保有规则），见模块文档。
mod host_telemetry;
#[cfg(test)]
#[path = "prepared_test.rs"]
mod prepared_tests;
mod prompt;
mod prompt_dispatch;
pub mod prompt_handle;
/// 进入会话时的恢复（设计 §3/§4）：范围、实例判定、提醒接纳与延后启动。
mod recovery;
mod requests;
mod server_loop;
mod session_endpoint;
mod shutdown;
pub mod stage_builder;
#[cfg(not(target_os = "emscripten"))]
pub mod stdio;
mod task_scope;
use host_telemetry as telemetry;
/// Workflow agent 装配面薄壳（session 级运行时创建，见模块文档）。
/// `not(emscripten)`：本地 workflow 运行时随 `peri-mcp-workflow` 目标依赖移出
/// 构建面（wasm 产物不打包 workflow），该目标不提供 workflow 装配面。
#[cfg(not(target_os = "emscripten"))]
pub mod session_workflow_chain;
#[cfg(test)]
#[path = "unify_wire_baseline_test.rs"]
mod unify_wire_baseline_tests;
mod user_input;
#[cfg(not(target_os = "emscripten"))]
pub use session_workflow_chain as workflow_agent;
/// 宿主侧 workflow 任务消费者（P3 就绪与订阅接线，design `workflow-mcp` §5.5）。
/// `not(emscripten)`：依赖 builtin `workflow` 实例面（同目标段依赖 `peri-mcp-workflow`）。
#[cfg(not(target_os = "emscripten"))]
pub(crate) mod workflow_consumer;
#[cfg(all(test, not(target_os = "emscripten")))]
#[path = "workflow_consumer_test.rs"]
mod workflow_consumer_tests;
#[cfg(not(target_os = "emscripten"))]
mod workflow_projection;
// wave 3（AW3-11 session 级 seam）**发送端**用例：会话环境装配产出的 per-session
// `TaskManager` + session 级 `on_bg_complete` 送进 builtin `workspace` 实例上下文，
// 并在会话登记处保持同一 `Arc`；回调经 inbox 把 Shell 完成投为 `Defer` 并唤醒。
// 模块名参与 `cargo test` 过滤（`host::workspace_seam`），故不沿用 `mod tests`。
#[cfg(test)]
#[path = "workspace_seam_test.rs"]
mod workspace_seam;

pub(crate) use continuation::{
    run_continuation_scheduler, run_cron_continuation_scheduler, CronContinuationContext,
};
pub(crate) use notify::{extract_session_id, handle_notification, send_session_info_update};
pub(crate) use prompt_dispatch::{dispatch_prompt_turn, PromptOrigin};
pub(crate) use requests::handle_request_for_connection;

pub(crate) use acp_server_config::SCHEDULED_TRIGGER_NAMESPACE;

pub(crate) use acp_server_config::next_session_generation;

pub(crate) use acp_server_config::ContextOwnership;

pub(crate) use acp_server_config::SessionState;

pub use acp_server_config::AcpServerConfig;

pub(crate) use acp_server_config::PromptLocks;
pub(crate) use acp_server_config::SharedSessions;

pub use acp_server_config::run_acp_server;

// 此 crate 内入口别名仅供 native stdio 装配使用。
#[cfg(not(target_os = "emscripten"))]
pub(crate) use acp_server_config::run_acp_server_with_sessions;

#[cfg(test)]
#[path = "oauth_delivery_test.rs"]
mod oauth_cap_tests;

#[path = "acp_server_config.rs"]
mod acp_server_config;
