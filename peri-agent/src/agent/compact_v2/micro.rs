//! Micro plans commit as one durable unit. Canonical payloads cross the writer barrier
//! before catalog/member flags; the hot view advances only after acknowledgement.

use std::collections::HashMap;

use crate::agent::compact_v2::config::CompactConfig;
use crate::error::AgentResult;
use crate::messages::MessageId;
use crate::session::transcript::MessageTranscript;
use peri_acp_types::context_archive::{validate_projection_metadata, ArchiveError};
use peri_acp_types::projection::ArchiveGroupLink;
use peri_acp_types::store::{CompactionChange, MessageFlags};

use super::projection::{MessageProjectionDirective, MicroCompactPlan};

/// Persist a single already validated plan without replanning or publishing early.
pub async fn apply_micro_plan(
    transcript: &mut MessageTranscript,
    plan: &MicroCompactPlan,
) -> AgentResult<usize> {
    if plan.actions.is_empty() && plan.catalogs.is_empty() {
        return Ok(0);
    }
    validate_source_flags(transcript, plan)?;
    validate_plan_metadata(transcript, plan)
        .map_err(|error| anyhow::anyhow!("recovery_unavailable: {error}"))?;
    let mut committed_tails = HashMap::new();
    if !plan.catalogs.is_empty() {
        transcript.flush_persistence().await?;
        let (resources, session_id, _) = transcript
            .idempotent_reminder_port()
            .ok_or_else(|| anyhow::anyhow!("recovery_unavailable: archive requires persistence"))?;
        let reader = crate::context_archive::ContextArchiveReader::new(resources, session_id);
        for catalog in &plan.catalogs {
            if transcript
                .projection_ref(catalog.anchor_message_id)
                .and_then(|directive| directive.archive_catalog.as_ref())
                .is_some_and(|existing| existing != catalog)
            {
                return Err(
                    anyhow::anyhow!("archive catalog identity cannot be overwritten").into(),
                );
            }
            let tail = reader
                .verify_catalog(catalog)
                .await
                .map_err(|error| anyhow::anyhow!("recovery_unavailable: {error}"))?;
            committed_tails.insert(catalog.source_thread_id.clone(), tail);
        }
    }
    validate_source_flags(transcript, plan)?;
    let flag_updates = prepare_updates(transcript, plan, &committed_tails)
        .map_err(|error| anyhow::anyhow!("recovery_unavailable: {error}"))?;
    let affected = flag_updates.len();
    transcript
        .commit_compaction_lifecycle(CompactionChange {
            flag_updates,
            appended_messages: Vec::new(),
        })
        .await?;
    tracing::debug!(
        affected,
        policy_version = plan.policy_version,
        "Micro projection committed"
    );
    Ok(affected)
}

/// Check the exact persisted shape without I/O, using the same assembly as commit.
pub(super) fn validate_plan_metadata(
    transcript: &MessageTranscript,
    plan: &MicroCompactPlan,
) -> Result<(), ArchiveError> {
    prepare_updates(transcript, plan, &HashMap::new()).map(|_| ())
}

fn prepare_updates(
    transcript: &MessageTranscript,
    plan: &MicroCompactPlan,
    committed_tails: &HashMap<String, MessageId>,
) -> Result<Vec<(MessageId, MessageFlags)>, ArchiveError> {
    let mut directives: HashMap<MessageId, MessageProjectionDirective> = HashMap::new();
    for action in &plan.actions {
        let directive = directives.entry(action.message_id).or_insert_with(|| {
            transcript
                .projection_ref(action.message_id)
                .cloned()
                .unwrap_or_default()
        });
        directive.policy_version = plan.policy_version;
        // A later level replaces its own target, leaving other calls and prior catalogs intact.
        directive.entries.retain(|old| old.target != action.target);
        directive.entries.push(action.clone());
    }
    for catalog in &plan.catalogs {
        for member in &catalog.members {
            let directive = directives.entry(member.message_id).or_insert_with(|| {
                transcript
                    .projection_ref(member.message_id)
                    .cloned()
                    .unwrap_or_default()
            });
            directive.policy_version = plan.policy_version;
            directive.archive_group = Some(ArchiveGroupLink {
                group_id: catalog.group_id.clone(),
                anchor_message_id: catalog.anchor_message_id,
            });
        }
        let directive = directives
            .entry(catalog.anchor_message_id)
            .or_insert_with(|| {
                transcript
                    .projection_ref(catalog.anchor_message_id)
                    .cloned()
                    .unwrap_or_default()
            });
        directive.policy_version = plan.policy_version;
        directive.archive_catalog = Some(catalog.clone());
    }
    // Roots are attached once, to the newest catalog anchor. Full transfers their range.
    if let Some(catalog) = plan.catalogs.last() {
        let directive = directives.entry(catalog.anchor_message_id).or_default();
        directive.archive_roots = plan.archive_roots.clone();
        for root in &mut directive.archive_roots {
            if let Some(tail) = committed_tails.get(&root.source_thread_id) {
                root.committed_tail = *tail;
            }
        }
        directive.archive_receipt_budget_tokens = Some(plan.receipt_budget_tokens);
    }
    let mut flag_updates: Vec<_> = directives
        .into_iter()
        .map(|(id, directive)| {
            validate_projection_metadata(&directive)?;
            let mut flags: MessageFlags = transcript.flags(id);
            flags.truncated = true;
            flags.projection = Some(directive);
            Ok((id, flags))
        })
        .collect::<Result<Vec<_>, ArchiveError>>()?;
    flag_updates.sort_by_key(|(id, _)| id.as_uuid().as_u128());
    Ok(flag_updates)
}

/// A new tail is independent of the planned sources; a changed source is a conflict.
fn validate_source_flags(
    transcript: &MessageTranscript,
    plan: &MicroCompactPlan,
) -> AgentResult<()> {
    for (id, expected) in &plan.source_flags {
        if transcript.get(*id).is_none() || transcript.flags(*id) != *expected {
            return Err(anyhow::anyhow!(
                "micro plan source changed; discard plan before commit: {id:?}"
            )
            .into());
        }
    }
    for (id, expected) in &plan.source_content {
        let actual = transcript.get(*id).and_then(|entry| entry.as_message());
        if match actual {
            Some(message) => super::exposure::fingerprint(message)? != *expected,
            None => true,
        } {
            return Err(anyhow::anyhow!(
                "micro plan source changed; discard plan before commit: {id:?}"
            )
            .into());
        }
    }
    Ok(())
}

/// Convenience entry for callers that have not already planned a Micro pass.
pub async fn micro_compact(
    transcript: &mut MessageTranscript,
    config: &CompactConfig,
) -> AgentResult<usize> {
    let plan = super::planner::plan_micro(transcript, config, true);
    apply_micro_plan(transcript, &plan).await
}

#[cfg(test)]
#[path = "micro_test.rs"]
mod tests;
