//! Background Tasks 事件处理器 — §4.7：快照 / 增量与任务生命周期。
//!
//! 由 `system` 挂载；`system::handle_bg_task_*` / `system::apply_bg_task_snapshot` /
//! `system::request_bg_task_snapshot` 路径经 `pub use` 保持不变。

use super::*;
use crate::i18n;
use crate::kit::acp_events::SessionPhase;
use crate::kit::acp_types::{AcpEventData, AcpEventWithEpoch, BgTaskEntry, BgTaskSnapshotSource};
use crate::kit::atoms::{
    ACP_CLIENT_HANDLE, ACTIVE_SESSION_ID, BG_DISPLAY, BG_LIVE_DETAIL, BG_TASK_IDENTITY,
    BG_TASK_REVISION, BG_TASKS, BRIDGE_RESET_COUNTER, BgLiveStatus, LOCAL_EVENT_TX, NOTIFICATION,
    PROGRAM_STATUS,
};
use crate::kit::bg_task_identity::{
    METADATA_TEXT_BYTES, PendingChildRun, RECENT_METADATA_BYTES, RECENT_METADATA_ENTRIES,
    bounded_label, protected_task_ids, prune_metadata, upsert_identity_from_started,
};
use crate::kit::bg_task_live::{
    mark_task_cancelled, mark_task_completed, seed_live_from_running_snapshot,
    seed_live_from_started, seed_live_from_terminal_snapshot, seed_unobserved_snapshot,
};
use crate::kit::bg_task_status::BgTaskStatus;
use crate::kit::child_run_routing::ChildRunRouting;
use crate::kit::program_status::{self, ProgramStatus};
use fluent_bundle::FluentValue;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

// ── §4.7 Background Tasks ──

pub(crate) fn apply_bg_task_snapshot(
    tasks: &[BgTaskEntry],
    revision: Option<u64>,
    source: BgTaskSnapshotSource,
) -> bool {
    apply_bg_task_snapshot_with_routing(tasks, revision, source, None)
}

fn apply_bg_task_snapshot_with_routing(
    tasks: &[BgTaskEntry],
    revision: Option<u64>,
    source: BgTaskSnapshotSource,
    routing: Option<&mut ChildRunRouting>,
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
    // The full snapshot is the only upper bound on what is currently active.
    // Consume child occurrences that this snapshot can account for before the
    // per-row binding below; otherwise an old occurrence stays queued and the
    // next registry Started would claim it as its own run.
    let pending = match (live_source, routing) {
        (true, Some(routing)) => snapshot_pending_runs(&tasks, routing),
        _ => HashMap::new(),
    };
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
                linked_agent_id: t.child_thread_id.clone().or_else(|| {
                    previous
                        .and_then(|row| row.linked_agent_id.clone())
                        .or_else(|| {
                            BG_TASK_IDENTITY
                                .state()
                                .read()
                                .get(&t.task_id)
                                .and_then(|identity| identity.agent_id.clone())
                        })
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
        crate::kit::bg_task_identity::bind_registered_child(
            &t.task_id,
            t.child_thread_id.as_deref(),
            pending.get(&t.task_id).copied(),
        );
    }
    if live_source {
        crate::kit::bg_task_live::reconcile_snapshot_tasks(&protected_task_ids());
    }
    prune_metadata();
    true
}

/// One snapshot row's known child occurrence. Terminal rows matter because a
/// full snapshot includes completed tasks for the same child.
struct SnapshotRow<'a> {
    task_id: &'a str,
    active: bool,
    run_order: Option<u64>,
    known: Option<u64>,
}

