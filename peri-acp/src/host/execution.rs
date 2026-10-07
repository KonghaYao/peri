use std::sync::Arc;

use peri_acp_types::execution_admission::{AdmissionOutcome, AdmissionRequest};

use peri_acp_types::session_resources::work::{
    AdmissionRecord, WorkAction, WorkAdmission, WorkCommand, WorkDecision, WorkPage, WorkQuery,
    WorkSelector,
};
use serde_json::{json, Value};

use super::{AcpServerConfig, PromptLocks, SharedSessions};
use crate::transport::{types::AcpError, AcpTransport};

#[path = "execution_finish.rs"]
mod finishing;

pub(super) async fn query(params: &Value, cfg: &AcpServerConfig) -> Result<Value, AcpError> {
    let session_id = session_id(params)?;
    if let Some(mailbox) = cfg.session_manager.user_input_mailbox_for(session_id) {
        mailbox.publish_next_durable().await.map_err(|error| {
            AcpError::new(
                -32010,
                format!("pending input publication unconfirmed: {error}"),
            )
        })?;
    }
    let control = cfg
        .session_resources
        .load_session_control(&session_id.to_owned())
        .await
        .map_err(super::workspace::resource_error)?;
    if let Some(pool) = &cfg.mcp_pool {
        if let Some((inbox, _)) = Arc::clone(pool)
            .agent_session_binding_for_lifecycle(session_id, control.lifecycle)
            .await
            .map_err(|error| AcpError::new(-32010, error))?
        {
            peri_agent::agent::stages::publish_session_inbox(
                Arc::clone(&cfg.session_resources),
                session_id,
                control.lifecycle,
                inbox.queue(),
            )
            .await
            .map_err(|error| {
                AcpError::new(
                    -32010,
                    format!("inbox publication remains unconfirmed: {error}"),
                )
            })?;
        }
    }
    let snapshot = super::work_recovery::resolve_pending(
        cfg.session_resources.as_ref(),
        &WorkQuery {
            session_id: session_id.to_owned(),
            selector: WorkSelector::Availability,
            limit: 64,
            cursor: None,
        },
    )
    .await
    .map_err(super::workspace::resource_error)?;
    let availability = super::work_query::availability(&snapshot)?;
    let pending = cfg
        .session_resources
        .inspect_work(&WorkQuery::new(session_id, WorkSelector::PendingCommands))
        .await
        .map_err(super::workspace::resource_error)?;
    let WorkPage::Commands(commands) = pending.page else {
        return Err(super::work_query::wrong_page());
    };
    let commands: Vec<_> = commands.into_iter().map(|owned| owned.command).collect();
    let work = availability.candidates.first().map(|candidate| {
        json!({
            "workId": candidate.work_id,
            "revision": candidate.work_revision,
            "lifecycle": snapshot.control.lifecycle,
            "controlGeneration": snapshot.control.control_generation,
        })
    });
    Ok(
        json!({"control": snapshot.control, "work": work, "blocked": availability.blocked,
        "pendingCommands": commands}),
    )
}

pub(super) async fn resolve_work(params: &Value, cfg: &AcpServerConfig) -> Result<Value, AcpError> {
    let command: WorkCommand = serde_json::from_value(
        params
            .get("command")
            .cloned()
            .ok_or_else(|| AcpError::new(-32602, "original work command is required"))?,
    )
    .map_err(|error| AcpError::new(-32602, format!("invalid original work command: {error}")))?;
    if command.session_id != session_id(params)? {
        return Err(AcpError::new(-32602, "work command recipient conflict"));
    }
    if matches!(&command.action, WorkAction::FinishAdmission { .. }) {
        let owned = super::work_query::command(
            cfg.session_resources.as_ref(),
            &command.session_id,
            &command.mutation_id,
        )
        .await?;
        if owned.is_none_or(|owned| owned.command != command) {
            return Err(AcpError::new(
                -32010,
                "original execution finish journal is required",
            ));
        }
    }
    let resolution = cfg
        .session_resources
        .resolve_work_mutation(&command)
        .await
        .map_err(super::workspace::resource_error)?;
    serde_json::to_value(resolution).map_err(|error| AcpError::new(-32603, error.to_string()))
}

