//! ACP Stdio 传输的共享上下文和 session 状态。
//!
//! 本模块是 stdio 部署单元的装配上下文，持有部署装配点注入的实现类引用
//! （`BaseTool` / `CronScheduler` / `McpClientPool` / `ToolSearchIndex` /
//! `LspServerConfig` 等，经全路径引用，不使用 `use` 声明），
//! 属「装配注入的类型」例外面（伞形 PRD 决策 7/8；
//! ARC-BOUNDARY-001 经 Controller 通道访问存储）。

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use crate::provider::LlmProvider;
use crate::provider::PeriConfig;
use crate::session::agent_pool::AgentPool;
use crate::session::executor::FrozenSessionData;
use parking_lot::RwLock;
use peri_acp_types::cron::CronSchedulerPort;
use peri_acp_types::hooks::RegisteredHook;
use peri_acp_types::interaction::{
    ApprovalDecision, InteractionContext, InteractionResponse, QuestionAnswer,
    UserInteractionBroker,
};
use peri_acp_types::lsp::LspServerConfig;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::permission::SharedPermissionMode;
use peri_acp_types::ports::{McpPoolPort, SkillsPort, ToolSearchPort, WorkflowMiddlewarePort};
use peri_acp_types::store::ThreadStore;
use peri_controller::langfuse::LangfuseSession;
use tokio_util::sync::CancellationToken;

/// 每个 stdio session 的运行时状态
pub(super) struct SessionInfo {
    #[allow(dead_code)] // session 标识字段，保留供调试
    pub(super) session_id: String,
    pub(super) thread_id: String,
    pub(super) cwd: String,
    pub(super) history: Vec<BaseMessage>,
    pub(super) cancel_token: Option<CancellationToken>,
    /// Frozen session data (built once at session/new).
    pub(super) frozen: Option<FrozenSessionData>,
    /// Session-scoped agent pool for LLM instance reuse.
    pub(super) agent_pool: AgentPool,
    /// Session 级 WorkflowMiddleware。
    pub(super) workflow_middleware: Option<Arc<dyn WorkflowMiddlewarePort>>,
    /// Session 级 LSP 服务器池（session/new 时创建，跨 turn 复用；H1）。
    pub(super) lsp_pool: Option<Arc<dyn peri_acp_types::ports::LspPoolPort>>,
}

/// Stdio 传输环境的共享上下文
pub(super) struct StdioContext {
    pub(super) provider: Arc<RwLock<LlmProvider>>,
    pub(super) peri_config: RwLock<PeriConfig>,
    pub(super) permission_mode: Arc<SharedPermissionMode>,
    pub(super) cron_scheduler: Arc<dyn CronSchedulerPort>,
    pub(super) mcp_pool: Option<Arc<dyn McpPoolPort>>,
    pub(super) channel_state: Option<Arc<peri_acp_types::interaction::ChannelState>>,
    pub(super) plugin_skill_roots: Vec<peri_acp_types::skills::SkillRoot>,
    pub(super) plugin_agent_dirs: Vec<PathBuf>,
    pub(super) plugin_loaded: Vec<peri_acp_types::plugin::LoadedPlugin>,
    pub(super) hook_groups: Vec<Vec<RegisteredHook>>,
    pub(super) plugin_lsp_servers: Vec<LspServerConfig>,
    pub(super) tool_search_index: Arc<dyn ToolSearchPort>,
    /// Skills 扫描端口（available-commands 通知经此访问）。
    pub(super) skills: Arc<dyn SkillsPort>,
    pub(super) shared_tools: Arc<RwLock<BTreeMap<String, Arc<dyn peri_agent::tools::BaseTool>>>>,
    /// Workflow agent 装配端口（p1-wa 收口：宿主装配点（cli）构造后注入，
    /// ACP 侧只持端口）。
    pub(super) workflow_middleware_factory:
        Arc<dyn peri_agent::agent::workflow::WorkflowMiddlewareFactory>,
    pub(super) sessions: RwLock<HashMap<String, SessionInfo>>,
    pub(super) thread_store: Arc<dyn ThreadStore>,
    /// Controller 层宿主：dispatch 存储操作（load/list/fork/execute-command/rewind）
    /// 经此访问持久化存储（ARC-BOUNDARY-001 方向，不再直操 `thread_store`）；
    /// 3.0 批 2：事件发射（`publish_event`）/ 执行发起（`run_session`）亦经此宿主。
    pub(super) controller: Arc<peri_controller::Controller>,
    pub(super) langfuse_session: Option<Arc<LangfuseSession>>,
    /// 共享 SessionManager：用于支撑 cascade cancel 子 agent 与 goal_state。
    ///
    /// stdio 本地仍维护 SessionInfo（history/frozen/agent_pool 等），但 SubAgent
    /// 注册/注销与 goal_state 通过 SessionManager 中的 AcpSession 记录管理，
    /// 保证 `execute_prompt` 接收 `Some(session_manager)` 时 cascade cancel 生效。
    pub(super) session_manager: crate::session::SessionManager,
}

/// Stdio 模式下的简化 Broker：直接 approve 所有权限请求，questions 返回空答案。
pub(super) struct StdioBroker;

impl StdioBroker {
    pub(super) fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl UserInteractionBroker for StdioBroker {
    async fn request(&self, context: InteractionContext) -> InteractionResponse {
        match context {
            InteractionContext::Approval { items } => InteractionResponse::Decisions(
                items
                    .into_iter()
                    .map(|_| ApprovalDecision::Approve { source: None })
                    .collect(),
            ),
            InteractionContext::Questions { requests } => InteractionResponse::Answers(
                requests
                    .into_iter()
                    .map(|q| QuestionAnswer {
                        id: q.id,
                        selected: vec![],
                        text: Some(String::new()),
                    })
                    .collect(),
            ),
        }
    }
}
