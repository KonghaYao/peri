//! Background task `task_id` ↔ `agent_id` 双键解析。

use crate::kit::atoms::{
    BG_DISPLAY, BG_LIVE_DETAIL, BG_TASK_IDENTITY, BG_TASKS, BgDisplayEntry, BgLiveStatus,
};
use crate::kit::bg_task_status::BgTaskStatus;
use std::collections::HashSet;

pub(crate) const METADATA_TEXT_BYTES: usize = 1024;
pub(crate) const RECENT_METADATA_BYTES: usize = 512 * 1024;
pub(crate) const RECENT_METADATA_ENTRIES: usize = 512;

/// A display label, not a control identifier. Clip before allocating the copy.
pub(crate) fn bounded_label(value: &str) -> String {
    if value.len() <= METADATA_TEXT_BYTES {
        return value.to_owned();
    }
    let mut end = METADATA_TEXT_BYTES - "…".len();
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    let mut result = String::with_capacity(end + "…".len());
    result.push_str(&value[..end]);
    result.push('…');
    result
}

pub fn bind_linked_agent_on_subagent_started(
    agent_id: &str,
    agent_name: &str,
    instance_id: Option<&str>,
) -> Option<String> {
    // A missing old Stop can leave its detail Running. Only the same display
    // occurrence proves a duplicate; a new run must claim its new unbound row.
    let same_run_tasks: HashSet<_> = BG_LIVE_DETAIL
        .state()
        .read()
        .iter()
        .filter(|(_, detail)| {
            detail.status == BgLiveStatus::Running
                && detail.agent_id.as_deref() == Some(agent_id)
                && instance_id.is_some()
                && detail.subagent_instance_id.as_deref() == instance_id
        })
        .map(|(id, _)| id.clone())
        .collect();
    let claimed_tasks: HashSet<_> = BG_LIVE_DETAIL
        .state()
        .read()
        .iter()
        .filter(|(_, detail)| detail.subagent_instance_id.is_some())
        .map(|(id, _)| id.clone())
        .collect();
    let display_store = BG_DISPLAY.state();
    let task_id = {
        let display = display_store.read();
        if let Some(existing) = display.iter().rev().find(|entry| {
            entry.is_active
                && entry.linked_agent_id.as_deref() == Some(agent_id)
                && same_run_tasks.contains(&entry.id)
        }) {
            existing.id.clone()
        } else {
            let entry = display.iter().rev().find(|entry| {
                entry.is_active
                    && entry.linked_agent_id.as_deref() == Some(agent_id)
                    && !claimed_tasks.contains(&entry.id)
            })?;
            entry.id.clone()
        }
    };
    let identity_store = BG_TASK_IDENTITY.state();
    if let Some(id) = identity_store.write().get_mut(&task_id) {
        id.agent_id = Some(agent_id.to_string());
        id.agent_name = Some(bounded_label(agent_name));
    }
    Some(task_id)
}

/// Registry ownership is authoritative even if SubagentStarted arrives first.
/// Never infer this edge from the most recently displayed unbound task.
pub(crate) fn bind_registered_child(task_id: &str, child_thread_id: Option<&str>) {
    let Some(child) = child_thread_id else {
        return;
    };
    let observed = highest_known_run(child, Some(task_id));
    if let Some(identity) = BG_TASK_IDENTITY.state().write().get_mut(task_id) {
        identity.agent_id = Some(child.to_owned());
        if let Some(observed) = observed {
            identity.run_floor = Some(
                identity
                    .run_floor
                    .map_or(observed, |floor| floor.max(observed)),
            );
        }
    }
    if let Some(row) = BG_DISPLAY
        .state()
        .write()
        .iter_mut()
        .find(|row| row.id == task_id)
    {
        row.linked_agent_id = Some(child.to_owned());
    }
    if let Some(detail) = BG_LIVE_DETAIL.state().write().get_mut(task_id) {
        detail.agent_id = Some(child.to_owned());
    }
}

fn highest_known_run(child: &str, exclude_task: Option<&str>) -> Option<u64> {
    BG_TASK_IDENTITY
        .state()
        .read()
        .iter()
        .filter(|(task_id, identity)| {
            exclude_task != Some(task_id.as_str()) && identity.agent_id.as_deref() == Some(child)
        })
        .filter_map(|(_, identity)| {
            identity
                .run_order
                .into_iter()
                .chain(identity.run_floor)
                .max()
        })
        .max()
}

