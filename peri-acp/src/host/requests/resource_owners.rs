use std::collections::HashMap;
use std::sync::Arc;

use peri_acp_types::plugin::{ConfigSource, McpServerConfig};
use peri_acp_types::session_resources::FrozenState;
use peri_acp_types::session_resources::work::{
    WorkAction, WorkCommand, WorkDecision, WorkInspection, WorkPage, WorkSelector,
};

use crate::host::{AcpServerConfig, prepared::PreparedSessionInputs};
use crate::transport::types::AcpError;

pub(super) async fn bind(
    cfg: &AcpServerConfig,
    session_id: &str,
    connections: &HashMap<String, McpServerConfig>,
) -> Result<(), AcpError> {
    let snapshot = current_descriptor(cfg, session_id).await?;
    let ordered: std::collections::BTreeMap<_, _> = connections.iter().collect();
    let connections_json = serde_json::to_string(&ordered)
        .map_err(|error| AcpError::new(-32603, error.to_string()))?;
    if let Some(existing) =
        crate::host::work_query::descriptor(&snapshot, snapshot.control.lifecycle)?
            .and_then(|descriptor| descriptor.resource_owners.as_ref())
    {
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
        .apply_work_mutation(&WorkCommand {
            session_id: session_id.to_owned(),
            recipient_lifecycle: snapshot.control.lifecycle,
            mutation_id: format!(
                "resource-owners:{session_id}:{}",
                snapshot.control.lifecycle
            ),
            action: WorkAction::BindResourceOwners {
                expected_revision: snapshot.head.change_seq,
                connections_json,
                authorization_ref: format!("trusted-session-setup:{session_id}"),
            },
        })
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
    let snapshot = current_descriptor(cfg, session_id).await?;
    let binding = crate::host::work_query::descriptor(&snapshot, snapshot.control.lifecycle)?
        .and_then(|descriptor| descriptor.resource_owners.as_ref())
        .ok_or_else(|| {
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
    let work = current_descriptor(cfg, session_id).await?;
    if crate::host::work_query::descriptor(&work, work.control.lifecycle)?
        .is_some_and(|descriptor| descriptor.resource_owners.is_some())
    {
        return load(cfg, session_id).await;
    }
    let evidence = serde_json::json!({
        "kind": "unknownBuiltin",
        "reason": "persisted resource owner declaration is absent; historical resource effects are not recoverable",
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
    let record_id = format!("{kind}:{session_id}:{lifecycle}");
    let legacy = crate::host::work_query::inspect(
        cfg.session_resources.as_ref(),
        session_id,
        WorkSelector::LegacyEvidence {
            record_id: record_id.clone(),
        },
    )
    .await?;
    let WorkPage::LegacyEvidence(records) = &legacy.page else {
        return Err(crate::host::work_query::wrong_page());
    };
    if !records.is_empty() {
        return Ok(());
    }
    persist(
        cfg,
        WorkCommand {
            session_id: session_id.to_owned(),
            recipient_lifecycle: lifecycle,
            mutation_id: format!("quarantine:{record_id}"),
            action: WorkAction::QuarantineLegacy {
                expected_revision: legacy.head.change_seq,
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
    let work = current_descriptor(cfg, session_id).await?;
    if work.control.lifecycle != lifecycle || previous_lifecycle.checked_add(1) != Some(lifecycle) {
        return Err(AcpError::new(
            -32010,
            "Reopen lifecycle changed before owner binding",
        ));
    }
    let previous_page = crate::host::work_query::inspect(
        cfg.session_resources.as_ref(),
        session_id,
        WorkSelector::RecoveryDescriptor {
            lifecycle: previous_lifecycle,
        },
    )
    .await?;
    let previous_descriptor =
        crate::host::work_query::descriptor(&previous_page, previous_lifecycle)?.ok_or_else(
            || {
                AcpError::new(
                    -32010,
                    "Reopen Blocked: previous recovery descriptor missing",
                )
            },
        )?;
    let Some(previous) = previous_descriptor.resource_owners.as_ref() else {
        load_for_restore(cfg, session_id).await?;
        return Err(AcpError::new(
            -32010,
            "Reopen Blocked: previous persistent resource owners are missing",
        ));
    };
    if let Some(current) = crate::host::work_query::descriptor(&work, lifecycle)?
        .and_then(|descriptor| descriptor.resource_owners.as_ref())
    {
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
                    expected_revision: work.head.change_seq,
                    connections_json: previous.connections_json.clone(),
                    authorization_ref: previous.authorization_ref.clone(),
                },
            },
        )
        .await?;
    }
    if let Some(metadata) = &previous_descriptor.child_resume_metadata_json {
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
        let current = current_descriptor(cfg, session_id).await?;
        if let Some(existing) = crate::host::work_query::descriptor(&current, lifecycle)?
            .and_then(|descriptor| descriptor.child_resume_metadata_json.as_ref())
        {
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
                        expected_revision: current.head.change_seq,
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
                    ));
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
        let work = current_descriptor(cfg, session_id).await?;
        quarantine(
            cfg,
            session_id,
            work.control.lifecycle,
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

async fn current_descriptor(
    cfg: &AcpServerConfig,
    session_id: &str,
) -> Result<WorkInspection, AcpError> {
    let control = cfg
        .session_resources
        .load_session_control(&session_id.to_owned())
        .await
        .map_err(crate::host::workspace::resource_error)?;
    let page = crate::host::work_query::inspect(
        cfg.session_resources.as_ref(),
        session_id,
        WorkSelector::RecoveryDescriptor {
            lifecycle: control.lifecycle,
        },
    )
    .await?;
    if page.control.lifecycle != control.lifecycle {
        return Err(AcpError::new(
            -32010,
            "resource owner lifecycle changed during inspection",
        ));
    }
    Ok(page)
}