pub(super) async fn resolve(params: &Value, cfg: &AcpServerConfig) -> Result<Value, AcpError> {
    let ticket = ticket(params)?;
    let snapshot = cfg
        .session_resources
        .inspect_work(&WorkQuery {
            session_id: ticket.session_id.clone(),
            selector: WorkSelector::Admission {
                admission_id: ticket.admission_id.clone(),
            },
            limit: 1,
            cursor: None,
        })
        .await
        .map_err(super::workspace::resource_error)?;
    match super::work_query::admission(&snapshot, &ticket.admission_id)? {
        Some(record) if record.admission == ticket => {
            if finishing::reconcile_finish(cfg.session_resources.as_ref(), &ticket).await?
                || super::cold_terminal::reconcile(cfg.session_resources.as_ref(), &ticket).await?
            {
                finish_admission(cfg.session_resources.as_ref(), &ticket).await?;
                let updated = cfg
                    .session_resources
                    .inspect_work(&WorkQuery {
                        session_id: ticket.session_id.clone(),
                        selector: WorkSelector::Admission {
                            admission_id: ticket.admission_id.clone(),
                        },
                        limit: 1,
                        cursor: None,
                    })
                    .await
                    .map_err(super::workspace::resource_error)?;
                let record = super::work_query::admission(&updated, &ticket.admission_id)?
                    .ok_or_else(|| AcpError::new(-32010, "terminal admission disappeared"))?;
                return Ok(json!({"status": "applied", "reply": reply(record)}));
            }
            Ok(json!({"status": "applied", "reply": reply(record)}))
        }
        Some(_) => Err(AcpError::new(
            -32010,
            "execution admission identity conflict",
        )),
        None => {
            let command = admission_command(&ticket);
            match cfg.session_resources.resolve_work_mutation(&command).await {
                Ok(peri_acp_types::session_resources::work::WorkResolution::NotApplied) => {
                    Ok(json!({"status": "notApplied"}))
                }
                Ok(_) => Ok(json!({"status": "unknown"})),
                Err(error) => Err(super::workspace::resource_error(error)),
            }
        }
    }
}

pub(super) struct ExecutionContext<'a> {
    pub(super) sessions: &'a SharedSessions,
    pub(super) prompt_locks: &'a PromptLocks,
    pub(super) cfg: &'a Arc<AcpServerConfig>,
    pub(super) transport: &'a Arc<dyn AcpTransport>,
    pub(super) continuation:
        &'a Arc<tokio::sync::mpsc::UnboundedSender<crate::session::executor::ContinuationRequest>>,
}