/// Bind the producer's execution occurrence to a task row. The child id is
/// reused by resume, so later child events must match this exact occurrence.
pub(crate) fn bind_task_run(task_id: &str, run_order: u64) {
    let identities = BG_TASK_IDENTITY.state();
    let mut identities = identities.write();
    let Some(identity) = identities.get_mut(task_id) else {
        return;
    };
    identity.run_order = Some(run_order);
    let child = identity.agent_id.clone();
    let Some(child) = child else {
        return;
    };
    // Every other row for this child was created before this occurrence became
    // known; a Stop at or below it is older than that row's run.
    for (id, other) in identities.iter_mut() {
        if id != task_id && other.agent_id.as_deref() == Some(child.as_str()) {
            other.run_floor = Some(
                other
                    .run_floor
                    .map_or(run_order, |floor| floor.max(run_order)),
            );
        }
    }
}

/// Resolve the task row owned by a concrete child execution occurrence.
///
/// Exact occurrence wins even when the child id was reused. A run newer than
/// every occurrence bound so far is a start whose Started/snapshot binding was
/// missed, so the newest row for the child may claim it; an older or equal run
/// without an exact owner is stale and must never settle a newer task.
pub(crate) fn task_id_for_child_run(agent_id: &str, run_order: u64) -> Option<String> {
    let identities = BG_TASK_IDENTITY.state();
    let identities = identities.read();
    if let Some(task_id) = identities
        .iter()
        .find(|(_, identity)| {
            identity.agent_id.as_deref() == Some(agent_id) && identity.run_order == Some(run_order)
        })
        .map(|(task_id, _)| task_id.clone())
    {
        return Some(task_id);
    }
    let stale = identities.iter().any(|(_, identity)| {
        identity.agent_id.as_deref() == Some(agent_id)
            && (identity.run_order.is_some_and(|known| known >= run_order)
                || identity.run_floor.is_some_and(|floor| floor >= run_order))
    });
    drop(identities);
    if stale {
        return None;
    }
    newest_display_task_for_child(agent_id)
}

/// Whether `run_order` is still the newest occurrence known for the child.
/// A stale Stop must not unregister the child while a newer run is live.
pub(crate) fn is_latest_child_run(agent_id: &str, run_order: u64) -> bool {
    BG_TASK_IDENTITY
        .state()
        .read()
        .iter()
        .filter(|(_, identity)| identity.agent_id.as_deref() == Some(agent_id))
        .all(|(_, identity)| {
            identity.run_order.is_none_or(|known| known <= run_order)
                && identity.run_floor.is_none_or(|floor| floor <= run_order)
        })
}

fn newest_display_task_for_child(agent_id: &str) -> Option<String> {
    BG_DISPLAY
        .state()
        .read()
        .iter()
        .rev()
        .find(|entry| entry.linked_agent_id.as_deref() == Some(agent_id))
        .map(|entry| entry.id.clone())
}

pub fn resolve_subagent_id_for_display(entry: &BgDisplayEntry) -> Option<String> {
    (entry.agent_type == "agent").then(|| entry.id.clone())
}

pub fn task_id_for_agent_id(agent_id: &str) -> Option<String> {
    let display_store = BG_DISPLAY.state();
    {
        let display = display_store.read();
        if let Some(task_id) = display
            .iter()
            .rev()
            .find(|e| e.linked_agent_id.as_deref() == Some(agent_id))
            .map(|e| e.id.clone())
        {
            return Some(task_id);
        }
    }
    let identity_store = BG_TASK_IDENTITY.state();
    {
        let identities = identity_store.read();
        for (task_id, identity) in identities.iter() {
            if identity.agent_id.as_deref() == Some(agent_id) {
                return Some(task_id.clone());
            }
        }
    }
    let display = display_store.read();
    display
        .iter()
        .find(|e| e.id == agent_id)
        .map(|e| e.id.clone())
}

pub fn with_bg_display_for_agent<F>(agent_id: &str, f: F)
where
    F: FnOnce(&mut BgDisplayEntry),
{
    if let Some(task_id) = task_id_for_agent_id(agent_id) {
        let display_store = BG_DISPLAY.state();
        let mut display = display_store.write();
        if let Some(entry) = display.iter_mut().find(|e| e.id == task_id) {
            f(entry);
            if let Some(tool) = &mut entry.current_tool {
                *tool = bounded_label(tool);
            }
            return;
        }
    }
    let display_store = BG_DISPLAY.state();
    let mut display = display_store.write();
    if let Some(entry) = display
        .iter_mut()
        .find(|e| e.linked_agent_id.as_deref() == Some(agent_id) || e.id == agent_id)
    {
        f(entry);
        if let Some(tool) = &mut entry.current_tool {
            *tool = bounded_label(tool);
        }
    }
}

