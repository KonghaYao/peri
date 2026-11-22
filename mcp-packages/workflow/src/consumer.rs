//! Host consumer commands use tools/call and the same authenticated MCP boundary as backfill.
use super::json;
use super::AgentRunResult;
use super::DispatchOutcome;
use super::RequestMetaObject;
use super::Tool;
use super::Value;
use super::WorkflowMcpServer;
use super::WorkflowTaskKind;
use super::WorkflowTaskState;
use super::WorkflowToolErrorCode;
use super::WorkflowToolOutcome;

pub(super) fn tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "WorkflowAgentClaim",
            "Host-only: atomically claim one agent task; retry with the same claimId.",
            super::schema_of(json!({
                "type":"object", "properties":{"runId":{"type":"string"},"taskId":{"type":"string"},"claimId":{"type":"string"}},"required":["runId","taskId","claimId"]
            })),
        ),
        Tool::new(
            "WorkflowConsumerReady",
            "Host-only: declare consumer readiness after reading a subscribed baseline.",
            super::schema_of(json!({
                "type":"object", "properties":{"ready":{"type":"boolean"}},"required":["ready"]
            })),
        ),
        Tool::new(
            "WorkflowCompletionAck",
            "Host-only: confirm accepted terminal delivery.",
            super::schema_of(json!({
                "type":"object", "properties":{"runId":{"type":"string"}},"required":["runId"]
            })),
        ),
        Tool::new(
            "WorkflowClose",
            "Host-only: close admissions and drain instance-owned executions.",
            super::schema_of(json!({"type":"object","properties":{}})),
        ),
        Tool::new(
            "WorkflowTaskObserved",
            "Host-only: confirm a run is registered in the consumer task projection.",
            super::schema_of(
                json!({"type":"object","properties":{"runId":{"type":"string"}},"required":["runId"]}),
            ),
        ),
    ]
}

impl WorkflowMcpServer {
    pub(super) async fn call_consumer(
        &self,
        name: &str,
        arguments: &Value,
        meta: &RequestMetaObject,
    ) -> DispatchOutcome {
        let runtime = match self.authorize_consumer(meta) {
            Ok(runtime) => runtime,
            Err(error) => return DispatchOutcome::Protocol(error),
        };
        match name {
            "WorkflowConsumerReady" => {
                let Some(ready) = arguments["ready"].as_bool() else {
                    return DispatchOutcome::invalid_params("ready must be a boolean");
                };
                runtime.set_consumer_ready(ready && !runtime.is_closed());
                if !ready {
                    // Subscription loss cancels local executors before withdrawing
                    // readiness. Settle their abandoned claims through the owner.
                    {
                        let iterated_runtime = runtime.tasks().active();
                        for task in iterated_runtime {
                            if task.state == WorkflowTaskState::Executing {
                                match task.kind() {
                                    WorkflowTaskKind::Agent => {
                                        runtime.settle_agent_task(
                                            &task.run_id,
                                            &task.task_id,
                                            WorkflowTaskState::Cancelled,
                                            Some(AgentRunResult::Dead {
                                                scope_close_incomplete: false,
                                                reason: Some("consumer disconnected".into()),
                                                detail: None,
                                                duration_ms: None,
                                                token_count: None,
                                            }),
                                            Some("consumer disconnected".into()),
                                        );
                                    }
                                    // tool 任务走同一终态入口：脚本等待方收到可 catch 的
                                    // `cancelled`（不伪造完成）。
                                    WorkflowTaskKind::Tool => {
                                        let tool = task
                                            .params
                                            .tool_call()
                                            .map_or("<unknown>", |(tool_name, _)| tool_name);
                                        runtime.complete_tool_call(
                                            &task.run_id,
                                            &task.task_id,
                                            WorkflowToolOutcome::error(
                                                WorkflowToolErrorCode::Cancelled,
                                                tool,
                                                "consumer disconnected",
                                            ),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                DispatchOutcome::tool_ok("{}".into())
            }
            "WorkflowClose" => {
                runtime.mark_closed();
                runtime.set_consumer_ready(false);
                let Some(manager) = runtime.bg_registry() else {
                    return DispatchOutcome::tool_err("workflow execution owner is unavailable");
                };
                match manager.shutdown().await {
                    peri_acp_types::tasks::TaskShutdownReport::Complete => {
                        DispatchOutcome::tool_ok("{}".into())
                    }
                    _ => DispatchOutcome::tool_err("workflow execution cleanup is incomplete"),
                }
            }
            "WorkflowTaskObserved" => {
                let run_id = match super::required_str(arguments, "runId") {
                    Ok(id) => id,
                    Err(error) => return DispatchOutcome::Protocol(error),
                };
                if !runtime.bg_registry().is_some_and(|manager| {
                    manager
                        .snapshot()
                        .tasks
                        .iter()
                        .any(|task| task.task_id == run_id)
                }) {
                    return DispatchOutcome::tool_err("unknown workflow task");
                }
                runtime.registry().acknowledge_observation(run_id);
                DispatchOutcome::tool_ok("{}".into())
            }
            "WorkflowCompletionAck" => {
                let run_id = match super::required_str(arguments, "runId") {
                    Ok(id) => id,
                    Err(error) => return DispatchOutcome::Protocol(error),
                };
                runtime.registry().acknowledge_completion(run_id);
                DispatchOutcome::tool_ok("{}".into())
            }
            "WorkflowAgentClaim" => {
                let run_id = match super::required_str(arguments, "runId") {
                    Ok(id) => id,
                    Err(error) => return DispatchOutcome::Protocol(error),
                };
                let task_id = match super::required_str(arguments, "taskId") {
                    Ok(id) => id,
                    Err(error) => return DispatchOutcome::Protocol(error),
                };
                let claim_id = match super::required_str(arguments, "claimId") {
                    Ok(id) if !id.is_empty() => id,
                    _ => return DispatchOutcome::invalid_params("claimId must be nonempty"),
                };
                if runtime.is_closed() {
                    return DispatchOutcome::tool_err("workflow instance is closed");
                }
                super::encode_json(
                    &runtime.tasks().claim_for_consumer(
                        run_id,
                        task_id,
                        claim_id,
                        runtime.run_capacity(run_id),
                    ),
                    "workflow claim",
                )
            }
            _ => DispatchOutcome::invalid_params("unknown consumer command"),
        }
    }
}
