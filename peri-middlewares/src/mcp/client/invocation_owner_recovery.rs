use std::sync::Arc;

use peri_acp_types::session_resources::work::*;
use peri_acp_types::tasks::{TaskManager, TaskTerminalDelivery};
use rmcp::model::{ClientRequest, CustomRequest, ServerResult};
use rmcp::service::{Peer, RoleClient};
use serde::Deserialize;

use super::McpClientPool;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OwnerCatalog {
    owner_identity: String,
    scope_id: String,
    invocations: Vec<OwnerRecord>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OwnerRecord {
    metadata: serde_json::Value,
    owner_task_id: Option<String>,
}

impl McpClientPool {
    pub(super) async fn recover_immutable_task_owner(
        &self,
        session_id: &str,
        server: &str,
        raw_task_id: &str,
        peer: &Peer<RoleClient>,
    ) -> Result<
        (
            TaskBinding,
            Arc<dyn TaskManager>,
            Arc<dyn TaskTerminalDelivery>,
        ),
        String,
    > {
        let resources = {
            let directory = self.session_bindings.read();
            let lifecycle = directory
                .lifecycle(session_id)
                .ok_or("Unroutable: session resource owner unavailable")?;
            directory
                .resources(session_id, lifecycle)
                .ok_or("Unroutable: session durable resources unavailable")?
        };
        let mut snapshot = resources
            .load_session_work(&WorkQuery {
                session_id: session_id.into(),
                limit: 1,
            })
            .await
            .map_err(|error| error.to_string())?;
        let meta = self
            .task_scope_meta_for(server, session_id)
            .ok_or("Unroutable: trusted owner scope unavailable")?;
        let response = peri_time::timeout(
            std::time::Duration::from_secs(10),
            peer.send_request(ClientRequest::CustomRequest(CustomRequest::new(
                "workspace/invocationSnapshot",
                Some(serde_json::json!({"_meta": meta})),
            ))),
        )
        .await
        .map_err(|_| "OutcomeUnknown: owner discovery timeout")?
        .map_err(|error| format!("OutcomeUnknown: owner discovery failed: {error}"))?;
        let ServerResult::CustomResult(result) = response else {
            return Err("OutcomeUnknown: invalid owner invocation response".into());
        };
        let catalog: OwnerCatalog =
            serde_json::from_value(result.0).map_err(|error| error.to_string())?;
        if catalog.scope_id != session_id || catalog.owner_identity.is_empty() {
            return Err("Unroutable: owner catalog scope conflicts".into());
        }
        let matching: Vec<_> = catalog
            .invocations
            .iter()
            .filter(|record| record.owner_task_id.as_deref() == Some(raw_task_id))
            .collect();
        if matching.len() != 1 {
            return Err("OutcomeUnknown: owner task lacks unique invocation evidence".into());
        }
        let evidence = &matching[0].metadata;
        let invocation_id = evidence
            .get("invocationId")
            .and_then(serde_json::Value::as_str)
            .ok_or("Unroutable: immutable invocation ID missing")?;
        let record = snapshot
            .state
            .invocations
            .get(invocation_id)
            .ok_or("OutcomeUnknown: original invocation intent unavailable")?;
        let intent = &record.intent;
        if intent.owner_identity != catalog.owner_identity
            || intent.scope_id != session_id
            || intent.recovery_locator != server
            || evidence
                .get("ownerIdentity")
                .and_then(serde_json::Value::as_str)
                != Some(intent.owner_identity.as_str())
            || evidence
                .get("initiatorSessionId")
                .and_then(serde_json::Value::as_str)
                != Some(session_id)
            || evidence
                .get("recipientLifecycle")
                .and_then(serde_json::Value::as_u64)
                != Some(record.recipient_lifecycle)
            || evidence
                .get("scopeEpoch")
                .and_then(serde_json::Value::as_u64)
                != intent.scope_epoch
            || evidence
                .get("argumentsDigest")
                .and_then(serde_json::Value::as_str)
                != Some(intent.effective_arguments_digest.as_str())
            || evidence
                .get("argumentsJson")
                .and_then(serde_json::Value::as_str)
                != Some(intent.effective_arguments_json.as_str())
            || evidence.get("toolName").and_then(serde_json::Value::as_str)
                != Some(intent.effective_tool_name.as_str())
            || evidence
                .get("authorizationRef")
                .and_then(serde_json::Value::as_str)
                != Some(intent.authorization_ref.as_str())
        {
            return Err("Unroutable: physical owner evidence differs from immutable intent".into());
        }
        let binding = TaskBinding {
            invocation_id: invocation_id.into(),
            owner_identity: intent.owner_identity.clone(),
            owner_task_id: raw_task_id.into(),
            initiator_session_id: session_id.into(),
            recipient_lifecycle: record.recipient_lifecycle,
            recovery_locator: intent.recovery_locator.clone(),
            authorization_ref: intent.authorization_ref.clone(),
        };
        if !snapshot.state.task_bindings.contains_key(invocation_id) {
            let mut acknowledged = false;
            for _ in 0..3 {
                let mut command = WorkCommand {
                    session_id: session_id.into(),
                    recipient_lifecycle: binding.recipient_lifecycle,
                    mutation_id: "owner-task-recovery".into(),
                    action: WorkAction::ReconcileTaskBinding {
                        expected_revision: snapshot.state.revision,
                        binding: binding.clone(),
                    },
                };
                command.mutation_id = format!(
                    "owner-reconcile:{}",
                    command.digest().map_err(|error| error.to_string())?
                );
                let command =
                    PreparedWorkCommand::try_new(command).map_err(|error| error.to_string())?;
                match crate::mcp::invocation::commit(resources.as_ref(), &command).await {
                    Ok(_) => {
                        acknowledged = true;
                        break;
                    }
                    Err(crate::mcp::invocation::InvocationError::Rejected(
                        WorkRejection::StaleRevision,
                    )) => {
                        snapshot = resources
                            .load_session_work(&WorkQuery {
                                session_id: session_id.into(),
                                limit: 1,
                            })
                            .await
                            .map_err(|error| error.to_string())?;
                    }
                    Err(error) => return Err(error.to_string()),
                }
            }
            if !acknowledged {
                return Err("Incomplete: task binding ACK unavailable".into());
            }
            snapshot = resources
                .load_session_work(&WorkQuery {
                    session_id: session_id.into(),
                    limit: 1,
                })
                .await
                .map_err(|error| error.to_string())?;
        }
        let recovered = crate::mcp::invocation_recovery::recover_task_binding(
            &snapshot,
            &catalog.owner_identity,
            raw_task_id,
        )?;
        if recovered.intent.scope_epoch != intent_epoch(evidence) {
            return Err("Unroutable: immutable owner epoch conflicts".into());
        }
        let (inbox, manager) = self
            .session_bindings
            .read()
            .binding_at(session_id, recovered.binding.recipient_lifecycle)
            .ok_or("Incomplete: original lifecycle task directory unavailable")?;
        let delivery = peri_agent::agent::async_tasks::durable_task_terminal_delivery(
            resources,
            session_id.into(),
            recovered.binding.recipient_lifecycle,
            inbox.queue().clone(),
        );
        Ok((recovered.binding, manager, delivery))
    }
}

fn intent_epoch(evidence: &serde_json::Value) -> Option<u64> {
    evidence
        .get("scopeEpoch")
        .and_then(serde_json::Value::as_u64)
}
