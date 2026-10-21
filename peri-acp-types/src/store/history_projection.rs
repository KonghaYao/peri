//! Rewind reconciles archive dependencies in the same transaction as history deletion.
use std::collections::{HashMap, HashSet};

use crate::context_archive::ArchiveRoot;
use crate::messages::MessageId;
use crate::projection::ProjectionAction;
use crate::store::MessageFlags;

use super::RewindFlagEntry;

/// Compute flag replacements for a retained canonical prefix, without reading its bodies.
/// Missing anchors, members, or replacement evidence revoke the whole archive group.
pub fn rewind_projection_updates(
    retained: &[RewindFlagEntry],
    clear_excluded: &[MessageId],
) -> Vec<(MessageId, MessageFlags)> {
    let ids: HashSet<_> = retained.iter().map(|(id, _, _)| *id).collect();
    let clear: HashSet<_> = clear_excluded.iter().copied().collect();
    let by_id: HashMap<_, _> = retained.iter().map(|(id, flags, _)| (*id, flags)).collect();
    let omission_groups: HashSet<_> = retained
        .iter()
        .flat_map(|(_, flags, _)| {
            flags
                .projection
                .iter()
                .flat_map(|p| p.entries.iter())
                .filter_map(|entry| match &entry.action {
                    ProjectionAction::OmitToolExchange {
                        anchor_message_id,
                        group_id,
                        ..
                    } => Some((*anchor_message_id, group_id.clone())),
                    _ => None,
                })
        })
        .collect();
    let mut valid_catalogs = HashSet::new();
    let mut roots: HashMap<String, ArchiveRoot> = HashMap::new();
    let Some((tail, _, _)) = retained.last() else {
        return Vec::new();
    };
    for (id, flags, _) in retained {
        let Some(catalog) = flags
            .projection
            .as_ref()
            .and_then(|p| p.archive_catalog.as_ref())
        else {
            continue;
        };
        let omission_complete = !omission_groups.contains(&(*id, catalog.group_id.clone()))
            || catalog.members.iter().all(|member| {
                by_id.get(&member.message_id).is_some_and(|flags| flags.excluded
                    || flags.projection.iter().flat_map(|p| &p.entries).any(|entry| {
                        matches!(&entry.action, ProjectionAction::OmitToolExchange { anchor_message_id, group_id, .. }
                            if *anchor_message_id == *id && *group_id == catalog.group_id)
                    }))
            });
        if omission_complete
            && catalog.anchor_message_id == *id
            && catalog.members.iter().all(|m| ids.contains(&m.message_id))
            && catalog
                .dependencies
                .iter()
                .all(|dependency| ids.contains(dependency))
        {
            valid_catalogs.insert(*id);
            roots
                .entry(catalog.source_thread_id.clone())
                .and_modify(|root| {
                    root.last_anchor = *id;
                })
                .or_insert_with(|| ArchiveRoot {
                    source_thread_id: catalog.source_thread_id.clone(),
                    first_anchor: *id,
                    last_anchor: *id,
                    committed_tail: *tail,
                });
        }
    }
    let mut updates = Vec::new();
    for (id, original, _) in retained {
        let mut flags = original.clone();
        if clear.contains(id) {
            flags.excluded = false;
        }
        if let Some(projection) = flags.projection.as_mut() {
            if clear.contains(id) {
                projection.superseded_by = None;
            }
            let invalid_member_link = projection
                .archive_group
                .as_ref()
                .is_some_and(|group| !valid_catalogs.contains(&group.anchor_message_id));
            projection.entries.retain(|entry| match &entry.action {
                ProjectionAction::OmitToolExchange {
                    anchor_message_id, ..
                }
                | ProjectionAction::ResultReceipt {
                    anchor_message_id, ..
                } => valid_catalogs.contains(anchor_message_id),
                ProjectionAction::CompactToolResult { .. }
                | ProjectionAction::ReplaceMedia { .. }
                | ProjectionAction::CompactText { .. }
                | ProjectionAction::Exclude => !invalid_member_link,
                _ => true,
            });
            if projection
                .archive_group
                .as_ref()
                .is_some_and(|group| !valid_catalogs.contains(&group.anchor_message_id))
            {
                projection.archive_group = None;
            }
            if projection.archive_catalog.is_some() && !valid_catalogs.contains(id) {
                projection.archive_catalog = None;
            }
            // Range roots describe the surviving source scope; no removed tail remains usable.
            projection.archive_roots = projection
                .archive_roots
                .iter()
                .filter_map(|root| roots.get(&root.source_thread_id).cloned())
                .collect();
            if clear.contains(id)
                && projection.archive_catalog.is_none()
                && projection.archive_group.is_none()
                && projection.archive_roots.is_empty()
            {
                flags.projection = None;
            }
        }
        if flags != *original {
            updates.push((*id, flags));
        }
    }
    updates
}
