//! MCP projection from the session's single connection pool.
use super::AssemblyContext;
use crate::mcp::{McpClientPool, McpMiddleware};
use peri_agent::middleware::chain::MiddlewareChain;
use std::sync::Arc;
pub(super) fn add_mcp(
    ctx: &AssemblyContext,
    chain: &mut MiddlewareChain,
    mcp_pool_concrete: &Option<Arc<McpClientPool>>,
) {
    if let Some(pool) = mcp_pool_concrete.as_ref() {
        let mw = McpMiddleware::new(Arc::clone(pool))
            .with_session_id(ctx.session_id.clone())
            .with_skill_discovery(ctx.mcp_skill_registry.clone(), ctx.cancel.clone())
            .with_command_registry(ctx.command_registry.clone())
            .with_builtin_closures(crate::mcp::builtin::closed_instances(
                &ctx.meta_harness_disabled,
            ))
            .with_sub_agent_face_closed(
                ctx.meta_harness_disabled
                    .contains(crate::assembly::SUB_AGENT_FACE_CLOSED_KEY),
            );
        mw.ensure_discovery();
        crate::mcp::middleware::attach_connection_notifier(
            pool,
            ctx.mcp_skill_registry.as_ref(),
            ctx.command_registry.as_ref(),
            &ctx.cancel,
            Some(ctx.bg_event_tx.clone()),
        );
        chain.add(Box::new(mw));
    }
}
