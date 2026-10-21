//! Restored archive projections keep their recovery dependency after policy changes.
use super::StageContext;
use crate::error::{AgentError, AgentResult};
use crate::session::tool_catalog::SessionToolCatalogSnapshot;
use crate::tools::ContextRetention;
use peri_acp_types::projection::ProjectionAction;

pub(super) async fn validate(
    ctx: &StageContext,
    catalog: &SessionToolCatalogSnapshot,
) -> AgentResult<()> {
    let (needs_recovery, only_inherited, port) = {
        let transcript = ctx.session.transcript.read();
        let dependencies: Vec<_> = transcript
            .entries()
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                !transcript.is_excluded(entry.id())
                    && transcript
                        .projection_ref(entry.id())
                        .is_some_and(|directive| {
                            directive.policy_version == 3
                                && (!directive.archive_roots.is_empty()
                                    || directive.entries.iter().any(|entry| {
                                        !matches!(entry.action, ProjectionAction::Keep)
                                    }))
                        })
            })
            .map(|(index, _)| index)
            .collect();
        (
            !dependencies.is_empty(),
            dependencies
                .iter()
                .all(|index| *index < transcript.ancestor_len()),
            transcript.idempotent_reminder_port(),
        )
    };
    if !needs_recovery {
        return Ok(());
    }
    if !catalog
        .tools
        .values()
        .any(|entry| available(entry.tool.as_ref()))
    {
        tracing::error!(
            agent_id = ?ctx.session.agent_id,
            "restored context projection requires an authorized archive recovery tool"
        );
        return Err(AgentError::LlmError(
            "context_archive_capability_required: restore permission for ReadContextArchive before resuming this archived context".into(),
        ));
    }
    let (resources, thread_id, _) = port.ok_or_else(|| {
        AgentError::LlmError(
            "context_archive_capability_required: archive storage is unavailable".into(),
        )
    })?;
    let query = peri_acp_types::context_archive::ArchiveMetadataQuery {
        limit: 1,
        ..Default::default()
    };
    tokio::select! {
        biased;
        _ = ctx.session.turn.cancel_token.cancelled() => return Err(AgentError::Interrupted),
        result = resources.read_archive_metadata(&thread_id, &query) => {
            match result {
                Ok(_) => {},
                // A child may have only inherited roots while its new prompt is queued in
                // the writer. Accept the empty own scope only for inherited dependencies; a missing own
                // committed projection is an unavailable recovery store, not a pending append.
                Err(error) if only_inherited && matches!(error.kind(), peri_acp_types::session_resources::SessionResourceErrorKind::NotFound) => {},
                Err(error) => {
                    tracing::error!(%error, "restored archive recovery capability probe failed");
                    return Err(AgentError::LlmError(format!("context_archive_capability_required: {error}")));
                }
            }
        }
    }
    Ok(())
}

pub(super) fn available(tool: &dyn crate::tools::BaseTool) -> bool {
    tool.context_retention() == ContextRetention::ArchiveRead
        && tool.visible_to_model()
        && tool.is_direct()
}

#[cfg(test)]
#[path = "archive_capability_test.rs"]
mod tests;
