//! 失败持久化、Git 后置检查与自然结束时的终态投影。

use peri_js_runtime::JsExecutionHost;
use tokio::sync::watch;
use tracing::warn;

use crate::error::WorkflowError;
use crate::journal::WorkflowJournalStore;
use crate::progress::WorkflowProgressStore;
use crate::runner::RunLimitReport;
use crate::runner::WorkflowInput;
use crate::runner::WorkflowResult;

fn public_workflow_error(error: &str) -> String {
    peri_acp_types::session::bounded_error_message(error, 2_000)
}

fn public_stderr_summary(stderr_tail: Option<String>) -> Option<String> {
    stderr_tail.filter(|tail| !tail.trim().is_empty())
}

/// 终态 Git 错误文案：既有状态词 + 归因档位（`PostconditionFailure` 的 Display）。
pub(super) fn postcondition_error(failure: &crate::journal::PostconditionFailure) -> String {
    format!(
        "Git postcondition {}: {failure}",
        if failure.is_violation() {
            "failed"
        } else {
            "blocked"
        }
    )
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
        delivery_reason: None,
        postcondition_observation: None,
        git_observation_scope: input.git_observation.scope(),
        outcome: peri_acp_types::workflow::WorkflowOutcome::Failed,
        write_intent: input.write_intent.clone(),
        limits: input.limits.clone(),
        limit_breaches: Vec::new(),
        token_usage: None,
        budget_total: input.budget_total,
        args: input.args.clone(),
        max_concurrency: input.max_concurrency,
        attempts: journal_store.read_attempts(run_id).unwrap_or_default(),
        return_value: None,
        script: input.script.clone(),
        started_at: started_at.to_string(),
        finished_at: Some(peri_time::now_utc_rfc3339()),
        error: Some(public_workflow_error(&format!("{error:#}"))),
    };
    if let Err(write_error) = journal_store.write_state(run_id, &state) {
        warn!(target: "workflow", run_id, error = %write_error, "failed to persist workflow startup failure");
    }
}

/// 终态交付投影：旧枚举 + 结构化原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PostconditionProjection {
    pub post_processing: peri_acp_types::workflow::PostProcessingStatus,
    pub delivery: peri_acp_types::workflow::DeliveryStatus,
    pub reason: Option<peri_acp_types::workflow::DeliveryReason>,
    pub observation: peri_acp_types::workflow::PostconditionObservation,
}

