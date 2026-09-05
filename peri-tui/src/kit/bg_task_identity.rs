//! Background task `task_id` ↔ `agent_id` 双键解析。

use crate::kit::atoms::{
    BG_AGENT_IDS, BG_DISPLAY, BG_TASK_IDENTITY, BgDisplayEntry, BgTaskIdentity,
};

pub fn is_unbound_agent_row(entry: &BgDisplayEntry) -> bool {
    entry.agent_type == "agent" && entry.linked_agent_id.is_none()
}

pub fn bind_linked_agent_on_subagent_started(agent_id: &str, agent_name: &str) -> Option<String> {
    let display_store = BG_DISPLAY.state();
    let task_id = display_store.write().iter_mut().rev().find_map(|e| {
        if !is_unbound_agent_row(e) {
            return None;
        }
        e.linked_agent_id = Some(agent_id.to_string());
        Some(e.id.clone())
    })?;
    let identity_store = BG_TASK_IDENTITY.state();
    if let Some(id) = identity_store.write().get_mut(&task_id) {
        id.agent_id = Some(agent_id.to_string());
        id.agent_name = Some(agent_name.to_string());
    }
    Some(task_id)
}

pub fn resolve_subagent_id_for_display(entry: &BgDisplayEntry) -> Option<String> {
    if let Some(linked) = entry.linked_agent_id.clone() {
        return Some(linked);
    }
    if entry.agent_type != "agent" {
        return None;
    }
    let agent_store = BG_AGENT_IDS.state();
    let ids = agent_store.read();
    if ids.len() == 1 {
        return ids.iter().next().cloned();
    }
    None
}

pub fn task_id_for_agent_id(agent_id: &str) -> Option<String> {
    let display_store = BG_DISPLAY.state();
    {
        let display = display_store.read();
        if let Some(task_id) = display
            .iter()
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
    }
}

pub fn upsert_identity_from_started(task_id: &str, kind: &str, summary: &str, _pid: Option<u32>) {
    let identity_store = BG_TASK_IDENTITY.state();
    identity_store.write().insert(
        task_id.to_string(),
        BgTaskIdentity {
            kind: kind.to_string(),
            summary: summary.to_string(),
            agent_id: None,
            agent_name: None,
        },
    );
}

#[cfg(test)]
#[path = "bg_task_identity_test.rs"]
mod tests;
