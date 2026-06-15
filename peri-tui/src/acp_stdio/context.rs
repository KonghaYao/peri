//! ACP Stdio 传输的共享上下文和 session 状态。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use peri_acp::langfuse::LangfuseSession;
use peri_acp::session::agent_pool::AgentPool;
use peri_acp::session::executor::FrozenSessionData;
use peri_agent::agent::AgentCancellationToken;
use peri_agent::interaction::{
    ApprovalDecision, InteractionContext, InteractionResponse, QuestionAnswer,
    UserInteractionBroker,
};
use peri_agent::messages::BaseMessage;
use peri_agent::thread::ThreadStore;
use peri_agent::tools::BaseTool;
use peri_lsp::config::LspServerConfig;
use peri_middlewares::cron::CronScheduler;
use peri_middlewares::hooks::RegisteredHook;
use peri_middlewares::mcp::McpClientPool;
use peri_middlewares::prelude::SharedPermissionMode;
use peri_middlewares::tool_search::ToolSearchIndex;
use peri_tui::app::agent::LlmProvider;
use peri_tui::config::PeriConfig;

/// 每个 stdio session 的运行时状态
pub(super) struct SessionInfo {
    #[allow(dead_code)] // session 标识字段，保留供调试
    pub(super) session_id: String,
    pub(super) thread_id: String,
    pub(super) cwd: String,
    pub(super) history: Vec<BaseMessage>,
    pub(super) cancel_token: Option<AgentCancellationToken>,
    /// Frozen session data (built once at session/new).
    pub(super) frozen: Option<FrozenSessionData>,
    /// Session-scoped agent pool for LLM instance reuse.
    pub(super) agent_pool: AgentPool,
}

/// Stdio 传输环境的共享上下文
pub(super) struct StdioContext {
    pub(super) provider: RwLock<LlmProvider>,
    pub(super) peri_config: RwLock<PeriConfig>,
    pub(super) permission_mode: Arc<SharedPermissionMode>,
    pub(super) cron_scheduler: Arc<Mutex<CronScheduler>>,
    pub(super) mcp_pool: Option<Arc<McpClientPool>>,
    pub(super) channel_state: Option<Arc<peri_agent::interaction::ChannelState>>,
    pub(super) plugin_skill_dirs: Vec<PathBuf>,
    pub(super) plugin_agent_dirs: Vec<PathBuf>,
    pub(super) hook_groups: Vec<Vec<RegisteredHook>>,
    pub(super) plugin_lsp_servers: Vec<LspServerConfig>,
    pub(super) tool_search_index: Arc<ToolSearchIndex>,
    pub(super) shared_tools: Arc<RwLock<HashMap<String, Arc<dyn BaseTool>>>>,
    pub(super) sessions: RwLock<HashMap<String, SessionInfo>>,
    pub(super) thread_store: Arc<dyn ThreadStore>,
    pub(super) langfuse_session: Option<Arc<LangfuseSession>>,
    /// 共享 SessionManager：用于支撑 cascade cancel 子 agent 与 goal_state。
    ///
    /// stdio 本地仍维护 SessionInfo（history/frozen/agent_pool 等），但 SubAgent
    /// 注册/注销与 goal_state 通过 SessionManager 中的 AcpSession 记录管理，
    /// 保证 `execute_prompt` 接收 `Some(session_manager)` 时 cascade cancel 生效。
    pub(super) session_manager: peri_acp::session::SessionManager,
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