pub(super) async fn execute(
    params: Value,
    context: ExecutionContext<'_>,
) -> Result<Value, AcpError> {
    let admission = ticket(&params)?;
    let resources = &context.cfg.session_resources;
    let snapshot = resources
        .inspect_work(&WorkQuery {
            session_id: admission.session_id.clone(),
            selector: WorkSelector::Admission {
                admission_id: admission.admission_id.clone(),
            },
            limit: 1,
            cursor: None,
        })
        .await
        .map_err(super::workspace::resource_error)?;
    if let Some(record) = super::work_query::admission(&snapshot, &admission.admission_id)? {
        if record.admission != admission {
            return Err(AcpError::new(
                -32010,
                "execution admission identity conflict",
            ));
        }
        if finishing::reconcile_finish(resources.as_ref(), &admission).await?
            || super::cold_terminal::reconcile(resources.as_ref(), &admission).await?
        {
            let evidence_id = finish_admission(resources.as_ref(), &admission).await?;
            return Ok(settled_reply(&admission, &evidence_id));
        }
        return Ok(reply(record));
    }
    let available = super::work_query::inspect(
        resources.as_ref(),
        &admission.session_id,
        WorkSelector::Availability,
    )
    .await?;
    let admission_snapshot =
        peri_acp_types::execution_admission::AdmissionSnapshot::from(available);
    admission_snapshot
        .validate_admission(&admission)
        .map_err(super::workspace::resource_error)?;
    let sdk = context
        .cfg
        .execution_admission_port
        .as_ref()
        .ok_or_else(|| AcpError::new(-32010, "SDK admission capability is unavailable"))?;
    let confirmed = sdk
        .admit(AdmissionRequest {
            request_id: admission.admission_id.clone(),
            snapshot: admission_snapshot,
            existing_admission: Some(admission.clone()),
        })
        .await
        .map_err(|error| AcpError::new(-32010, error.to_string()))?;
    if !matches!(confirmed, AdmissionOutcome::Admitted { admission: confirmed } if confirmed == admission)
    {
        return Err(AcpError::new(
            -32010,
            "SDK execution admission remains unconfirmed",
        ));
    }
    let receipt = resources
        .apply_work_mutation(&admission_command(&admission))
        .await
        .map_err(super::workspace::resource_error)?;
    if !matches!(receipt.decision, WorkDecision::Accepted) {
        return Ok(
            json!({"status": "notApplied", "ticket": admission, "reason": "stale domain association"}),
        );
    }
    let meta = resources
        .load_session_meta(&admission.session_id)
        .await
        .map_err(super::workspace::resource_error)?;
    let mut child_run = None;
    let result = if meta.parent_thread_id.is_some() {
        match super::cold_execution::run(
            &admission,
            context.cfg,
            context.sessions,
            context.transport,
        )
        .await
        {
            Ok(run) => {
                child_run = Some(run);
                Ok(json!({"childSessionId": admission.session_id}))
            }
            Err(error) => {
                super::cold_execution::block(&admission, context.cfg, &error).await?;
                Err(error)
            }
        }
    } else {
        let prompt = json!({
            "sessionId": admission.session_id,
            "executionAdmission": admission,
            "requestId": admission.admission_id,
            "message": {"role": "user", "content": []},
        });
        let environment = context
            .sessions
            .lock()
            .await
            .get(&admission.session_id)
            .and_then(|state| state.environment.clone());
        let cfg = environment
            .as_ref()
            .map(|environment| &environment.cfg)
            .unwrap_or(context.cfg);
        let mailbox =
            super::user_input::ensure_mailbox(&admission.session_id, cfg, context.transport)
                .await?;
        let ticket = mailbox
            .observe_sdk_run(&admission)
            .await
            .map_err(|error| AcpError::new(-32010, error.to_string()))?;
        super::prompt_dispatch::dispatch_prompt_turn_with_input(
            prompt,
            true,
            context.sessions,
            context.prompt_locks,
            context.transport,
            context.cfg,
            context.continuation,
            Some(super::user_input::UserInputRun::new(ticket)),
        )
        .await
    };
    if let Some(child_run) = child_run {
        let command = child_run
            .execution
            .terminal_command(&child_run.result)
            .await
            .map_err(|error| {
                AcpError::new(
                    -32010,
                    format!("child terminal delivery remains unfinished: {error}"),
                )
            })?;
        super::cold_terminal::persist(resources.as_ref(), &admission, command).await?;
        if !super::cold_terminal::reconcile(resources.as_ref(), &admission).await? {
            return Err(AcpError::new(
                -32010,
                "child terminal obligation is missing",
            ));
        }
    } else if meta.parent_thread_id.is_some() {
        return result;
    }
    let evidence_id = finish_admission(resources.as_ref(), &admission).await?;
    let mut response = settled_reply(&admission, &evidence_id);
    if let Err(error) = result {
        if meta.parent_thread_id.is_some() {
            return Err(error);
        }
        tracing::warn!(session_id = admission.session_id, %error, "admitted execution ended with an error");
        response["executionError"] = json!({"code": error.code, "message": error.message});
    }
    Ok(response)
}

pub(super) async fn finish_admission(
    resources: &dyn peri_acp_types::session_resources::SessionResources,
    admission: &WorkAdmission,
) -> Result<String, AcpError> {
    finishing::finish_admission(resources, admission).await
}

fn admission_command(admission: &WorkAdmission) -> WorkCommand {
    WorkCommand {
        session_id: admission.session_id.clone(),
        recipient_lifecycle: admission.lifecycle,
        mutation_id: format!("execution-admit:{}", admission.admission_id),
        action: WorkAction::RegisterAdmission {
            admission: admission.clone(),
        },
    }
}

fn reply(record: &AdmissionRecord) -> Value {
    match &record.leaving_evidence_id {
        Some(evidence_id) => settled_reply(&record.admission, evidence_id),
        None => json!({"status": "running", "ticket": record.admission}),
    }
}

fn settled_reply(admission: &WorkAdmission, evidence_id: &str) -> Value {
    json!({"status": "settled", "ticket": admission, "proof": {
        "kind": "attemptStopped",
        "instanceId": admission.instance_id,
        "generationId": admission.generation_id,
        "execution": admission.execution,
        "evidenceId": evidence_id,
    }})
}

fn session_id(params: &Value) -> Result<&str, AcpError> {
    params
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|session_id| !session_id.is_empty())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))
}

fn ticket(params: &Value) -> Result<WorkAdmission, AcpError> {
    let ticket: WorkAdmission = serde_json::from_value(
        params
            .get("ticket")
            .cloned()
            .ok_or_else(|| AcpError::new(-32602, "missing execution ticket"))?,
    )
    .map_err(|error| AcpError::new(-32602, format!("invalid execution ticket: {error}")))?;
    if session_id(params)? != ticket.session_id {
        return Err(AcpError::new(-32602, "execution ticket session mismatch"));
    }
    Ok(ticket)
}
