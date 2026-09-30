//! LSP 文档同步中间件的生产槽位挂载。
//!
//! LSP pool 的配置加载与构造归 `peri_mcp_lsp`；本模块只把 host 投影的
//! `LspPoolPort` 交给同步中间件，不构造或拥有第二份 pool。
use super::AssemblyContext;
use crate::LspSyncMiddleware;
use peri_agent::middleware::chain::MiddlewareChain;
use std::sync::Arc;

pub(super) fn add_lsp(ctx: &AssemblyContext, chain: &mut MiddlewareChain) {
    let AssemblyContext {
        lsp_servers,
        lsp_pool,
        ..
    } = ctx;
    if lsp_servers.is_empty() {
        return;
    }
    // 单 pool 原则：host 装配从 `peri_mcp_lsp::create_host_lsp_pool` 得到唯一实例，
    // 经 `AssemblyContext::lsp_pool` 投影进链。本函数只把该端口交给
    // `LspSyncMiddleware`——不建第二份 pool、不 downcast 还原具体类型
    //（端口即消费面）。
    let Some(port) = lsp_pool else {
        tracing::debug!(
            target: "lsp",
            servers = lsp_servers.len(),
            "无 host pool ⇒ 不装同步中间件"
        );
        return;
    };
    let pool = ctx.mcp_pool.as_ref().and_then(|pool| {
        Arc::clone(pool)
            .downcast_arc::<crate::mcp::McpClientPool>()
            .ok()
    });
    let reader = Arc::new(crate::workspace_io::McpWorkspaceFileReader::new(
        pool,
        Some(ctx.session_id.clone()),
        &ctx.meta_harness_disabled,
    ));
    chain.add(Box::new(LspSyncMiddleware::new(Arc::clone(port), reader)));
}
