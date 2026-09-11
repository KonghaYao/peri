//! MCP projection lease、发现与通知器仅在启用的 Mcp 槽位构造。
use super::AssemblyContext;
use crate::mcp::{McpClientPool, McpMiddleware};
use peri_acp_types::{command_registry::CommandRegistry, mcp_skills::McpSkillRegistry};
use peri_agent::middleware::chain::MiddlewareChain;
use std::sync::Arc;

pub(super) fn add_mcp(
    ctx: &AssemblyContext,
    chain: &mut MiddlewareChain,
    mcp_pool_concrete: &Option<Arc<McpClientPool>>,
) {
    let AssemblyContext {
        dynamic_mcp,
        dynamic_mcp_projection,
        session_id,
        command_registry,
        ..
    } = ctx;
    if let Some(deployment) = dynamic_mcp.as_ref() {
        chain.add(Box::new(crate::mcp::dynamic::DynamicMcpMiddleware::new(
            session_id.clone(),
            Arc::clone(deployment),
        )));
    }
    if let Some(pool) = mcp_pool_concrete.as_ref() {
        let effective_pool = if let Some(deployment) = dynamic_mcp.as_ref() {
            let mut projection_holder = dynamic_mcp_projection.lock();
            if let Some(existing) = projection_holder.as_ref() {
                existing
                    .as_any()
                    .downcast_ref::<crate::mcp::dynamic::registry::CheckedSessionMcpProjection>()
                    .map(|projection| projection.pool())
                    .unwrap_or_else(|| Arc::clone(pool))
            } else {
                let static_handles = pool
                    .get_all_clients()
                    .into_iter()
                    .map(|handle| {
                        let token: peri_acp_types::mcp_skills::HandleToken = handle.clone();
                        (handle.name.clone(), token)
                    })
                    .collect();
                let skill_registry = ctx
                    .mcp_skill_registry
                    .clone()
                    .unwrap_or_else(|| Arc::new(McpSkillRegistry::new()));
                let command_registry = command_registry
                    .clone()
                    .unwrap_or_else(|| Arc::new(CommandRegistry::new()));
                let lease = deployment.capability(session_id).bind_projection(
                    static_handles,
                    skill_registry,
                    command_registry,
                );
                let projected = lease
                    .as_any()
                    .downcast_ref::<crate::mcp::dynamic::registry::CheckedSessionMcpProjection>()
                    .map(|projection| projection.pool())
                    .unwrap_or_else(|| Arc::clone(pool));
                *projection_holder = Some(lease);
                projected
            }
        } else {
            Arc::clone(pool)
        };
        let mw = McpMiddleware::new(Arc::clone(&effective_pool))
            .with_tool_pool(Arc::clone(pool))
            .with_skill_discovery(ctx.mcp_skill_registry.clone(), ctx.cancel.clone())
            .with_command_registry(command_registry.clone());
        // 决策 B：装配后立即触发幂等发现（覆盖「装配时连接已
        // 完成」的场景——已连接 server 即刻 spawn 发现，命令
        // 面/元数据面无需等首轮 before_agent；Started 去重 /
        // Completed 跳过 / Arc::ptr_eq 重连检测保证幂等）。
        mw.ensure_discovery();
        // 注入状态变化通知：经 session 事件通道发布
        // system-notification（TUI 通知面显示）。pool 全局共享，
        // 多 session 时以最后装配的 session 通道为准。
        // 连接完成事件（决策 B）：Connected 状态变化经
        // record_status_change → notifier 补偿触发发现——
        // 覆盖重连/OAuth 授权后连接的场景。注意两个边界：
        // (1) 补偿以最后装配 session 的 cancel 生命周期为限
        // （cancel 后入口早退，发现延迟到其他 session 的
        // before_agent）；(2) 初始连接事件不经过
        // record_status_change（run_initialize 直接插入
        // Connected handle），由 run_initialize 收口时
        // notify_initial_connections 补发一次——「刚进入、
        // 未说话」时初始连接的 server 也能立即驱动发现，
        // 四挂点（装配后立即/session 预热/连接事件/
        // before_agent）+ 初始化收口补发覆盖全时序。
        let tx = ctx.bg_event_tx.clone();
        crate::mcp::middleware::attach_connection_notifier(
            &effective_pool,
            ctx.mcp_skill_registry.as_ref(),
            command_registry.as_ref(),
            &ctx.cancel,
            Some(tx),
        );
        chain.add(Box::new(mw));
    }
}
