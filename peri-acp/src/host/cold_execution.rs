use std::collections::BTreeSet;
use std::sync::Arc;

use peri_acp_types::event::{BackgroundTaskResult, EventSink};
use peri_acp_types::session_resources::work::{
    WorkAction, WorkAdmission, WorkCommand, WorkDecision, WorkQuery, WorkTarget,
};
use peri_agent::agent::stages::{run_react_loop, LoopResult};
use peri_agent::session::subagent::{
    ChildResumeMetadata, ColdChildExecution, ColdChildRuntime, SessionFactory,
    SubagentChainContext, SubagentHost,
};
use peri_agent::tools::BaseTool;

use super::{AcpServerConfig, SessionState, SharedSessions};
use crate::transport::{types::AcpError, AcpTransport};

pub(super) struct ColdChildRun {
    pub execution: ColdChildExecution,
    pub result: BackgroundTaskResult,
}

pub(super) async fn run(
    admission: &WorkAdmission,
    cfg: &Arc<AcpServerConfig>,
    sessions: &SharedSessions,
    transport: &Arc<dyn AcpTransport>,
) -> Result<ColdChildRun, AcpError> {
    let work = cfg
        .session_resources
        .load_session_work(&WorkQuery {
            session_id: admission.session_id.clone(),
            limit: 1,
        })
        .await
        .map_err(super::workspace::resource_error)?;
    let metadata: ChildResumeMetadata = serde_json::from_str(
        work.state
            .child_resume_metadata
            .get(&admission.lifecycle)
            .ok_or_else(|| blocked("persisted child resume metadata missing"))?,
    )
    .map_err(|error| blocked(format!("invalid child resume metadata: {error}")))?;
    let existing = sessions
        .lock()
        .await
        .get(&admission.session_id)
        .and_then(|state| state.environment.clone());
    let environment = match existing {
        Some(environment) => Some(environment),
        None => {
            super::requests::resource_owners::cold_environment(cfg, &admission.session_id).await?
        }
    };
    let local = environment
        .as_ref()
        .map(|environment| &environment.cfg)
        .unwrap_or(cfg);
    let pool = local
        .mcp_pool
        .as_ref()
        .ok_or_else(|| blocked("child resource owner capability unavailable"))?;
    let snapshot = cfg
        .session_resources
        .load_session_snapshot(&admission.session_id)
        .await
        .map_err(super::workspace::resource_error)?;
    let manager = match &environment {
        Some(environment) => environment.task_manager(),
        None => pool
            .clone()
            .agent_session_binding_for_lifecycle(&admission.session_id, admission.lifecycle)
            .await
            .map_err(blocked)?
            .map(|(_, manager)| manager)
            .ok_or_else(|| blocked("original child task owner unavailable"))?,
    };
    local
        .session_manager
        .ensure_session_for_lifecycle(
            &admission.session_id,
            admission.lifecycle,
            &snapshot.meta.cwd,
            manager.clone(),
        )
        .map_err(blocked)?;
    let inbox = local
        .session_manager
        .session_inbox_for(&admission.session_id)
        .ok_or_else(|| blocked("child owned inbox unavailable"))?;
    pool.bind_agent_session_for_lifecycle(
        &admission.session_id,
        admission.lifecycle,
        inbox.handle(),
        manager,
    )
    .map_err(blocked)?;
    pool.bind_agent_session_resources(
        &admission.session_id,
        admission.lifecycle,
        cfg.session_resources.clone(),
    )
    .map_err(blocked)?;
    local
        .session_manager
        .ensure_session_caps(&admission.session_id);
    let assembler =
        super::assemble::child_chain_assembler(&local.session_manager, &admission.session_id);
    let chain = assembler.assemble(&SubagentChainContext {
        cwd: snapshot.meta.cwd.clone(),
        skill_names: metadata.skill_names.clone(),
        frozen_claude_md: Some(metadata.claude_md.clone()),
        frozen_claude_local_md: metadata.claude_local_md.clone(),
        frozen_skill_summary: Some(metadata.skill_summary.clone()),
        meta_harness_disabled: metadata.disabled_middlewares.iter().cloned().collect(),
    });
    let mut tools = pool
        .clone()
        .cold_session_tools(&admission.session_id)
        .await
        .map_err(blocked)?;
    tools.extend(
        chain
            .collect_tools(&snapshot.meta.cwd)
            .into_iter()
            .map(Arc::<dyn BaseTool>::from),
    );
    if let Some(dynamic) = &local.dynamic_mcp {
        tools.extend(
            dynamic
                .capability(&admission.session_id)
                .snapshot()
                .tools
                .values()
                .map(|capability| capability.tool.clone()),
        );
    }
    tools.retain(|tool| metadata.tool_ceiling.contains(tool.name()));
    let provider = trusted_child_provider(local, &metadata.model_name)?;
    let model: Arc<dyn peri_acp_types::model::Model> = Arc::from(provider.into_model());
    let llm = Box::new(
        peri_agent::agent::model_bridge::AgentModelBridge::new(model)
            .with_system(metadata.system_prompt.clone())
            .with_session_id(admission.session_id.clone()),
    );
    let frozen =
        super::requests::resource_owners::load_frozen_for_environment(cfg, &admission.session_id)
            .await?;
    {
        let mut states = sessions.lock().await;
        states
            .entry(admission.session_id.clone())
            .or_insert_with(|| SessionState {
                session_id: admission.session_id.clone(),
                thread_id: admission.session_id.clone(),
                cwd: snapshot.meta.cwd.clone(),
                environment: environment.clone(),
                closing: false,
                history: snapshot
                    .payloads
                    .iter()
                    .filter_map(|payload| payload.as_message().cloned())
                    .collect(),
                history_payloads: snapshot.payloads.clone(),
                cancel_token: None,
                frozen: Some(frozen),
                recall_items: Vec::new(),
                agent_pool: crate::session::agent_pool::AgentPool::new(),
                workflow_middleware: None,
                title: snapshot.meta.title.clone(),
                tags: Vec::new(),
            });
    }
    let runtime = ColdChildRuntime {
        resources: cfg.session_resources.clone(),
        llm,
        chain_assembler: assembler,
        tools,
        host: Arc::new(SubagentHost {
            mcp_pool: Some(pool.clone()),
            session_resources: Some(cfg.session_resources.clone()),
            execution_admission_port: cfg.execution_admission_port.clone(),
            session_mcp_capability: local
                .dynamic_mcp
                .as_ref()
                .map(|deployment| deployment.capability(&admission.session_id)),
            ..Default::default()
        }),
        tool_invocation_resolver: Some(super::assemble::child_tool_invocation_resolver()),
        authorized_ceiling: metadata
            .tool_ceiling
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>(),
        authorization_ref: metadata.authorization_ref.clone(),
    };
    let mut execution = SessionFactory::prepare_cold_child_execution(admission.clone(), runtime)
        .await
        .map_err(|error| blocked(error.to_string()))?;
    if let Some(state) = sessions.lock().await.get_mut(&admission.session_id) {
        state.cancel_token = Some(execution.session.config().cancel_token.as_ref().clone());
    }
    let sink = Arc::new(crate::session::event_sink::TransportEventSink::new(
        transport.clone(),
        local.session_manager.caps_registry(),
    ));
    let mut handles = execution
        .event_handles
        .take()
        .ok_or_else(|| blocked("child event receiver unavailable"))?;
    let session_id = admission.session_id.clone();
    let (stopped_tx, mut stopped_rx) = tokio::sync::oneshot::channel();
    let forwarder = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut stopped_rx => {
                    while let Ok(event) = handles.render_rx.try_recv() {
                        if let Some(event) = peri_agent::agent::events_v2::render_event_to_executor(event) {
                            sink.push_event(&session_id, &event, 200_000).await;
                        }
                    }
                    while let Ok(event) = handles.state_rx.try_recv() {
                        if let Some(event) = peri_agent::agent::events_v2::state_event_to_executor(event) {
                            sink.push_event(&session_id, &event, 200_000).await;
                        }
                    }
                    break;
                },
                render = handles.render_rx.recv() => match render {
                    Some(event) => if let Some(event) = peri_agent::agent::events_v2::render_event_to_executor(event) {
                        sink.push_event(&session_id, &event, 200_000).await;
                    },
                    None => break,
                },
                state = handles.state_rx.recv() => match state {
                    Some(event) => if let Some(event) = peri_agent::agent::events_v2::state_event_to_executor(event) {
                        sink.push_event(&session_id, &event, 200_000).await;
                    },
                    None => break,
                },
            }
        }
    });
    let started = peri_time::monotonic_now();
    let result = run_react_loop(execution.context.clone(), metadata.max_iterations).await;
    if let Some(state) = sessions.lock().await.get_mut(&admission.session_id) {
        state.cancel_token = None;
    }
    stopped_tx
        .send(())
        .map_err(|_| blocked("child event forwarder stopped before the execution barrier"))?;
    forwarder
        .await
        .map_err(|_| blocked("child event forwarder did not join"))?;
    let flush = execution
        .session
        .transcript()
        .read()
        .persist_tx_handle()
        .ok_or_else(|| blocked("child persistence barrier unavailable"))?;
    peri_agent::session::MessageTranscript::flush_via_tx(&flush)
        .await
        .map_err(|error| blocked(format!("child persistence barrier incomplete: {error}")))?;
    cfg.session_resources
        .drain_persistence(&admission.session_id)
        .await
        .map_err(super::workspace::resource_error)?;
    let result = BackgroundTaskResult {
        task_id: execution.delegation_binding.owner_task_id.clone(),
        agent_name: metadata.agent_name,
        prompt_summary: "cold child durable execution".into(),
        success: matches!(result, LoopResult::Completed),
        output: match &result {
            LoopResult::Error(error) => error.user_facing_message(),
            _ => peri_agent::session::subagent::extract_last_ai_text(&execution.session),
        },
        duration_ms: started.elapsed().as_millis() as u64,
        timed_out: false,
        shell_output: None,
        tool_calls_count: peri_agent::session::subagent::count_tool_calls_from_session(
            &execution.session,
        ),
        child_thread_id: Some(admission.session_id.clone()),
        subagent_failure: match &result {
            LoopResult::Error(error) => {
                peri_agent::session::subagent::SubagentFailure::safe_failure_from_error(
                    &admission.session_id,
                    error,
                )
            }
            _ => None,
        },
    };
    Ok(ColdChildRun { execution, result })
}

