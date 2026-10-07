use std::collections::HashMap;
use std::sync::Arc;

use peri_acp_types::plugin::{ConfigSource, McpServerConfig};
use peri_acp_types::session_resources::work::{
    PreparedWorkCommand, WorkAction, WorkCommand, WorkDecision,
};
use peri_acp_types::session_resources::FrozenState;

use crate::host::{prepared::PreparedSessionInputs, AcpServerConfig};
use crate::transport::types::AcpError;

pub(super) async fn bind(
    cfg: &AcpServerConfig,
    session_id: &str,
    connections: &HashMap<String, McpServerConfig>,
) -> Result<(), AcpError> {
    let snapshot = cfg
        .session_resources
        .load_resource_owner_facts(&session_id.to_owned(), 0)
        .await
        .map_err(crate::host::workspace::resource_error)?;
    let ordered: std::collections::BTreeMap<_, _> = connections.iter().collect();
    let connections_json = serde_json::to_string(&ordered)
        .map_err(|error| AcpError::new(-32603, error.to_string()))?;
    if let Some(existing) = &snapshot.current_owner {
        return if existing.connections_json == connections_json {
            Ok(())
        } else {
            Err(AcpError::new(
                -32010,
                "resource owner declaration is immutable",
            ))
        };
    }
    let receipt = cfg
        .session_resources
        .apply_work_mutation(
            &PreparedWorkCommand::try_new(WorkCommand {
                session_id: session_id.to_owned(),
                recipient_lifecycle: snapshot.control.lifecycle,
                mutation_id: format!(
                    "resource-owners:{session_id}:{}",
                    snapshot.control.lifecycle
                ),
                action: WorkAction::BindResourceOwners {
                    expected_revision: snapshot.revision,
                    connections_json,
                    authorization_ref: format!("trusted-session-setup:{session_id}"),
                },
            })
            .map_err(crate::host::workspace::resource_error)?,
        )
        .await
        .map_err(crate::host::workspace::resource_error)?;
    if !matches!(receipt.decision, WorkDecision::Accepted) {
        return Err(AcpError::new(
            -32010,
            "resource owner declaration was rejected",
        ));
    }
    Ok(())
}

pub(crate) async fn load(
    cfg: &AcpServerConfig,
    session_id: &str,
) -> Result<HashMap<String, McpServerConfig>, AcpError> {
    let snapshot = cfg
        .session_resources
        .load_resource_owner_facts(&session_id.to_owned(), 0)
        .await
        .map_err(crate::host::workspace::resource_error)?;
    let binding = snapshot.current_owner.ok_or_else(|| {
        AcpError::new(
            -32010,
            "resource owner declarations require explicit migration",
        )
    })?;
    let mut connections: HashMap<String, McpServerConfig> =
        serde_json::from_str(&binding.connections_json)
            .map_err(|_| AcpError::new(-32010, "resource owner declarations are unreadable"))?;
    if let Some(workspace) = connections.get_mut("workspace") {
        if workspace.url.is_none() {
            return Err(AcpError::new(
                -32010,
                "persisted Workspace owner is not a remote connection",
            ));
        }
        workspace.source = Some(ConfigSource::WorkspaceRemote);
    }
    Ok(connections)
}

pub(crate) async fn load_for_restore(
    cfg: &AcpServerConfig,
    session_id: &str,
) -> Result<HashMap<String, McpServerConfig>, AcpError> {
    let work = cfg
        .session_resources
        .load_resource_owner_facts(&session_id.to_owned(), 0)
        .await
        .map_err(crate::host::workspace::resource_error)?;
    if work.current_owner.is_some() {
        return load(cfg, session_id).await;
    }
    let history = cfg
        .session_resources
        .load_session_snapshot(&session_id.to_owned())
        .await
        .map_err(crate::host::workspace::resource_error)?;
    let evidence = serde_json::json!({
        "kind": "unknownBuiltin",
        "reason": "persisted resource owner declaration is absent; historical resource effects are not recoverable",
        "canonicalMessageIds": history.payloads.iter().map(|payload| payload.id().as_uuid().to_string()).collect::<Vec<_>>(),
    }).to_string();
    quarantine(
        cfg,
        session_id,
        work.control.lifecycle,
        "ownerMissing",
        evidence,
    )
    .await?;
    Ok(HashMap::new())
}

