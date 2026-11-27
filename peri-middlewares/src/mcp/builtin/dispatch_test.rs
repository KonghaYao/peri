//! `dispatch` 层（实例名 → handler）的 crate 内证据（owner H-01，接线扩展 H-05）。
//!
//! 覆盖口径（主 plan §6 H-01/H-05 行的闸门用例）：工厂覆盖的实例是**精确**集合——注册表里
//! 每个已实现实例各得自己的变体（保留名表与已实现实例表在 W3-B 后逐项一致，**未接线集合为空**），
//! 表外的名字一律 `None`：由 `runtime` 按 `BuiltinSpawnError::HandlerNotWired` 收口，**不得**
//! 静默回退到任一已实现实例。
//!
//! `workspace` 是唯一的例外形状（AW3-11）：它的工厂 arm **无条件**构造——缺 session 级输入
//! 时仍返回 `Workspace` 变体（可见但退化），因此 `None` 在这一支上不代表「未接线」。
//!
//! 工厂选择与输入装配测试位于 `dispatch_factory_tests`；这里保留跨 handler 的 wire 行为。
//! 两条用例的分工：`dispatch_factory_covers_implemented_instances_only` 从注册表派生断言
//! 「未接线集合为空 + 表外名字恒 `None`」；`dispatch_covers_cron_variant` 用带实例
//! 输入的上下文断言各实例拿到**自己的**变体（含「注入的同一份状态对象进了 handler」）以及
//! `workspace` 在**无**输入时的可见但退化。
//!
//! V 矩阵（v4-part-3 sub-plan V §8）第 20/22 行的具名用例在同文件末尾：
//! - [`all_registered_instances_have_handler`]：注册表 → 工厂的一一映射（遍历注册表派生）；
//! - [`builtin_source_propagates`]：cron 的 `ConfigSource::Builtin` 经 overlay →
//!   建传输（三分类）→ status 快照 → `DiscoverMCP` 只读投影保留，transport 分类 = builtin；
//! - [`call_tool_smoke_and_effective_bridge_round_trip`]（原第 22 行 `call_tool_uses_shared_result_mapping`，
//!   改名以匹配精炼后的观测面）：跨实例烟测 + effective 桥一致性。第 23 行
//!   `call_tool_error_text_is_fixed_and_redacted` 的规则细节（固定脱敏文本、逐类变体）
//!   已由各 package 的 `server_test.rs` 覆盖，宿主侧重复证据删除（宿主无映射实现）。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use peri_acp_types::builtin_mcp;
use peri_acp_types::builtin_mcp::is_reserved_instance_name;
use peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES;
use peri_acp_types::builtin_mcp::BUILTIN_RESERVED_INSTANCE_NAMES;
use peri_acp_types::event::BackgroundTaskResult;
use peri_acp_types::plugin::{ConfigSource, McpServerConfig};
use peri_acp_types::tasks::{BgTaskKind, TaskManager};
use peri_agent::agent::async_tasks::TaskManager as ConcreteTaskManager;
use peri_agent::tools::ToolContext;
use rmcp::model::CallToolRequestParams;
use rmcp::model::CallToolResponse;
use rmcp::model::CallToolResult;
use rmcp::model::ClientRequest;
use rmcp::model::CustomRequest;
use rmcp::model::ErrorCode;
use rmcp::model::ServerResult;
use rmcp::service::Peer;
use rmcp::service::RoleClient;
use rmcp::service::ServiceError;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::BuiltinServerHandler;
use crate::mcp::apps::McpCapabilityProfile;
use crate::mcp::builtin::context::{BuiltinInstanceContext, CronInstanceInput};
use crate::mcp::builtin::runtime;
use crate::mcp::builtin::runtime::BuiltinInstanceSupervisor;
use crate::mcp::builtin::runtime::BuiltinServerExit;
use crate::mcp::builtin::runtime::TickCloseOutcome;
use crate::mcp::builtin::runtime::BUILTIN_CONVERGE_TIMEOUT;
use crate::mcp::builtin::{apply_builtin_overlay, BuiltinInjectionPolicy};
use crate::mcp::client;
use crate::mcp::client::ClientStatus;
use crate::mcp::client::McpClientHandle;
use crate::mcp::client::McpClientPool;
use crate::mcp::client::McpServiceWrapper;
use crate::mcp::client::OAuthStatus;
use crate::mcp::discover_tool::DiscoverMCPTool;
use crate::mcp::tool_bridge;
use crate::mcp::tool_bridge::McpToolBridge;
use crate::mcp::transport::{require_known_builtin_instance, TransportConfig, TransportKind};
use crate::mcp::ToolCallError;
use peri_mcp_cron::{CronScheduler, CronTrigger};
use peri_mcp_workflow::WorkflowInstanceInput;
use peri_mcp_workspace::{
    ResourceRoot, ResourceScope, WorkspaceInstanceInput, WorkspaceMcpServer,
    WorkspaceResourcesInput,
};

mod invocation_fixture {
    use peri_acp_types::tools::ToolContext;
    pub(super) const FIXTURE_SESSION_ID: &str = "fixture-dispatch-session";
    pub(super) struct InvocationFixture;
    impl InvocationFixture {
        pub(super) async fn new_calls(_calls: &[(&str, serde_json::Value)]) -> Self {
            Self
        }
        pub(super) fn context(&self, index: usize) -> ToolContext<'_> {
            let mut context = ToolContext::new(&[], ".")
                .with_session_identity(FIXTURE_SESSION_ID, "fixture-turn");
            context.invocation_id = Some(format!("invocation-{index}").into());
            context.tool_call_id = Some(format!("call-{index}").into());
            context
        }
    }
}

#[path = "dispatch_resource_wire_test.rs"]
mod dispatch_resource_wire_tests;

#[path = "dispatch_factory_test.rs"]
mod dispatch_factory_tests;

#[path = "dispatch_test/test_cases.rs"]
mod test_cases;
use test_cases::connect_handler;
use test_cases::connect_via_dispatch;
use test_cases::CrossFixture;
use test_cases::Pair;
use test_cases::WAVE2_INSTANCES;
