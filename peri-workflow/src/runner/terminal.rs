//! 失败持久化、Git 后置检查与自然结束时的终态投影。

use peri_js_runtime::JsExecutionHost;
use tokio::sync::watch;
use tracing::warn;

use super::{WorkflowInput, WorkflowResult};
use crate::error::WorkflowError;
use crate::journal::WorkflowJournalStore;
use crate::progress::WorkflowProgressStore;
use crate::protocol::ProgressEvent;

fn public_workflow_error(error: &str) -> String {
    peri_acp_types::session::sanitize_public_error(error, 2_000)
}

fn public_stderr_summary(stderr_tail: Option<String>) -> Option<String> {
    stderr_tail.filter(|tail| !tail.trim().is_empty()).map(|_| {
        "workflow process emitted diagnostic stderr; check protected logs for details".into()
    })
}

fn persist_failed_state(
    journal_store: &WorkflowJournalStore,
    run_id: &str,
    input: &WorkflowInput,
    started_at: &str,
    error: &WorkflowError,
) {
    let state = crate::journal::RunState {
        run_id: run_id.to_string(),
        workflow_name: input.workflow_name.clone(),
        status: "failed".to_string(),
        execution_status: peri_acp_types::workflow::ExecutionStatus::Failed,
        acceptance_status: peri_acp_types::workflow::AcceptanceStatus::Unknown,
        post_processing_status: peri_acp_types::workflow::PostProcessingStatus::Blocked,
        delivery_status: peri_acp_types::workflow::DeliveryStatus::Blocked,
        write_intent: input.write_intent.clone(),
        limits: input.limits.clone(),
        budget_total: input.budget_total,
        attempts: journal_store.read_attempts(run_id).unwrap_or_default(),
        return_value: None,
        script: input.script.clone(),
        started_at: started_at.to_string(),
        finished_at: Some(chrono::Utc::now().to_rfc3339()),
        error: Some(public_workflow_error(&format!("{error:#}"))),
    };
    if let Err(write_error) = journal_store.write_state(run_id, &state) {
        warn!(target: "workflow", run_id, error = %write_error, "failed to persist workflow startup failure");
    }
}

pub(super) fn project_postcondition(
    execution_status: &str,
    acceptance_status: peri_acp_types::workflow::AcceptanceStatus,
    write_intent: Option<&peri_acp_types::workflow::WorkflowWriteIntent>,
    verification: Option<&Result<(), String>>,
) -> (
    peri_acp_types::workflow::PostProcessingStatus,
    peri_acp_types::workflow::DeliveryStatus,
) {
    use peri_acp_types::workflow::{
        AcceptanceStatus, DeliveryStatus, PostProcessingStatus, WorkflowWriteIntent,
    };

    let post_processing = match (write_intent, verification) {
        (_, Some(Err(_))) => PostProcessingStatus::Failed,
        (Some(WorkflowWriteIntent::ReadOnly), Some(Ok(()))) => PostProcessingStatus::NotRequired,
        (Some(WorkflowWriteIntent::Write { .. }), Some(Ok(()))) => PostProcessingStatus::Passed,
        _ => PostProcessingStatus::Blocked,
    };
    let delivery = if execution_status != "completed"
        || acceptance_status == AcceptanceStatus::Failed
        || write_intent.is_none()
        || matches!(
            post_processing,
            PostProcessingStatus::Failed | PostProcessingStatus::Blocked
        ) {
        DeliveryStatus::Blocked
    } else if acceptance_status == AcceptanceStatus::Passed {
        DeliveryStatus::Deliverable
    } else {
        DeliveryStatus::Unknown
    };
    (post_processing, delivery)
}

pub(super) fn send_failure(
    tx: &watch::Sender<Option<WorkflowResult>>,
    journal_store: Option<&WorkflowJournalStore>,
    rid: &str,
    input: &WorkflowInput,
    started_at: &str,
    error: &WorkflowError,
    stderr_tail: Option<String>,
) {
    if let Some(journal_store) = journal_store {
        persist_failed_state(journal_store, rid, input, started_at, error);
    }
    let _ = tx.send(Some(WorkflowResult {
        run_id: rid.to_string(),
        status: "failed".to_string(),
        return_value: None,
        error: Some(public_workflow_error(&format!("{error:#}"))),
        post_processing_status: peri_acp_types::workflow::PostProcessingStatus::Blocked,
        delivery_status: peri_acp_types::workflow::DeliveryStatus::Blocked,
        stderr_tail: public_stderr_summary(stderr_tail),
    }));
}

