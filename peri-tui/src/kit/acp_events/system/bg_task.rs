//! Background Tasks 事件处理器 — §4.7：快照 / 增量与任务生命周期。
//!
//! 由 `system` 挂载；`system::handle_bg_task_*` / `system::apply_bg_task_snapshot` /
//! `system::request_bg_task_snapshot` 路径经 `pub use` 保持不变。

use super::*;
use crate::i18n;
use crate::kit::acp_types::{BgTaskEntry, BgTaskSnapshotSource};
use crate::kit::atoms::{
    ACP_CLIENT_HANDLE, ACTIVE_SESSION_ID, BG_DISPLAY, BG_LIVE_DETAIL, BG_TASK_IDENTITY,
    BG_TASK_REVISION, BG_TASKS, NOTIFICATION,
};
use crate::kit::bg_task_identity::{
    METADATA_TEXT_BYTES, RECENT_METADATA_BYTES, RECENT_METADATA_ENTRIES, bounded_label,
    protected_task_ids, prune_metadata, upsert_identity_from_started,
};
use crate::kit::bg_task_live::{
    mark_task_cancelled, mark_task_completed, seed_live_from_running_snapshot,
    seed_live_from_started, seed_live_from_terminal_snapshot, seed_unobserved_snapshot,
};
use crate::kit::bg_task_status::BgTaskStatus;
use fluent_bundle::FluentValue;
use serde_json::Value;
use std::time::Duration;

// ── §4.7 Background Tasks ──

pub(crate) fn apply_bg_task_snapshot(
    tasks: &[BgTaskEntry],
    revision: Option<u64>,
    source: BgTaskSnapshotSource,
) -> bool {
    if let (Some(incoming), Some(current)) = (revision, *BG_TASK_REVISION.state().read())
        && incoming < current
    {
        return false;
    }
    *BG_TASK_REVISION.state().write() = revision;
    let selected = bounded_snapshot_tasks(tasks, source);
    let tasks: Vec<_> = selected.into_iter().map(project_task).collect();
    BG_TASKS.state().write().clone_from(&tasks);
    let previous_display = BG_DISPLAY.state().read().clone();
    let live_source = source == BgTaskSnapshotSource::Live;
    // 当前权威 live 快照是活动证据：running/None 可以创建（漏 Started）或恢复
    // （lost 后）live detail。历史回放只建立只读事实，不赋予活动。
    if live_source {
        for task in &tasks {
            if BgTaskStatus::classify(task.status.as_deref()).is_active() {
                seed_live_from_running_snapshot(&task.task_id, &task.kind, &task.summary, task.pid);
            }
        }
    }
    // 从快照全量构造 BG_DISPLAY 条目
    let entries: Vec<crate::kit::atoms::BgDisplayEntry> = tasks
        .iter()
        .filter_map(|t| {
            let status = BgTaskStatus::classify(t.status.as_deref());
            let previous = previous_display.iter().find(|row| row.id == t.task_id);
            let active = live_source && status.is_active();
            if !status.is_terminal() && !active {
                return None;
            }
            Some(crate::kit::atoms::BgDisplayEntry {
                id: t.task_id.clone(),
                linked_agent_id: previous
                    .and_then(|row| row.linked_agent_id.clone())
                    .or_else(|| {
                        BG_TASK_IDENTITY
                            .state()
                            .read()
                            .get(&t.task_id)
                            .and_then(|identity| identity.agent_id.clone())
                    }),
                agent_type: t.kind.clone(),
                desc: bg_task_description(&t.summary, t.status.as_deref()),
                is_active: active,
                is_error: matches!(status, BgTaskStatus::Failed | BgTaskStatus::Cancelled),
                current_tool: previous.and_then(|row| row.current_tool.clone()),
                tool_count: previous.map_or(0, |row| row.tool_count),
                created_at: previous.map_or_else(peri_time::monotonic_now, |row| row.created_at),
                completed_at: status.is_terminal().then(peri_time::monotonic_now),
            })
        })
        .collect();
    if live_source {
        BG_DISPLAY.state().write().clone_from(&entries);
    }
    for t in &tasks {
        upsert_identity_from_started(&t.task_id, &t.kind, &t.summary, t.pid);
        let status = BgTaskStatus::classify(t.status.as_deref());
        if status.is_terminal() {
            // 历史回放不得覆盖当前 live 明细；缺失时才补只读终态事实。
            if live_source || !has_live_detail(&t.task_id) {
                seed_live_from_terminal_snapshot(
                    &t.task_id,
                    &t.kind,
                    &t.summary,
                    t.pid,
                    t.status.as_deref().unwrap_or_default(),
                );
            }
        } else if live_source {
            if !status.is_active() {
                seed_unobserved_snapshot(&t.task_id, &t.kind, &t.summary, t.pid);
            }
        } else if !has_live_detail(&t.task_id) {
            seed_unobserved_snapshot(&t.task_id, &t.kind, &t.summary, t.pid);
        }
    }
    if live_source {
        crate::kit::bg_task_live::reconcile_snapshot_tasks(&protected_task_ids());
    }
    prune_metadata();
    true
}