fn trusted_child_provider(
    cfg: &AcpServerConfig,
    model_name: &str,
) -> Result<crate::provider::LlmProvider, AcpError> {
    let current = cfg.provider.read().clone();
    if current.model_name() == model_name {
        return Ok(current);
    }
    let configuration = cfg.peri_config.read();
    for alias in peri_acp_types::agents::MODEL_TIERS {
        if let Some(provider) =
            crate::provider::LlmProvider::from_config_for_alias(&configuration, alias)
        {
            if provider.model_name() == model_name {
                return Ok(provider);
            }
        }
    }
    Err(blocked(
        "saved child model capability is not authorized by this deployment",
    ))
}

pub(super) async fn block(
    admission: &WorkAdmission,
    cfg: &AcpServerConfig,
    error: &AcpError,
) -> Result<(), AcpError> {
    let work = cfg
        .session_resources
        .load_session_work(&WorkQuery {
            session_id: admission.session_id.clone(),
            limit: 1,
        })
        .await
        .map_err(super::workspace::resource_error)?;
    let target = work
        .state
        .works
        .get(&admission.work_id)
        .ok_or_else(|| blocked("original child work unavailable"))?;
    let receipt = cfg
        .session_resources
        .apply_work_mutation(&WorkCommand {
            session_id: admission.session_id.clone(),
            recipient_lifecycle: admission.lifecycle,
            mutation_id: format!("cold-child-block:{}", admission.admission_id),
            action: WorkAction::BlockWork {
                expected_revision: work.state.revision,
                target: WorkTarget {
                    work_id: admission.work_id.clone(),
                    expected_work_revision: target.revision,
                },
                reason: error.message.clone(),
                recovery_condition:
                    "restore the original frozen child authorization and resource owner capability"
                        .into(),
            },
        })
        .await
        .map_err(super::workspace::resource_error)?;
    if receipt.decision != WorkDecision::Accepted {
        return Err(blocked("child blocked state ACK unavailable"));
    }
    Ok(())
}

fn blocked(reason: impl Into<String>) -> AcpError {
    AcpError::new(-32010, format!("Cold child Blocked: {}", reason.into()))
}
