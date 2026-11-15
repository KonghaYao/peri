//! Background task `task_id` ↔ `agent_id` 双键解析。

use crate::kit::atoms::{
    BG_DISPLAY, BG_LIVE_DETAIL, BG_TASK_IDENTITY, BG_TASKS, BgDisplayEntry, BgLiveStatus,
};
use crate::kit::bg_task_live::pending_live_owner_for_agent;
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

pub fn is_unbound_agent_row(entry: &BgDisplayEntry) -> bool {
    entry.agent_type == "agent" && entry.linked_agent_id.is_none()
}

/// 以 child thread 身份找任务行。该身份由注册时的 live inbox 提供，
/// 与 `SubagentStarted.instance_id` 是同一个 UUID。
pub(crate) fn task_id_for_child_thread(child_thread_id: &str) -> Option<String> {
    BG_TASKS
        .state()
        .read()
        .iter()
        .rev()
        .find(|task| {
            task.kind == "agent" && task.child_thread_id.as_deref() == Some(child_thread_id)
        })
        .map(|task| task.task_id.clone())
}

pub fn bind_linked_agent_on_subagent_started(
    agent_id: &str,
    agent_name: &str,
    instance_id: Option<&str>,
) -> Option<String> {
    // 任务行与 child 共享 child_thread_id：精确身份优先，不依赖跨通道到达顺序。
    if let Some(task_id) = task_id_for_child_thread(agent_id)
        && link_row_to_agent(&task_id, agent_id, agent_name)
    {
        return Some(task_id);
    }
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
    let display_store = BG_DISPLAY.state();
    let task_id = {
        let mut display = display_store.write();
        if let Some(existing) = display.iter().rev().find(|entry| {
            entry.is_active
                && entry.linked_agent_id.as_deref() == Some(agent_id)
                && same_run_tasks.contains(&entry.id)
        }) {
            existing.id.clone()
        } else {
            let entry = display
                .iter_mut()
                .rev()
                .find(|entry| entry.is_active && is_unbound_agent_row(entry))?;
            entry.linked_agent_id = Some(agent_id.to_string());
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

pub fn resolve_subagent_id_for_display(entry: &BgDisplayEntry) -> Option<String> {
    (entry.agent_type == "agent").then(|| entry.id.clone())
}

pub fn task_id_for_agent_id(agent_id: &str) -> Option<String> {
    // 当前 occurrence 的占位 owner 优先于旧保留行/旧 identity：resume 复用
    // agent_id 时，旧任务的终态明细不能抢走新一轮的流式事件。
    if let Some(task_id) = pending_live_owner_for_agent(agent_id) {
        return Some(task_id);
    }
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
    // 反向乱序：任务行尚未出现时，占位 owner 以 agent_id 为键承接迟到预览。
    if BG_LIVE_DETAIL.state().read().contains_key(agent_id) {
        return Some(agent_id.to_string());
    }
    let display = display_store.read();
    display
        .iter()
        .find(|e| e.id == agent_id)
        .map(|e| e.id.clone())
}

/// 把已存在但未绑定 agent 的任务行认领给迟到的 `SubagentStarted` 对应 child。
///
/// 返回是否真的建立了绑定（任务行存在、active 且仍是未绑定 agent 行）。
pub(crate) fn link_row_to_agent(task_id: &str, agent_id: &str, agent_name: &str) -> bool {
    let display_store = BG_DISPLAY.state();
    {
        let mut display = display_store.write();
        let Some(entry) = display.iter_mut().find(|entry| {
            entry.id == task_id
                && entry.is_active
                && (is_unbound_agent_row(entry)
                    || entry.linked_agent_id.as_deref() == Some(agent_id))
        }) else {
            return false;
        };
        entry.linked_agent_id = Some(agent_id.to_string());
    }
    let identity_store = BG_TASK_IDENTITY.state();
    let mut identities = identity_store.write();
    let identity = identities.entry(task_id.to_owned()).or_default();
    identity.agent_id = Some(agent_id.to_string());
    if !agent_name.is_empty() {
        identity.agent_name = Some(bounded_label(agent_name));
    }
    true
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