pub fn upsert_identity_from_started(task_id: &str, kind: &str, summary: &str, _pid: Option<u32>) {
    let identity_store = BG_TASK_IDENTITY.state();
    let mut identities = identity_store.write();
    let identity = identities.entry(task_id.to_owned()).or_default();
    identity.kind = bounded_label(kind);
    identity.summary = bounded_label(summary);
}

pub(crate) fn protected_task_ids() -> HashSet<String> {
    let mut protected: HashSet<_> = BG_TASKS
        .state()
        .read()
        .iter()
        .filter(|task| !BgTaskStatus::classify(task.status.as_deref()).is_terminal())
        .map(|task| task.task_id.clone())
        .collect();
    protected.extend(
        BG_DISPLAY
            .state()
            .read()
            .iter()
            .filter(|row| row.is_active)
            .map(|row| row.id.clone()),
    );
    protected
}

pub(crate) fn display_reserved(entry: &BgDisplayEntry) -> usize {
    std::mem::size_of::<BgDisplayEntry>()
        + entry.id.capacity()
        + entry.agent_type.capacity()
        + entry.desc.capacity()
        + entry.linked_agent_id.as_ref().map_or(0, String::capacity)
        + entry.current_tool.as_ref().map_or(0, String::capacity)
}

/// Serial event-boundary cleanup. Read each source completely before taking the
/// next lock; no lock is held while calling another atom's writer.
pub(crate) fn prune_metadata() {
    let protected = protected_task_ids();
    {
        let tasks = BG_TASKS.state();
        let mut tasks = tasks.write();
        if tasks.capacity() > tasks.len().saturating_mul(2).saturating_add(64) {
            tasks.shrink_to_fit();
        }
    }
    {
        let display = BG_DISPLAY.state();
        let mut display = display.write();
        let (mut count, mut bytes) = (0usize, 0usize);
        let mut keep = vec![true; display.len()];
        for (index, row) in display.iter().enumerate().rev() {
            if row.is_active || protected.contains(&row.id) {
                continue;
            }
            let size = display_reserved(row);
            if count >= RECENT_METADATA_ENTRIES
                || bytes.saturating_add(size) > RECENT_METADATA_BYTES
            {
                keep[index] = false;
            } else {
                count += 1;
                bytes += size;
            }
        }
        let mut index = 0;
        display.retain(|_| {
            let retain = keep[index];
            index += 1;
            retain
        });
        if display.capacity() > display.len().saturating_mul(2).saturating_add(64) {
            display.shrink_to_fit();
        }
    }
    let mut recent: Vec<_> = BG_LIVE_DETAIL
        .state()
        .read()
        .iter()
        .filter(|(id, _)| !protected.contains(*id))
        .map(|(id, detail)| (id.clone(), detail.preview_updated))
        .collect();
    recent.sort_unstable_by_key(|item| std::cmp::Reverse(item.1));
    let identities = BG_TASK_IDENTITY.state();
    let mut identities = identities.write();
    let mut retained = HashSet::new();
    let (mut count, mut bytes) = (0usize, 0usize);
    for (id, _) in recent {
        let Some(identity) = identities.get(&id) else {
            continue;
        };
        let size = std::mem::size_of_val(identity)
            + id.capacity()
            + identity.kind.capacity()
            + identity.summary.capacity()
            + identity.agent_id.as_ref().map_or(0, String::capacity)
            + identity.agent_name.as_ref().map_or(0, String::capacity);
        if count < RECENT_METADATA_ENTRIES && bytes.saturating_add(size) <= RECENT_METADATA_BYTES {
            count += 1;
            bytes += size;
            retained.insert(id);
        }
    }
    identities.retain(|id, _| protected.contains(id) || retained.contains(id));
    if identities.capacity() > identities.len().saturating_mul(2).saturating_add(64) {
        identities.shrink_to_fit();
    }
}

#[cfg(test)]
#[path = "bg_task_identity_test.rs"]
mod tests;
