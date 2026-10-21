//! Pin the exact source projection/content facts used by a plan, without rejecting tail appends.
use super::super::projection::{MicroCompactPlan, ProjectionAction};
use crate::error::AgentResult;
use crate::session::transcript::MessageTranscript;
use std::collections::HashSet;

pub(super) fn capture(
    transcript: &MessageTranscript,
    plan: &mut MicroCompactPlan,
) -> AgentResult<()> {
    let mut source_ids = plan
        .actions
        .iter()
        .map(|action| action.message_id)
        .collect::<HashSet<_>>();
    for catalog in &plan.catalogs {
        source_ids.extend(catalog.members.iter().map(|member| member.message_id));
        source_ids.extend(catalog.dependencies.iter().copied());
        source_ids.insert(catalog.anchor_message_id);
    }
    for root in &plan.archive_roots {
        source_ids.extend([root.first_anchor, root.last_anchor, root.committed_tail]);
    }
    let mut pending = source_ids.iter().copied().collect::<Vec<_>>();
    while let Some(id) = pending.pop() {
        if let Some(directive) = transcript.projection_ref(id) {
            for dependency in directive
                .archive_catalog
                .iter()
                .flat_map(|catalog| catalog.dependencies.iter().copied())
                .chain(
                    directive
                        .archive_group
                        .iter()
                        .map(|group| group.anchor_message_id),
                )
                .chain(
                    directive
                        .entries
                        .iter()
                        .filter_map(|entry| match entry.action {
                            ProjectionAction::OmitToolExchange {
                                anchor_message_id, ..
                            }
                            | ProjectionAction::ResultReceipt {
                                anchor_message_id, ..
                            } => Some(anchor_message_id),
                            _ => None,
                        }),
                )
            {
                if source_ids.insert(dependency) {
                    pending.push(dependency);
                }
            }
        }
    }
    plan.source_content = transcript
        .entries()
        .iter()
        .filter(|entry| source_ids.contains(&entry.id()))
        .filter_map(|entry| entry.as_message())
        .map(|message| Ok((message.id(), super::super::exposure::fingerprint(message)?)))
        .collect::<AgentResult<_>>()?;
    plan.source_flags = transcript
        .entries()
        .iter()
        .filter(|entry| source_ids.contains(&entry.id()))
        .map(|entry| (entry.id(), transcript.flags(entry.id())))
        .collect();
    Ok(())
}
