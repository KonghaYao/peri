//! 按原 Hook 槽位展开非空 group；不另行遍历或重排生产蓝本。
use super::AssemblyContext;
use crate::hooks::HookMiddleware;
use peri_agent::agent::react::ReactLLM;
use peri_agent::middleware::chain::MiddlewareChain;
use std::sync::Arc;

pub(super) fn add_hooks(
    ctx: &AssemblyContext,
    chain: &mut MiddlewareChain,
    host_approval_path: bool,
) {
    let AssemblyContext {
        hook_groups,
        session_start_source,
        llm_factory,
        cwd,
        session_id,
        permission_mode,
        provider_name,
        task_manager,
        broker,
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
                move || {
                    // 工厂只产出模型来源；单次调用身份由触发 hook 的执行阶段绑定。
                    factory(None).into_plain_bridge()
                }
            });
        {
            let indexed_hook_groups = hook_groups.iter().enumerate();
            for (i, group) in indexed_hook_groups {
                if group.is_empty() {
                    continue;
                }
                let group_size = group.len();
                let mw = HookMiddleware::with_session_start(
                    group.clone(),
                    hook_llm_factory.clone(),
                    cwd,
                    session_id,
                    "",
                    permission_mode.clone(),
                    provider_name.clone(),
                    session_start_source.clone(),
                )
                .with_model_call_scope(format!("group:{i}"))
                .with_task_manager(task_manager.clone())
                // PreToolUse `ask` 与 PermissionMiddleware 共用同一 broker 源：
                // 宿主会弹窗时交宿主，宿主不会弹窗时由 hook 层有界审批，避免双通道。
                .with_host_approval_path(host_approval_path)
                .with_broker(Arc::clone(broker));
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
}
