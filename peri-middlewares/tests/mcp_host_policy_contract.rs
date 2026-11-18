//! D-04：宿主策略与生命周期契约测试（验收契约 6，兼契约 3 的 deferred 半边）。
//!
//! ## 证据范围（诚实分级）
//!
//! 本文件是 `peri-middlewares` 的**外部集成测试**，只能使用 crate 的 `pub` API。
//! MCP 侧使用真实的 rmcp client service（`serve_client_with_lifecycle`）连到
//! `tokio::io::duplex` 上的 JSON-RPC fixture：server 端记录真实收到的 `tools/call`，
//! 因此下面所有「未调用 / 恰好调用一次」断言都是 wire 事实，不是 mock 计数。
//!
//! 逐能力断言（每条一个测试，不用一条测试覆盖多类能力）：
//!
//! | 能力 | 断言 | 承担者 |
//! | --- | --- | --- |
//! | Permission + HITL | broker 收到 effective name；拒绝 → 0 次 wire 调用；批准 → 恰好 1 次 | 本文件的 `hitl_*` 测试 |
//! | effective tool name | 策略/审批看到 `mcp__{server}__{tool}`，wire 上仍是属于该 namespace 的裸工具名 | 同上 |
//! | cancel | 在飞 `tools/call` 被取消后不再重试，dispatch 返回 `Interrupted` | `in_flight_cancellation_*` |
//! | ToolSearch deferral | 未提升的 MCP bridge 不进 `direct_definitions`（与 Reason 阶段下发给 LLM 的工具集用**同一** `is_direct() && visible_to_model()` 谓词），只能经 ToolSearch/ToolInvoke 到达 | `deferred_*` |
//! | session / event / host assembly | **BLOCKED**（见下） | B-07 |
//!
//! ## BLOCKED（不在本层假装覆盖）
//!
//! 1. **被提升为 direct 的真实 MCP bridge**（`McpToolBridge::with_direct` 是
//!    `pub(crate)`，`prepare_system_tools` / `system_mcp_tools` 解析同理）。本层只能
//!    证明**判定输入**：Permission 的决策只看 `(name, input)`，从不接触 `BaseTool`，
//!    因此 `is_direct` 在结构上无法影响审批；而 `is_direct` → `direct_definitions`
//!    的过滤边界由 `deferred_*` 的探针工具单独钉住。真实 direct 提升后的端到端证据
//!    归 **D-02**（crate 内 `mcp_v4_seam_test.rs`）与 **B-07**（`peri-acp` host seam）。
//! 2. **`run_initialize` 驱动的配置 → 提升接线**在本层不可用：它是唯一会发布
//!    `SystemMcpManifest::Loaded` + system 依赖的 public 入口，但内部按
//!    `dirs::home_dir()/.peri/settings.json` 解析全局配置，没有可注入 seam
//!    （`load_merged_config_full_with_paths` 不对外）。在进程内跑它等于读开发机上
//!    真实的 MCP 配置并连真实 server（含凭据），因此本文件**不**调用它。该接线证据
//!    同样归 **B-07** 的隔离 HOME host 场景；本文件记录为 BLOCKED。
//! 3. **session 身份 / ACP 事件投影 / 首个 LLM 请求的 tools 入参 / 生产链装配
//!    （`ProductionChainAssembler` 槽位）**：需要 `peri-acp` host 装配层。
//!    本层只断言 `StageContext` 的 render 事件属于同一 turn，不代替 host seam。
//!    归 **B-07**（`peri-acp/src/host/mcp_v4_startup_test.rs`）。
//! 4. **Hook / SubAgent / Workflow / Goal 的具体实现**未在本层重测；本层只证明
//!    工具调用仍然完整经过 middleware chain（`before_tools_batch` 对 MCP bridge 可见），
//!    即 direct 注入没有短路链上既有 hook 位。
//!
//! fixture 全部定义在本文件内（不建立 workspace 级共享 test helper），且不含任何真实
//! 凭据：server 不校验 header / token，测试也不读取用户 HOME 下的 MCP 配置。

use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::{Mutex, RwLock};
use peri_agent::agent::react::Reasoning;
use peri_agent::agent::react::ToolCall;
use peri_agent::agent::stages::tool_dispatch;
use peri_agent::agent::stages::SharedToolMap;
use peri_agent::agent::stages::StageContext;
use peri_agent::interaction::ApprovalDecision;
use peri_agent::interaction::InteractionContext;
use peri_agent::interaction::InteractionResponse;
use peri_agent::interaction::TrustedInteractionOwner;
use peri_agent::interaction::UserInteractionBroker;
use peri_agent::middleware::capabilities as hook_state;
use peri_agent::middleware::r#trait::Middleware;
use peri_agent::middleware::MiddlewareChain;
use peri_agent::session::tool_catalog::SessionToolCatalog;
use peri_agent::session::FrozenContext;
use peri_agent::session::Session;
use peri_agent::tools::BaseTool;
use peri_agent::tools::ToolContext;
use peri_middlewares::mcp::ClientStatus;
use peri_middlewares::mcp::McpClientHandle;
use peri_middlewares::mcp::McpClientPool;
use peri_middlewares::mcp::McpToolBridge;
use peri_middlewares::mcp::OAuthStatus;
use peri_middlewares::permission;
use peri_middlewares::permission::default_requires_approval;
use peri_middlewares::permission::PermissionMiddleware;
use peri_middlewares::permission::PermissionMode;
use peri_middlewares::permission::SharedPermissionMode;
use peri_middlewares::tool_search::ExecuteExtraTool;
use peri_middlewares::tool_search::SearchExtraTools;
use peri_middlewares::tool_search::ToolSearchIndex;
use peri_middlewares::tool_search::ToolSearchMiddleware;
use peri_middlewares::tool_search::EXECUTE_EXTRA_TOOL_NAME;
use peri_middlewares::tool_search::TOOL_INVOKE_NAME;
use peri_middlewares::tool_search::TOOL_SEARCH_NAME;
use peri_middlewares::ExecuteExtraToolResolver;
use rmcp::model::CallToolRequestParams;
use rmcp::model::ClientCapabilities;
use rmcp::model::Implementation;
use rmcp::model::InitializeRequestParams;
use rmcp::model::JsonObject;
use rmcp::service;
use rmcp::service::ClientLifecycleMode;
use rmcp::service::RoleClient;
use rmcp::service::RunningService;
use rmcp::transport::async_rw::AsyncRwTransport;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

#[path = "mcp_host_policy_contract/app_visibility.rs"]
mod app_visibility;

#[path = "mcp_host_policy_contract/test_cases.rs"]
mod test_cases;
use test_cases::approval_chain;
use test_cases::bridge_tools;
use test_cases::direct_definition_names;
use test_cases::effective_name;
use test_cases::make_context;
use test_cases::spawn_fixture;
use test_cases::RecordingBroker;
use test_cases::APP_ONLY_TOOL;
use test_cases::DEFERRED_TOOL;
