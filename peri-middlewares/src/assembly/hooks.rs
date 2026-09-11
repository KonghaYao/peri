//! 按原 Hook 槽位展开非空 group；不另行遍历或重排生产蓝本。
use super::AssemblyContext;
use crate::hooks::HookMiddleware;
use peri_agent::{agent::react::ReactLLM, middleware::chain::MiddlewareChain};
use std::sync::Arc;

pub(super) fn add_hooks(ctx: &AssemblyContext, chain: &mut MiddlewareChain) {
    let AssemblyContext {
        hook_groups,
        session_start_source,
        llm_factory,
        cwd,
        permission_mode,
        provider_name,
        ..
    } = ctx;
    tracing::info!(
        groups = hook_groups.len(),
        total_hooks = hook_groups.iter().map(|g| g.len()).sum::<usize>(),
        session_start = session_start_source.is_some(),
        "Builder: assembling HookMiddleware from groups"
    );
    if !hook_groups.is_empty() {
        let hook_llm_factory: Arc<dyn Fn() -> Box<dyn ReactLLM + Send + Sync> + Send + Sync> =
            Arc::new({
                let factory = llm_factory.clone();
                move || factory(None)
            });
        for (i, group) in hook_groups.iter().enumerate() {
            if group.is_empty() {
                continue;
            }
            let group_size = group.len();
            let mw = HookMiddleware::with_session_start(
                group.clone(),
                hook_llm_factory.clone(),
                cwd,
                "",
                "",
                permission_mode.clone(),
                provider_name.clone(),
                session_start_source.clone(),
            );
            tracing::info!(
                group_index = i,
                group_size,
                "Builder: HookMiddleware group {} created with {} hooks",
                i,
                group_size
            );
            chain.add(Box::new(mw));
        }
    }
}
