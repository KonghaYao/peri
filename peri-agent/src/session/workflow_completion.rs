use std::sync::Arc;

use peri_acp_types::event::BackgroundTaskResult;
use peri_acp_types::tasks::{OnBgCompleteFn, TaskManager};
use peri_acp_types::workflow::WorkflowTaskResult;

use crate::session::async_router::AsyncRouter;

pub fn apply_workflow_task_result(
    task_result: &WorkflowTaskResult,
    router: &AsyncRouter,
    notify_bg: &dyn TaskManager,
) -> Result<bool, String> {
    let bg = BackgroundTaskResult {
        task_id: task_result.run_id.clone(),
        agent_name: format!("workflow:{}", task_result.workflow_name),
        prompt_summary: task_result.workflow_name.clone(),
        success: task_result.agent_facing_success(),
        output: format!(
            "Workflow '{}' finished with status {:?} ({}ms, {} agents, {} tool calls). \
             Results in .claude/workflow-runs/{}/state.json",
            task_result.workflow_name,
            task_result.status,
            task_result.duration_ms,
            task_result.agent_count,
            task_result.tool_calls_count,
            task_result.run_id
        ),
        tool_calls_count: task_result.tool_calls_count,
        duration_ms: task_result.duration_ms,
        child_thread_id: None,
        timed_out: false,
        subagent_failure: None,
        shell_output: None,
    };

    let router = router.clone();
    let notification = task_result.clone();
    let delivery: OnBgCompleteFn = Arc::new(move |_, _| {
        router.route_workflow_task_result(&notification);
        Ok(())
    });
    notify_bg.settle_completed(&task_result.run_id, bg, delivery)
}

#[cfg(test)]
#[path = "workflow_completion_test.rs"]
mod tests;