pub(crate) async fn quarantine(
    cfg: &AcpServerConfig,
    session_id: &str,
    lifecycle: u64,
    kind: &str,
    evidence: String,
) -> Result<(), AcpError> {
    let work = cfg
        .session_resources
        .load_work_availability(&session_id.to_owned())
        .await
        .map_err(crate::host::workspace::resource_error)?;
    let record_id = format!("{kind}:{session_id}:{lifecycle}");
    if work.state.legacy_unknown.contains(&record_id) {
        return Ok(());
    }
    persist(
        cfg,
        WorkCommand {
            session_id: session_id.to_owned(),
            recipient_lifecycle: lifecycle,
            mutation_id: format!("quarantine:{record_id}"),
            action: WorkAction::QuarantineLegacy {
                expected_revision: work.state.revision,
                record_id,
                evidence,
            },
        },
    )
    .await
}

pub(super) async fn copy_for_reopen(
    cfg: &AcpServerConfig,
    session_id: &str,
    previous_lifecycle: u64,
    lifecycle: u64,
) -> Result<(), AcpError> {
    let work = cfg
        .session_resources
        .load_resource_owner_facts(&session_id.to_owned(), previous_lifecycle)
        .await
        .map_err(crate::host::workspace::resource_error)?;
    if work.control.lifecycle != lifecycle || previous_lifecycle.checked_add(1) != Some(lifecycle) {
        return Err(AcpError::new(
            -32010,
            "Reopen lifecycle changed before owner binding",
        ));
    }
    let Some(previous) = &work.previous_owner else {
        load_for_restore(cfg, session_id).await?;
        return Err(AcpError::new(
            -32010,
            "Reopen Blocked: previous persistent resource owners are missing",
        ));
    };
    if let Some(current) = &work.current_owner {
        if current.connections_json != previous.connections_json
            || current.authorization_ref != previous.authorization_ref
        {
            return Err(AcpError::new(
                -32010,
                "Reopen resource owner declaration conflicts",
            ));
        }
    } else {
        persist(
            cfg,
            WorkCommand {
                session_id: session_id.to_owned(),
                recipient_lifecycle: lifecycle,
                mutation_id: format!("reopen-resource-owners:{session_id}:{lifecycle}"),
                action: WorkAction::BindResourceOwners {
                    expected_revision: work.revision,
                    connections_json: previous.connections_json.clone(),
                    authorization_ref: previous.authorization_ref.clone(),
                },
            },
        )
        .await?;
    }
    if let Some(metadata) = &work.previous_child_metadata {
        let mut metadata: peri_agent::session::subagent::ChildResumeMetadata =
            serde_json::from_str(metadata).map_err(|_| {
                AcpError::new(
                    -32010,
                    "Reopen Blocked: previous child metadata is unreadable",
                )
            })?;
        if metadata.child_session_id != session_id
            || metadata.recipient_lifecycle != previous_lifecycle
        {
            return Err(AcpError::new(
                -32010,
                "Reopen Blocked: previous child identity conflicts",
            ));
        }
        metadata.recipient_lifecycle = lifecycle;
        let metadata_json = serde_json::to_string(&metadata)
            .map_err(|error| AcpError::new(-32603, error.to_string()))?;
        let current = cfg
            .session_resources
            .load_resource_owner_facts(&session_id.to_owned(), previous_lifecycle)
            .await
            .map_err(crate::host::workspace::resource_error)?;
        if let Some(existing) = &current.current_child_metadata {
            if serde_json::from_str::<serde_json::Value>(existing).ok()
                != serde_json::from_str::<serde_json::Value>(&metadata_json).ok()
            {
                return Err(AcpError::new(
                    -32010,
                    "Reopen child resume metadata conflicts",
                ));
            }
        } else {
            persist(
                cfg,
                WorkCommand {
                    session_id: session_id.to_owned(),
                    recipient_lifecycle: lifecycle,
                    mutation_id: format!("reopen-child-metadata:{session_id}:{lifecycle}"),
                    action: WorkAction::BindChildResumeMetadata {
                        expected_revision: current.revision,
                        metadata_json,
                    },
                },
            )
            .await?;
        }
    }
    Ok(())
}

