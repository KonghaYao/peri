//! StageContext 的可选运行依赖；按原 builder 顺序逐项注入。
use super::StageBuildInput;
use crate::{
    agent::{stages::StageContextBuilder, token::ContextBudget},
    error_suggest::ErrorSuggestRegistry,
    session::Session,
};
use peri_acp_types::{compact::CompactConfig, goal::GoalController, session::SessionInbox};
use std::sync::Arc;

pub(super) struct StageDependencies {
    pub(super) goal_controller: Option<Arc<dyn GoalController>>,
    pub(super) error_suggest_registry: Option<Arc<ErrorSuggestRegistry>>,
    pub(super) context_budget: Option<ContextBudget>,
    pub(super) compact_config: Option<CompactConfig>,
    pub(super) compact_llm_for_v2: Option<Arc<dyn peri_model::Model>>,
    pub(super) idle_inbox: Option<Arc<SessionInbox>>,
    pub(super) idle_should_wait: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}

pub(super) fn configure_stage(
    mut builder: StageContextBuilder,
    input: &StageBuildInput,
    session: &Arc<Session>,
    dependencies: StageDependencies,
) -> StageContextBuilder {
    let StageDependencies {
        goal_controller,
        error_suggest_registry,
        context_budget,
        compact_config,
        compact_llm_for_v2,
        idle_inbox,
        idle_should_wait,
    } = dependencies;
    if let Some(controller) = goal_controller {
        builder = builder.with_goal_controller(controller);
    }
    if let Some(reg) = error_suggest_registry {
        builder = builder.with_error_suggest_registry(reg);
    }
    if let Some(budget) = context_budget {
        builder = builder.with_context_budget(budget);
    }
    if let Some(cc) = compact_config {
        builder = builder.with_compact_config(cc);
    }
    if let Some(llm) = compact_llm_for_v2 {
        builder = builder.with_compact_llm(llm);
    }
    if let Some(inbox) = idle_inbox {
        builder = builder.with_idle_inbox(inbox);
    }
    if let Some(handle) = input
        .idle_inbox
        .as_ref()
        .map(|inbox| inbox.handle())
        .or_else(|| {
            session
                .async_owners_guard()
                .and_then(|guard| guard.as_ref().map(|owners| owners.inbox.handle()))
        })
    {
        builder = builder.with_inbox_handle(handle);
    }
    if let Some(probe) = idle_should_wait {
        builder = builder.with_idle_should_wait(probe);
    }
    if let Some(flag) = input.idle_suspended_flag.clone() {
        builder = builder.with_idle_suspended_flag(flag);
    }

    // 注入 compact plugin hook 回调（hook_groups 非空时 ACP 装配点构造闭包）
    if let Some(hook) = &input.compact_pre_hook {
        builder = builder.with_compact_pre_hook(Arc::clone(hook));
    }
    if let Some(hook) = &input.compact_post_hook {
        builder = builder.with_compact_post_hook(Arc::clone(hook));
    }

    builder
}
