//! 生产装配前的端口还原与父工具投影；所有句柄仍由 assemble 持有。
use super::AssemblyContext;
use crate::{
    cron::{CronScheduler, CronSchedulerPortHandle},
    mcp::{build_tool_bridges, McpClientPool, McpResourceTool},
    middleware::{FilesystemMiddleware, TerminalMiddleware, WebMiddleware},
    permission::{AutoClassifier, LlmAutoClassifier},
    tool_search::ToolSearchIndex,
    workflow::WorkflowMiddleware,
};
use peri_acp_types::mcp_skills::McpSkillRegistry;
use peri_agent::{
    interaction::{ChannelBroker, MultiplexBroker, UserInteractionBroker},
    tools::BaseTool,
};
use std::sync::Arc;

pub(super) struct ResolvedPorts {
    pub(super) cron_scheduler_concrete: Option<Arc<parking_lot::Mutex<CronScheduler>>>,
    pub(super) mcp_pool_concrete: Option<Arc<McpClientPool>>,
    pub(super) mcp_agent_registry: Option<Arc<crate::mcp::McpAgentRegistry>>,
    pub(super) tool_search_index_concrete: Arc<ToolSearchIndex>,
    pub(super) workflow_middleware_concrete: Option<Arc<WorkflowMiddleware>>,
    pub(super) auto_classifier: Option<Arc<dyn AutoClassifier>>,
    pub(super) effective_broker: Arc<dyn UserInteractionBroker>,
}

pub(super) fn resolve_ports(ctx: &AssemblyContext) -> ResolvedPorts {
    let AssemblyContext {
        cron_scheduler,
        mcp_pool,
        tool_search_index,
        workflow_middleware,
        auto_classifier_model,
        channel_state,
        broker,
        ..
    } = ctx;
    // L5：middlewares 具体类型经 peri-acp-types 端口接入，此处 downcast
    // 还原（端口实现方为本 crate，生产路径必成功；失败回退与原上层
    // 回退逻辑一致——临时实例 / None 降级）。

    // Cron 调度器：端口 → Arc<Mutex<CronScheduler>>（CronMiddleware 消费）。
    // downcast 失败或无注入时构造临时实例（行为与迁移前一致）。
    let cron_scheduler_concrete: Option<Arc<parking_lot::Mutex<CronScheduler>>> =
        cron_scheduler.as_ref().map(|p| {
            Arc::clone(p)
                .downcast_arc::<CronSchedulerPortHandle>()
                .map(|h| h.0.clone())
                .unwrap_or_else(|_| {
                    Arc::new(parking_lot::Mutex::new(CronScheduler::new(
                        tokio::sync::mpsc::unbounded_channel().0,
                    )))
                })
        });

    // MCP 连接池：端口 → Arc<McpClientPool>。downcast 失败按未注入处理
    //（不注册 MCP 中间件/工具）。
    let mcp_pool_concrete: Option<Arc<McpClientPool>> = mcp_pool.as_ref().map(|p| {
        Arc::clone(p)
            .downcast_arc::<McpClientPool>()
            .unwrap_or_else(|_| Arc::new(McpClientPool::new_pending()))
    });
    let mcp_agent_registry = mcp_pool_concrete
        .as_ref()
        .map(|pool| Arc::new(crate::mcp::McpAgentRegistry::new(Arc::clone(pool))));

    // 工具搜索索引：端口 → Arc<ToolSearchIndex>（失败回退默认实例）。
    let tool_search_index_concrete: Arc<ToolSearchIndex> = Arc::clone(tool_search_index)
        .downcast_arc::<ToolSearchIndex>()
        .unwrap_or_else(|_| Arc::new(ToolSearchIndex::default()));

    // WorkflowMiddleware 端口（会话级复用，None 时构造临时实例）。
    let workflow_middleware_concrete: Option<Arc<WorkflowMiddleware>> = workflow_middleware
        .as_ref()
        .and_then(|p| Arc::clone(p).downcast_arc::<WorkflowMiddleware>().ok());

    // HITL middleware — reuse auto_classifier model from cache when available
    let auto_classifier: Option<Arc<dyn AutoClassifier>> = Some(Arc::new(LlmAutoClassifier::new(
        auto_classifier_model.clone(),
    )));
    // 构造 permission broker（当 channel_state 存在时用 MultiplexBroker 包装）
    let effective_broker: Arc<dyn UserInteractionBroker> =
        match (channel_state, mcp_pool_concrete.as_ref()) {
            (Some(cs), Some(pool)) => {
                let pool_arc: Arc<McpClientPool> = Arc::clone(pool);
                let sender: Arc<dyn peri_agent::interaction::ChannelNotificationSender> = pool_arc;
                let channel_broker = Arc::new(ChannelBroker::new(cs.clone(), sender));
                Arc::new(MultiplexBroker::new(vec![
                    ("tui".to_string(), broker.clone()),
                    (
                        "channel".to_string(),
                        channel_broker as Arc<dyn UserInteractionBroker>,
                    ),
                ]))
            }
            _ => broker.clone(),
        };

    ResolvedPorts {
        cron_scheduler_concrete,
        mcp_pool_concrete,
        mcp_agent_registry,
        tool_search_index_concrete,
        workflow_middleware_concrete,
        auto_classifier,
        effective_broker,
    }
}

pub(super) fn build_parent_tools(
    ctx: &AssemblyContext,
    mcp_pool_concrete: &Option<Arc<McpClientPool>>,
) -> Vec<Box<dyn BaseTool>> {
    let AssemblyContext {
        cwd,
        mcp_skill_registry,
        meta_harness_disabled: disabled,
        ..
    } = ctx;
    // 父工具集（供子 agent 继承）。MetaHarness：父工具按持有 middleware
    // 分支构造——关闭的 middleware 连坐，其工具不进入 parent_tools
    // （设计 §2.5"关闭面 = 全部装配入口"）。
    let mut parent_tools: Vec<Box<dyn BaseTool>> = Vec::new();
    if !disabled.contains("FilesystemMiddleware") {
        parent_tools.extend(FilesystemMiddleware::build_tools(cwd));
    }
    if !disabled.contains("TerminalMiddleware") {
        parent_tools.extend(TerminalMiddleware::build_tools(cwd));
    }
    if !disabled.contains("WebMiddleware") {
        parent_tools.extend(WebMiddleware::build_tools());
    }
    if !disabled.contains("McpMiddleware") {
        if let Some(ref pool) = mcp_pool_concrete {
            let mcp_tools = build_tool_bridges(pool);
            for tool in mcp_tools {
                parent_tools.push(tool);
            }
            if pool.has_resources() {
                parent_tools.push(Box::new(McpResourceTool::new(
                    Arc::clone(pool),
                    // 未装配 session 注册表（print 模式）→ 空注册表
                    //（无条目 = 不校验）
                    mcp_skill_registry
                        .clone()
                        .unwrap_or_else(|| Arc::new(McpSkillRegistry::new())),
                )));
            }
        }
    }

    parent_tools
}