pub(super) fn finalize_workflow(
    mut final_result: WorkflowResult,
    input: &WorkflowInput,
    journal_store: &WorkflowJournalStore,
    progress_store: &WorkflowProgressStore,
    started_at_iso: String,
    host: &JsExecutionHost,
) -> WorkflowResult {
    let postcondition = input
        .git_baseline
        .as_ref()
        .map(|baseline| baseline.verify_postcondition(input.write_intent.as_ref()));
    let acceptance_status = peri_acp_types::workflow::AcceptanceStatus::Unknown;
    (
        final_result.post_processing_status,
        final_result.delivery_status,
    ) = project_postcondition(
        &final_result.status,
        acceptance_status,
        input.write_intent.as_ref(),
        postcondition.as_ref(),
    );
    if let Some(Err(error)) = postcondition {
        let error = format!("Git postcondition failed: {error}");
        final_result.error = Some(match final_result.error.take() {
            Some(existing) => format!("{existing}; {error}"),
            None => error,
        });
    }
    final_result.error = final_result.error.as_deref().map(public_workflow_error);

    // Write state.json
    let stderr_tail = public_stderr_summary(host.stderr_tail());
    let state = crate::journal::RunState {
        run_id: final_result.run_id.clone(),
        workflow_name: input.workflow_name.clone(),
        status: final_result.status.clone(),
        execution_status: match final_result.status.as_str() {
            "completed" => peri_acp_types::workflow::ExecutionStatus::Completed,
            "killed" => peri_acp_types::workflow::ExecutionStatus::Killed,
            _ => peri_acp_types::workflow::ExecutionStatus::Failed,
        },
        acceptance_status: peri_acp_types::workflow::AcceptanceStatus::Unknown,
        post_processing_status: final_result.post_processing_status,
        delivery_status: final_result.delivery_status,
        write_intent: input.write_intent.clone(),
        limits: input.limits.clone(),
        budget_total: input.budget_total,
        attempts: journal_store
            .read_attempts(&final_result.run_id)
            .unwrap_or_default(),
        return_value: final_result.return_value.clone(),
        script: input.script.clone(),
        started_at: started_at_iso,
        finished_at: Some(chrono::Utc::now().to_rfc3339()),
        error: final_result.error.clone(),
    };
    tracing::debug!(
        target: "workflow",
        run_id = %final_result.run_id,
        "calling write_state"
    );
    if let Err(e) = journal_store.write_state(&final_result.run_id, &state) {
        warn!(target: "workflow", run_id = %final_result.run_id, error = %e, "write_state failed");
        final_result.status = "failed".to_string();
        final_result.post_processing_status =
            peri_acp_types::workflow::PostProcessingStatus::Failed;
        final_result.delivery_status = peri_acp_types::workflow::DeliveryStatus::Blocked;
        final_result.error = Some("workflow state persistence failed".to_string());
    } else {
        tracing::info!(
            target: "workflow",
            run_id = %final_result.run_id,
            "write_state succeeded"
        );
    }

    // 收尾收敛 progress_store：msg_loop 自然退出（Node 崩溃/stdout 关闭）时
    // Node 侧 run_done 事件不会到达，run 会永久停留在 Running（幽灵 running，
    // 与 kill 分支同源，issue 2026-08-05）。status 取 final_result.status：
    // completed 路径与 Node 已发的 RunDone 幂等，failed 路径修复永久 Running。
    // 与 kill 分支时序无冲突：kill 会 abort 本 msg_loop，收尾不执行。
    progress_store.apply_event(&ProgressEvent::RunDone {
        run_id: final_result.run_id.clone(),
        status: final_result.status.clone(),
        return_value: None,
        error: final_result.error.clone(),
    });
    progress_store.set_terminal_projection(
        &final_result.run_id,
        match final_result.status.as_str() {
            "completed" => peri_acp_types::workflow::ExecutionStatus::Completed,
            "killed" => peri_acp_types::workflow::ExecutionStatus::Killed,
            _ => peri_acp_types::workflow::ExecutionStatus::Failed,
        },
        peri_acp_types::workflow::AcceptanceStatus::Unknown,
        final_result.post_processing_status,
        final_result.delivery_status,
    );

    final_result.stderr_tail = stderr_tail;
    final_result
}
