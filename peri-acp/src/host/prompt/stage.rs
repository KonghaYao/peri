//! ACP stage assembly and plugin compact hooks.
use crate::session::executor;
use peri_acp_types::hooks::RegisteredHook;
use peri_agent::session::exec::executor_helpers::StageBuildFn;
use std::sync::Arc;

pub(super) fn build_stage_bridge(
    ctx: &executor::SessionContext,
    langfuse_hooks: Option<&executor::LangfuseHooks>,
) -> StageBuildFn {
    let ctx_for_stage = ctx.clone();
    let bridge_factory_for_stage: Option<
        Arc<dyn Fn() -> Arc<dyn peri_agent::agent::LangfuseBridgeLike> + Send + Sync>,
    > = langfuse_hooks.map(|h| {
        let bf = Arc::clone(&h.bridge_factory);
        let provider_display = ctx_for_stage.provider_name.clone();
        Arc::new(move || {
            bf(provider_display.clone(), None)
                .expect("stage bridge_factory: hooks 存在时 bridge 构造必须成功")
        }) as Arc<dyn Fn() -> Arc<dyn peri_agent::agent::LangfuseBridgeLike> + Send + Sync>
    });
    Arc::new(move |sbr| {
        // compact hook 闭包在每次装配时构造（hook_groups 非空才产生动作；
        // 与迁移前 stage_builder 内构造时机逐次一致）
        let (compact_pre_hook, compact_post_hook) = crate::host::prompt::build_compact_hooks(
            &ctx_for_stage.hook_groups,
            &ctx_for_stage.cwd,
            &ctx_for_stage.session_id,
            &ctx_for_stage.provider_model_name,
        );
        crate::host::stage_builder::build_stage_context(
            &ctx_for_stage,
            &peri_middlewares::assembly::ProductionChainAssembler, // ZST 装配器
            compact_pre_hook,
            compact_post_hook,
            sbr.cached_llm.as_ref(),
            sbr.frozen_session,
            sbr.event_handler,
            sbr.agent_overrides,
            sbr.preload_skills,
            sbr.child_handler_factory,
            sbr.auxiliary_model,
            sbr.thread_persistence,
            sbr.goal_controller,
            sbr.task_manager,
            sbr.on_bg_complete,
            bridge_factory_for_stage.clone(),
        )
    })
}

/// 构造 compact plugin hook 回调（宿主装配面职责，L5 归位自
/// host/stage_builder.rs：hook_groups 非空时构造 `fire_pre_compact` /
/// `fire_post_compact` 转发闭包；语义同迁移前——tokio::spawn 转发、不阻塞
/// 管线；hook_groups 为空返回 `(None, None)`）。
#[allow(clippy::type_complexity)]
pub(crate) fn build_compact_hooks(
    hook_groups: &[Vec<RegisteredHook>],
    cwd: &str,
    session_id: &str,
    model: &str,
) -> (
    Option<Arc<dyn Fn() + Send + Sync>>,
    Option<Arc<dyn Fn(bool, usize) + Send + Sync>>,
) {
    let hook_groups_flat: Vec<RegisteredHook> = hook_groups.iter().flatten().cloned().collect();
    if hook_groups_flat.is_empty() {
        return (None, None);
    }
    let cwd = cwd.to_string();
    let sid = session_id.to_string();
    let model = model.to_string();
    let pre: Arc<dyn Fn() + Send + Sync> = {
        let hooks = hook_groups_flat.clone();
        let cwd = cwd.clone();
        let sid = sid.clone();
        let model = model.clone();
        Arc::new(move || {
            let hooks = hooks.clone();
            let cwd = cwd.clone();
            let sid = sid.clone();
            let model = model.clone();
            tokio::spawn(async move {
                peri_middlewares::hooks::stage_firing::fire_pre_compact(
                    &hooks, &cwd, &sid, "", &model, 0,
                )
                .await;
            });
        })
    };
    let post: Arc<dyn Fn(bool, usize) + Send + Sync> = {
        let hooks = hook_groups_flat.clone();
        let cwd = cwd.clone();
        let sid = sid.clone();
        let model = model.clone();
        Arc::new(move |_compacted: bool, affected_count: usize| {
            let hooks = hooks.clone();
            let cwd = cwd.clone();
            let sid = sid.clone();
            let model = model.clone();
            tokio::spawn(async move {
                peri_middlewares::hooks::stage_firing::fire_post_compact(
                    &hooks,
                    &cwd,
                    &sid,
                    "",
                    &model,
                    affected_count,
                )
                .await;
            });
        })
    };
    (Some(pre), Some(post))
}