pub(super) fn project_postcondition(
    execution_status: &str,
    acceptance_status: peri_acp_types::workflow::AcceptanceStatus,
    write_intent: Option<&peri_acp_types::workflow::WorkflowWriteIntent>,
    observation: &crate::journal::GitObservation,
    verification: Option<&Result<(), crate::journal::PostconditionFailure>>,
) -> PostconditionProjection {
    use crate::journal::{GitObservation, PostconditionFailure};
    use peri_acp_types::workflow::{
        AcceptanceStatus, DeliveryReason, DeliveryStatus, PostProcessingStatus,
        PostconditionObservation, WorkflowWriteIntent,
    };

    // 观测完整性是独立事实：显式要求验证但没有仓库范围，或运行后采样失败，
    // 都不能读成“完整观测且无变化”。
    let observation_status = match observation {
        GitObservation::NotApplicable if write_intent.is_some() => {
            PostconditionObservation::Incomplete
        }
        GitObservation::Captured(_)
            if verification.is_none()
                || matches!(
                    verification,
                    Some(Err(PostconditionFailure::ObservationIncomplete(_)))
                ) =>
        {
            PostconditionObservation::Incomplete
        }
        other => other.completeness(),
    };

    let (post_processing, reason) = match observation {
        GitObservation::Incomplete(_) => (
            PostProcessingStatus::Blocked,
            Some(DeliveryReason::ObservationIncomplete),
        ),
        GitObservation::NotApplicable => match write_intent {
            // 没有可绑定的仓库范围，也没有请求交付：交付未请求，不是“已通过”。
            None => (
                PostProcessingStatus::Unknown,
                Some(DeliveryReason::NotRequested),
            ),
            Some(_) => (
                PostProcessingStatus::Blocked,
                Some(DeliveryReason::ObservationIncomplete),
            ),
        },
        GitObservation::Captured(_) => match verification {
            // 可归因违例保持 failed；未声明写范围时观察到的变化只能是 blocked。
            Some(Err(failure)) if failure.is_violation() => (PostProcessingStatus::Failed, None),
            Some(Err(failure)) => (PostProcessingStatus::Blocked, failure.delivery_reason()),
            Some(Ok(())) => match write_intent {
                // 未声明意图但观测完整且无差异：未请求交付，旧枚举保持 unknown。
                None => (
                    PostProcessingStatus::Unknown,
                    Some(DeliveryReason::NotRequested),
                ),
                Some(WorkflowWriteIntent::ReadOnly) => (PostProcessingStatus::NotRequired, None),
                Some(WorkflowWriteIntent::Write { .. }) => (PostProcessingStatus::Passed, None),
            },
            None => (
                PostProcessingStatus::Blocked,
                Some(DeliveryReason::ObservationIncomplete),
            ),
        },
    };
    // 内核只投影 `blocked` / `unknown`：验收不由运行内核判定（`acceptance_status`
    // 在完成路径恒为 `Unknown`，无生产写入方），`deliverable` 属引擎之外的验收层，
    // 不在本函数取值域（2026-10-11 用户裁决，见 ADLC-DELIVERY-STATUS-001）。
    // 交付未请求（`unknown` + `not_requested`）不阻塞；无证据仍阻塞。
    let blocked_by_execution =
        execution_status != "completed" || acceptance_status == AcceptanceStatus::Failed;
    let delivery = if blocked_by_execution
        || matches!(
            post_processing,
            PostProcessingStatus::Failed | PostProcessingStatus::Blocked
        ) {
        DeliveryStatus::Blocked
    } else {
        DeliveryStatus::Unknown
    };
    // 原因只解释完成运行上的交付结论；执行失败/取消的原因由 execution 表达，
    // 避免把“未请求交付”与执行失败混成同一个交付原因。
    let reason = if blocked_by_execution { None } else { reason };
    PostconditionProjection {
        post_processing,
        delivery,
        reason,
        observation: observation_status,
    }
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

/// failed 终态必须带有可读错误：引擎 `workflow/done` 偶尔只给 status。
pub(super) fn ensure_failure_error(result: &mut WorkflowResult) {
    if result.status == "failed"
        && result
            .error
            .as_deref()
            .is_none_or(|error| error.trim().is_empty())
    {
        result.error = Some("workflow failed without error details".to_string());
    }
}

pub(super) fn finalize_workflow(
    mut final_result: WorkflowResult,
    input: &WorkflowInput,
    journal_store: &WorkflowJournalStore,
    progress_store: &WorkflowProgressStore,
    started_at_iso: String,
    host: &JsExecutionHost,
    limit_report: RunLimitReport,
) -> WorkflowResult {
    let RunLimitReport {
        breaches,
        token_usage,
        first_error,
    } = limit_report;
    let postcondition = match &input.git_observation {
        crate::journal::GitObservation::Captured(baseline) => {
            Some(baseline.verify_postcondition(input.write_intent.as_ref()))
        }
        crate::journal::GitObservation::NotApplicable
        | crate::journal::GitObservation::Incomplete(_) => None,
    };
    let acceptance_status = peri_acp_types::workflow::AcceptanceStatus::Unknown;
    let projection = project_postcondition(
        &final_result.status,
        acceptance_status,
        input.write_intent.as_ref(),
        &input.git_observation,
        postcondition.as_ref(),
    );
    final_result.post_processing_status = projection.post_processing;
    final_result.delivery_status = projection.delivery;
    if let crate::journal::GitObservation::Incomplete(detail) = &input.git_observation {
        warn!(
            target: "workflow",
            run_id = %final_result.run_id,
            error = %detail,
            "git observation is incomplete; delivery cannot claim an unchanged worktree"
        );
    }
    // 终态读取的是收尾时的完整限额事实：done 之后才回填的 settle 仍不得把
    // 超限 run 显示成全绿（与 done 时的判定同一规则，不新增中断）。
    if !breaches.is_empty() && final_result.status == "completed" {
        final_result.status = "failed".to_string();
    }
    if final_result.error.is_none() {
        final_result.error = first_error.clone();
    }
    if let Some(Err(failure)) = postcondition {
        let error = postcondition_error(&failure);
        final_result.error = Some(match final_result.error.take() {
            Some(existing) => format!("{existing}; {error}"),
            None => error,
        });
    }
    ensure_failure_error(&mut final_result);
    final_result.error = final_result.error.as_deref().map(public_workflow_error);

    let execution_status = match final_result.status.as_str() {
        "completed" => peri_acp_types::workflow::ExecutionStatus::Completed,
        "killed" => peri_acp_types::workflow::ExecutionStatus::Killed,
        _ => peri_acp_types::workflow::ExecutionStatus::Failed,
    };
    let outcome = peri_acp_types::workflow::WorkflowOutcome::project(
        execution_status,
        acceptance_status,
        final_result.delivery_status,
    );

    // Write state.json
    let stderr_tail = public_stderr_summary(host.stderr_tail());
    let state = crate::journal::RunState {
        run_id: final_result.run_id.clone(),
        workflow_name: input.workflow_name.clone(),
        status: final_result.status.clone(),
        execution_status,
        acceptance_status,
        post_processing_status: final_result.post_processing_status,
        delivery_status: final_result.delivery_status,
        delivery_reason: projection.reason,
        postcondition_observation: Some(projection.observation),
        git_observation_scope: input.git_observation.scope(),
        outcome,
        write_intent: input.write_intent.clone(),
        limits: input.limits.clone(),
        limit_breaches: breaches,
        token_usage,
        budget_total: input.budget_total,
        args: input.args.clone(),
        max_concurrency: input.max_concurrency,
        attempts: journal_store
            .read_attempts(&final_result.run_id)
            .unwrap_or_default(),
        return_value: final_result.return_value.clone(),
        script: input.script.clone(),
        started_at: started_at_iso,
        finished_at: Some(peri_time::now_utc_rfc3339()),
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
    // 终态投影与 RunDone 同一次提交落定：订阅方按通知补读即得完整终态
    // （execution 由 reducer 从 status 推出，acceptance 固定 Unknown）。
    progress_store.apply_run_done(
        &final_result.run_id,
        &final_result.status,
        None,
        final_result.error.clone(),
        final_result.post_processing_status,
        final_result.delivery_status,
    );

    final_result.stderr_tail = stderr_tail;
    final_result
}

/// Cancellation has one projection for both startup and an active message loop.
#[allow(clippy::too_many_arguments)]
pub(super) fn send_killed(
    done: &tokio::sync::watch::Sender<Option<crate::runner::WorkflowResult>>,
    journal: &crate::journal::WorkflowJournalStore,
    progress: &crate::progress::WorkflowProgressStore,
    run_id: &str,
    input: &crate::runner::WorkflowInput,
    started_at: &str,
    stderr_tail: Option<String>,
    limit_report: RunLimitReport,
) {
    let RunLimitReport {
        breaches,
        token_usage,
        ..
    } = limit_report;
    let error = Some("workflow killed by user".to_string());
    let state = crate::journal::RunState {
        run_id: run_id.into(),
        workflow_name: input.workflow_name.clone(),
        status: "killed".into(),
        execution_status: peri_acp_types::workflow::ExecutionStatus::Killed,
        acceptance_status: peri_acp_types::workflow::AcceptanceStatus::Unknown,
        post_processing_status: peri_acp_types::workflow::PostProcessingStatus::Blocked,
        delivery_status: peri_acp_types::workflow::DeliveryStatus::Blocked,
        delivery_reason: None,
        postcondition_observation: None,
        git_observation_scope: input.git_observation.scope(),
        outcome: peri_acp_types::workflow::WorkflowOutcome::Killed,
        write_intent: input.write_intent.clone(),
        limits: input.limits.clone(),
        limit_breaches: breaches,
        token_usage,
        budget_total: input.budget_total,
        args: input.args.clone(),
        max_concurrency: input.max_concurrency,
        attempts: journal.read_attempts(run_id).unwrap_or_default(),
        return_value: None,
        script: input.script.clone(),
        started_at: started_at.into(),
        finished_at: Some(peri_time::now_utc_rfc3339()),
        error: error.clone(),
    };
    let _ = journal.write_state(run_id, &state);
    // 与 `done.send` 报告的投影一致：订阅方读到的终态投影 == 命令面结果。
    progress.apply_run_done(
        run_id,
        "killed",
        None,
        error.clone(),
        peri_acp_types::workflow::PostProcessingStatus::Blocked,
        peri_acp_types::workflow::DeliveryStatus::Blocked,
    );
    let _ = done.send(Some(crate::runner::WorkflowResult {
        run_id: run_id.into(),
        status: "killed".into(),
        return_value: None,
        error,
        post_processing_status: peri_acp_types::workflow::PostProcessingStatus::Blocked,
        delivery_status: peri_acp_types::workflow::DeliveryStatus::Blocked,
        stderr_tail: public_stderr_summary(stderr_tail),
    }));
}