/// Pair live snapshot rows with child occurrences already seen without a row.
///
/// Every row for the child is evidence, not only the active one: a terminal row
/// can already account for an old occurrence, and binding that occurrence to a
/// newer row is what lets a delayed Stop end the new task. One active row and
/// one occurrence is still the Started-first case, but only when no other row
/// could own the occurrence. An occurrence that is not strictly newer than a
/// known run/floor is retired into a floor and never handed to a later row;
/// only a provably newer occurrence stays queued for a row that has not
/// arrived yet.
fn snapshot_pending_runs(
    tasks: &[BgTaskEntry],
    routing: &mut ChildRunRouting,
) -> HashMap<String, PendingChildRun> {
    let mut rows_by_child: HashMap<&str, Vec<SnapshotRow<'_>>> = HashMap::new();
    for task in tasks {
        let Some(child) = task.child_thread_id.as_deref() else {
            continue;
        };
        let (run_order, known) = {
            let identities = BG_TASK_IDENTITY.state();
            let identities = identities.read();
            identities
                .get(&task.task_id)
                .map_or((None, None), |identity| {
                    (
                        identity.run_order,
                        identity
                            .run_order
                            .into_iter()
                            .chain(identity.run_floor)
                            .max(),
                    )
                })
        };
        rows_by_child.entry(child).or_default().push(SnapshotRow {
            task_id: &task.task_id,
            active: BgTaskStatus::classify(task.status.as_deref()).is_active(),
            run_order,
            known,
        });
    }
    let mut pending = HashMap::new();
    for (child, rows) in rows_by_child {
        let occurrences = routing.drain_unbound_starts(child);
        if occurrences.is_empty() {
            continue;
        }
        // Known run/floor values also include rows that have fallen out of the
        // bounded snapshot; an occurrence at or below any of them is explained.
        let mut known_ceiling = rows.iter().filter_map(|row| row.known).max();
        {
            let identities = BG_TASK_IDENTITY.state();
            let identities = identities.read();
            for identity in identities
                .values()
                .filter(|identity| identity.agent_id.as_deref() == Some(child))
            {
                for known in identity.run_order.into_iter().chain(identity.run_floor) {
                    known_ceiling = Some(known_ceiling.map_or(known, |current| current.max(known)));
                }
            }
        }
        let unaccounted_row = rows.iter().any(|row| row.known.is_none());
        let unaccounted_terminal = rows.iter().any(|row| !row.active && row.known.is_none());
        let active_rows: Vec<_> = rows.iter().filter(|row| row.active).collect();
        // Started-first shortcut: exactly one visible active row, exactly one
        // occurrence, and no terminal row that could own the occurrence instead.
        if !unaccounted_terminal
            && active_rows.len() == 1
            && active_rows[0].run_order.is_none()
            && occurrences.len() == 1
            && known_ceiling.is_none_or(|known| occurrences[0].run_order > known)
        {
            pending.insert(
                active_rows[0].task_id.to_owned(),
                PendingChildRun::Bind(occurrences[0]),
            );
            continue;
        }
        let mut future = Vec::new();
        let mut retired = Vec::new();
        for run in occurrences {
            // An unaccounted visible row could own this occurrence. Only when
            // every visible row has a known run can a strictly newer
            // occurrence belong to a row that has not arrived yet.
            if !unaccounted_row && known_ceiling.is_some_and(|known| run.run_order > known) {
                future.push(run);
            } else {
                retired.push(run);
            }
        }
        for run in future {
            routing.restore_unbound_start(child, run);
        }
        if retired.is_empty() {
            continue;
        }
        let max_retired = retired
            .iter()
            .map(|run| run.run_order)
            .max()
            .unwrap_or_default();
        // Floor every row, including terminal ones: with no active row left,
        // this is the only thing that keeps a delayed Stop from falling back to
        // a later registry row.
        for row in &rows {
            pending.insert(row.task_id.to_owned(), PendingChildRun::Floor(max_retired));
        }
    }
    pending
}

