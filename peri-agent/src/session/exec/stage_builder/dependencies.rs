//! StageContext 的可选运行依赖；按原 builder 顺序逐项注入。
use super::StageBuildInput;
use crate::agent::{stages::StageContextBuilder, token::ContextBudget};
use peri_acp_types::{compact::CompactConfig, goal::GoalController, session::SessionInbox};
use std::sync::Arc;

pub(super) struct StageDependencies {
    pub(super) task_manager: Arc<crate::agent::async_tasks::TaskManager>,
    pub(super) goal_controller: Option<Arc<dyn GoalController>>,
    pub(super) context_budget: Option<ContextBudget>,
    pub(super) compact_config: Option<CompactConfig>,
    pub(super) compact_llm_for_v2: Option<Arc<dyn peri_model::Model>>,
    pub(super) idle_inbox: Option<Arc<SessionInbox>>,
    pub(super) idle_should_wait: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}

pub(super) fn configure_stage(
    mut builder: StageContextBuilder,
    input: &StageBuildInput,
    dependencies: StageDependencies,
) -> StageContextBuilder {
    let StageDependencies {
        task_manager,
        goal_controller,
        context_budget,
        compact_config,
        compact_llm_for_v2,
        idle_inbox,
        idle_should_wait,
    } = dependencies;
    builder = builder.with_recipient_lifecycle(input.recipient_lifecycle);
    if let Some(pool) = &input.mcp_pool {
        builder = builder.with_work_mcp_binding(pool.clone(), task_manager);
    }
    if let Some(port) = &input.execution_admission_port {
        builder = builder.with_execution_admission_port(port.clone());
    }
    if let Some(publish) = &input.sdk_run_started {
        builder = builder.with_sdk_run_started(publish.clone());
    }
    if let Some(observe) = &input.sdk_admission_observed {
        builder = builder.with_sdk_admission_observed(observe.clone());
    }
    if let Some(controller) = goal_controller {
        builder = builder.with_goal_controller(controller);
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
    if idle_inbox.is_some() {
        builder = builder.with_idle_waiting();
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