fn project_task(task: &BgTaskEntry) -> BgTaskEntry {
    BgTaskEntry {
        task_id: task.task_id.clone(),
        kind: bounded_label(&task.kind),
        summary: bounded_label(&task.summary),
        started_at: bounded_label(&task.started_at),
        pid: task.pid,
        revision: task.revision,
        status: task.status.as_deref().map(bounded_label),
    }
}

/// Live nonterminal control rows, including pending delivery, survive. Historical
/// status text is not live evidence: all historical rows share the recent budget.
fn bounded_snapshot_tasks(
    tasks: &[BgTaskEntry],
    source: BgTaskSnapshotSource,
) -> Vec<&BgTaskEntry> {
    let (mut count, mut bytes) = (0usize, 0usize);
    let mut selected = Vec::new();
    for task in tasks.iter().rev() {
        if source == BgTaskSnapshotSource::Historical
            || BgTaskStatus::classify(task.status.as_deref()).is_terminal()
        {
            let size = std::mem::size_of::<BgTaskEntry>()
                + task.task_id.len()
                + task.kind.len().min(METADATA_TEXT_BYTES)
                + task.summary.len().min(METADATA_TEXT_BYTES)
                + task.started_at.len().min(METADATA_TEXT_BYTES)
                + task
                    .status
                    .as_ref()
                    .map_or(0, |status| status.len().min(METADATA_TEXT_BYTES));
            if count >= RECENT_METADATA_ENTRIES
                || bytes.saturating_add(size) > RECENT_METADATA_BYTES
            {
                continue;
            }
            count += 1;
            bytes += size;
        }
        selected.push(task);
    }
    selected.reverse();
    selected
}

fn has_live_detail(task_id: &str) -> bool {
    BG_LIVE_DETAIL.state().read().contains_key(task_id)
}

fn bg_task_description(summary: &str, status: Option<&str>) -> String {
    let summary = bounded_label(summary);
    match BgTaskStatus::classify(status) {
        BgTaskStatus::Lost => format!("{summary} · {}", i18n::tr("bg-task-status-lost")),
        BgTaskStatus::Reconciling => {
            format!("{summary} · {}", i18n::tr("bg-task-status-reconciling"))
        }
        _ => summary,
    }
}

pub(crate) fn handle_bg_task_snapshot(
    state: &mut BridgeState,
    tasks: &[BgTaskEntry],
    revision: Option<u64>,
    source: BgTaskSnapshotSource,
) {
    if !apply_bg_task_snapshot(tasks, revision, source) {
        return;
    }
    crate::kit::acp_events::render::push_acp_state(state);
}

pub(crate) fn request_bg_task_snapshot() {
    let Some(client) = ACP_CLIENT_HANDLE.get().cloned() else {
        return;
    };
    let session_id = ACTIVE_SESSION_ID.state().read().clone();
    if session_id.is_empty() {
        return;
    }
    tokio::spawn(async move {
        let result = client
            .send_raw_request(
                "session/bg-tasks",
                serde_json::json!({ "sessionId": session_id }),
            )
            .await;
        let Ok(value) = result else {
            return;
        };
        if ACTIVE_SESSION_ID.state().read().as_str() != session_id {
            return;
        }
        let revision = value.get("revision").and_then(Value::as_u64);
        let Some(tasks) = value.get("tasks").cloned() else {
            return;
        };
        if let Ok(tasks) = serde_json::from_value::<Vec<BgTaskEntry>>(tasks) {
            apply_bg_task_snapshot(&tasks, revision, BgTaskSnapshotSource::Live);
        }
    });
}

fn accept_bg_task_delta(revision: Option<u64>) -> bool {
    let Some(revision) = revision else {
        return true;
    };
    if let Some(current) = *BG_TASK_REVISION.state().read() {
        if revision <= current {
            return false;
        }
        if revision > current.saturating_add(1) {
            request_bg_task_snapshot();
            return false;
        }
    }
    *BG_TASK_REVISION.state().write() = Some(revision);
    true
}