fn project_task(task: &BgTaskEntry) -> BgTaskEntry {
    BgTaskEntry {
        task_id: task.task_id.clone(),
        child_thread_id: task.child_thread_id.clone(),
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

/// wire 上 `BgTaskKind::Agent` 的 serde snake_case 值。
const AGENT_KIND: &str = "agent";

fn is_agent_task(task: &BgTaskEntry) -> bool {
    task.kind == AGENT_KIND
}

/// 是否仍有 async agent 在跑。只计 `BgTaskKind::Agent`，且必须同时具备 live
/// 证据（`BG_LIVE_DETAIL` 为 Running）：历史快照会把非终态任务 seed 为
/// Unobserved，不能据此让程序状态停在 working。shell 是用户显式放到后台的命令，
/// workflow/mcp 的生命周期由各自调度承担，都不代表主程序还在推进任务。
pub(crate) fn has_active_async_agent() -> bool {
    let tasks = BG_TASKS.state();
    let tasks = tasks.read();
    let live = BG_LIVE_DETAIL.state();
    let live = live.read();
    tasks.iter().any(|task| {
        is_agent_task(task)
            && BgTaskStatus::classify(task.status.as_deref()).is_active()
            && live
                .get(&task.task_id)
                .is_some_and(|detail| detail.status == BgLiveStatus::Running)
    })
}

fn task_is_agent(task_id: &str) -> bool {
    BG_TASKS
        .state()
        .read()
        .iter()
        .any(|task| task.task_id == task_id && is_agent_task(task))
}

/// 最后一个 async agent 结束后退出合成的 working。完成/取消事件可能已经由
/// bg 回调启动新 turn（phase=PromptRunning），此时交给该 turn 结算；blocked、
/// error、done、idle 也不在这里覆盖。
fn settle_async_agent_status(state: &BridgeState, success: bool) {
    if state.phase == SessionPhase::PromptRunning || has_active_async_agent() {
        return;
    }
    if *PROGRAM_STATUS.state().read() != ProgramStatus::Working {
        return;
    }
    let status = if success {
        ProgramStatus::Done
    } else {
        ProgramStatus::Idle
    };
    program_status::publish(status);
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
    if !apply_bg_task_snapshot_with_routing(tasks, revision, source, Some(&mut state.child_runs)) {
        return;
    }
    crate::kit::acp_events::render::push_acp_state(state);
    // 权威 live 快照移除最后一个活跃 agent 时，完成回调可能已随 revision
    // gap 被丢弃；这里按成功快照结算，Historical 不赋予活动。
    if source == BgTaskSnapshotSource::Live {
        settle_async_agent_status(state, true);
    }
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
            // Re-enter through the bridge queue so this snapshot is reconciled
            // with child occurrences in event order. Applying it here would
            // leave the bridge's unbound queue behind the restored rows.
            if let Some(tx) = LOCAL_EVENT_TX.get() {
                let _ = tx.send(AcpEventWithEpoch {
                    child_origin: None,
                    view_epoch: BRIDGE_RESET_COUNTER.get(),
                    event: AcpEventData::BgTaskSnapshot {
                        tasks,
                        revision,
                        source: BgTaskSnapshotSource::Live,
                    },
                    active_session_id: session_id,
                });
            } else {
                apply_bg_task_snapshot(&tasks, revision, BgTaskSnapshotSource::Live);
            }
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

pub(crate) fn handle_bg_task_started(state: &mut BridgeState, entry: &BgTaskEntry) {
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
        linked_agent_id: entry.child_thread_id.clone(),
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
    // The child Started may have arrived first (separate async source). Consume
    // its occurrence here so the row owns a concrete run, not only an instance.
    let pending_run = entry
        .child_thread_id
        .as_deref()
        .and_then(|child| state.child_runs.take_unbound_start(child));
    crate::kit::bg_task_identity::bind_registered_child(
        &entry.task_id,
        entry.child_thread_id.as_deref(),
        pending_run.map(PendingChildRun::Bind),
    );
    // Only a row that consumed this row's child occurrence may adopt the
    // current turn instance; otherwise it still belongs to an older run whose
    // Stop has not arrived, and claiming it would hide this row from the new
    // run's Started.
    if pending_run.is_some()
        && let Some(instance_id) = entry
            .child_thread_id
            .as_deref()
            .and_then(|child| state.current_turn.subagent_instance_id(child))
    {
        let already_claimed = BG_LIVE_DETAIL.state().read().iter().any(|(task, detail)| {
            task != &entry.task_id && detail.subagent_instance_id.as_deref() == Some(instance_id)
        });
        if !already_claimed
            && let Some(detail) = BG_LIVE_DETAIL.state().write().get_mut(&entry.task_id)
        {
            detail.subagent_instance_id = Some(instance_id.to_owned());
        }
    }
    prune_metadata();
}

pub(crate) fn handle_bg_task_completed(
    state: &BridgeState,
    task_id: &str,
    kind: Option<&str>,
    success: bool,
    duration_ms: u64,
    output_preview: Option<String>,
    revision: Option<u64>,
) {
    if !accept_bg_task_delta(revision) {
        return;
    }
    let was_agent = task_is_agent(task_id);
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
    if kind.is_some_and(|kind| kind == AGENT_KIND) || was_agent {
        settle_async_agent_status(state, success);
    }
}

pub(crate) fn handle_bg_task_cancelled(
    state: &BridgeState,
    task_id: &str,
    reason: &str,
    revision: Option<u64>,
) {
    if !accept_bg_task_delta(revision) {
        return;
    }
    let was_agent = task_is_agent(task_id);
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
    NOTIFICATION
        .state()
        .write()
        .replace(crate::kit::atoms::Notification {
            message: i18n::tr_args(
                "app-note-bg-task-cancelled",
                &[(
                    "task_id".to_owned(),
                    FluentValue::from(bounded_label(task_id)),
                )],
            ),
            until: peri_time::monotonic_now() + Duration::from_millis(1500),
        });
    if was_agent {
        settle_async_agent_status(state, false);
    }
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