async fn persist(cfg: &AcpServerConfig, command: WorkCommand) -> Result<(), AcpError> {
    use peri_acp_types::session_resources::work::WorkResolution;
    let command =
        PreparedWorkCommand::try_new(command).map_err(crate::host::workspace::resource_error)?;
    let receipt = match cfg.session_resources.apply_work_mutation(&command).await {
        Ok(receipt) => receipt,
        Err(error) if error.is_persistence_uncertain() => {
            match cfg
                .session_resources
                .resolve_work_mutation(&command)
                .await
                .map_err(crate::host::workspace::resource_error)?
            {
                WorkResolution::Applied { receipt } => receipt,
                _ => {
                    return Err(AcpError::new(
                        -32010,
                        "Resource owner persistence remains unconfirmed",
                    ))
                }
            }
        }
        Err(error) => return Err(crate::host::workspace::resource_error(error)),
    };
    if receipt.decision != WorkDecision::Accepted {
        return Err(AcpError::new(
            -32010,
            "Resource owner mutation was rejected",
        ));
    }
    Ok(())
}

pub(crate) async fn cold_environment(
    cfg: &AcpServerConfig,
    session_id: &str,
) -> Result<Option<Arc<crate::host::workspace::SessionEnvironment>>, AcpError> {
    let connections = load(cfg, session_id).await?;
    if connections.is_empty() {
        let control = cfg
            .session_resources
            .load_session_control(&session_id.to_owned())
            .await
            .map_err(crate::host::workspace::resource_error)?;
        quarantine(
            cfg,
            session_id,
            control.lifecycle,
            "unknownBuiltin",
            "prior builtin resource owner has no persistent recoverable capability".into(),
        )
        .await?;
        return Err(AcpError::new(
            -32010,
            "Session close incomplete: unknownBuiltin resource owner is Blocked and cannot be recovered",
        ));
    }
    let snapshot = cfg
        .session_resources
        .load_session_snapshot(&session_id.to_owned())
        .await
        .map_err(crate::host::workspace::resource_error)?;
    let FrozenState::Present(frozen) = snapshot.frozen else {
        return Err(AcpError::new(
            -32010,
            "Session close incomplete: frozen context is unavailable",
        ));
    };
    let mut prepared =
        PreparedSessionInputs::prepare_restore(cfg, &snapshot.meta.cwd, frozen.as_str())?;
    prepared.session_mcp_servers = connections;
    let environment =
        crate::host::workspace::SessionEnvironment::assemble_prepared(cfg, &prepared, session_id)
            .await?;
    if let Some(environment) = &environment {
        environment.activate();
    }
    Ok(environment)
}

pub(crate) async fn load_frozen_for_environment(
    cfg: &AcpServerConfig,
    session_id: &str,
) -> Result<crate::session::executor::FrozenSessionData, AcpError> {
    let snapshot = cfg
        .session_resources
        .load_session_snapshot(&session_id.to_owned())
        .await
        .map_err(crate::host::workspace::resource_error)?;
    let FrozenState::Present(frozen) = snapshot.frozen else {
        return Err(AcpError::new(
            -32010,
            "persisted frozen context unavailable",
        ));
    };
    crate::session::frozen_snapshot::decode_frozen_snapshot(frozen.as_str())
        .map_err(crate::host::workspace::workspace_error)
}