pub(crate) fn handle_bg_task_started(_state: &mut BridgeState, entry: &BgTaskEntry) {
    if !accept_bg_task_delta(entry.revision) {
        return;
    }
    // A snapshot can arrive before a buffered started event. Keep one row per
    // task and do not resurrect a task that has already reached a terminal state.
    if BG_TASKS.state().read().iter().any(|task| {
        task.task_id == entry.task_id
            && BgTaskStatus::classify(task.status.as_deref()).is_terminal()
    }) {
        return;
    }
    {
        let tasks = BG_TASKS.state();
        let mut tasks = tasks.write();
        if let Some(existing) = tasks.iter_mut().find(|t| t.task_id == entry.task_id) {
            *existing = project_task(entry);
        } else {
            tasks.push(project_task(entry));
        }
    }
    upsert_identity_from_started(&entry.task_id, &entry.kind, &entry.summary, entry.pid);
    seed_live_from_started(&entry.task_id, &entry.kind, &entry.summary, entry.pid);
    let display_entry = crate::kit::atoms::BgDisplayEntry {
        id: entry.task_id.clone(),
        linked_agent_id: None,
        agent_type: bounded_label(&entry.kind),
        desc: bg_task_description(&entry.summary, entry.status.as_deref()),
        is_active: true,
        is_error: false,
        current_tool: None,
        tool_count: 0,
        created_at: peri_time::monotonic_now(),
        completed_at: None,
    };
    let display = BG_DISPLAY.state();
    let mut display = display.write();
    if let Some(existing) = display.iter_mut().find(|e| e.id == entry.task_id) {
        existing.agent_type = bounded_label(&entry.kind);
        existing.desc = bg_task_description(&entry.summary, entry.status.as_deref());
        existing.is_active = true;
        existing.completed_at = None;
    } else {
        display.push(display_entry);
    }
    drop(display);
    prune_metadata();
}

pub(crate) fn handle_bg_task_completed(
    task_id: &str,
    success: bool,
    duration_ms: u64,
    output_preview: Option<String>,
    revision: Option<u64>,
) {
    if !accept_bg_task_delta(revision) {
        return;
    }
    BG_TASKS.state().write().retain(|t| t.task_id != *task_id);
    mark_task_completed(task_id, success, duration_ms, output_preview);
    // 标记后台显示条目为完成（保留 3s 后自动清除）
    let now = peri_time::monotonic_now();
    if let Some(entry) = BG_DISPLAY
        .state()
        .write()
        .iter_mut()
        .find(|e| e.id == *task_id)
    {
        entry.is_active = false;
        entry.is_error = !success;
        entry.completed_at = Some(now);
    }
    prune_metadata();
    let msg = if success {
        i18n::tr_args(
            "app-note-bg-task-completed",
            &[(
                "duration".into(),
                FluentValue::from(duration_ms as f64 / 1000.0),
            )],
        )
    } else {
        i18n::tr_args(
            "app-note-bg-task-failed",
            &[(
                "duration".into(),
                FluentValue::from(duration_ms as f64 / 1000.0),
            )],
        )
    };
    NOTIFICATION
        .state()
        .write()
        .replace(crate::kit::atoms::Notification {
            message: msg,
            until: peri_time::monotonic_now() + Duration::from_millis(1500),
        });
}

pub(crate) fn handle_bg_task_cancelled(task_id: &str, reason: &str, revision: Option<u64>) {
    if !accept_bg_task_delta(revision) {
        return;
    }
    BG_TASKS.state().write().retain(|t| t.task_id != *task_id);
    mark_task_cancelled(task_id, reason);
    // 标记后台显示条目为失败（3s 倒计时）
    let now = peri_time::monotonic_now();
    if let Some(entry) = BG_DISPLAY
        .state()
        .write()
        .iter_mut()
        .find(|e| e.id == *task_id)
    {
        entry.is_active = false;
        entry.is_error = true;
        entry.completed_at = Some(now);
    }
    prune_metadata();
}

pub(crate) fn handle_bg_task_updated(task_id: &str, status: &str, revision: Option<u64>) {
    if !accept_bg_task_delta(revision) {
        return;
    }
    let tasks = BG_TASKS.state();
    let mut tasks = tasks.write();
    if let Some(task) = tasks.iter_mut().find(|task| task.task_id == task_id) {
        task.status = Some(bounded_label(status));
        let description = bg_task_description(&task.summary, task.status.as_deref());
        if let Some(entry) = BG_DISPLAY
            .state()
            .write()
            .iter_mut()
            .find(|entry| entry.id == task_id)
        {
            entry.desc = description;
        }
    }
    let snapshot = tasks.clone();
    drop(tasks);
    apply_bg_task_snapshot(&snapshot, revision, BgTaskSnapshotSource::Live);
}

#[cfg(test)]
#[path = "bg_task_metadata_test.rs"]
mod metadata_tests;
