use super::*;
use crate::kit::atoms::{BgDisplayEntry, BgLiveDetail, BgTaskIdentity};
use std::collections::HashMap;

struct Restore {
    tasks: Vec<BgTaskEntry>,
    display: Vec<BgDisplayEntry>,
    details: HashMap<String, BgLiveDetail>,
    identities: HashMap<String, BgTaskIdentity>,
    revision: Option<u64>,
}

impl Restore {
    fn empty() -> Self {
        crate::kit::atoms::init_atoms();
        Self {
            tasks: std::mem::take(&mut *BG_TASKS.state().write()),
            display: std::mem::take(&mut *BG_DISPLAY.state().write()),
            details: std::mem::take(&mut *BG_LIVE_DETAIL.state().write()),
            identities: std::mem::take(&mut *BG_TASK_IDENTITY.state().write()),
            revision: BG_TASK_REVISION.state().write().take(),
        }
    }
}

impl Drop for Restore {
    fn drop(&mut self) {
        *BG_TASKS.state().write() = std::mem::take(&mut self.tasks);
        *BG_DISPLAY.state().write() = std::mem::take(&mut self.display);
        *BG_LIVE_DETAIL.state().write() = std::mem::take(&mut self.details);
        *BG_TASK_IDENTITY.state().write() = std::mem::take(&mut self.identities);
        BG_TASK_REVISION.set(self.revision);
    }
}

fn task(id: String, status: &str, summary: &str) -> BgTaskEntry {
    BgTaskEntry {
        task_id: id,
        kind: "shell".into(),
        summary: summary.into(),
        started_at: String::new(),
        pid: None,
        revision: None,
        status: Some(status.into()),
    }
}

fn assert_recent_bounds(protected: usize) {
    let display = BG_DISPLAY.state();
    let display = display.read();
    let terminal: Vec<_> = display.iter().filter(|row| !row.is_active).collect();
    assert!(terminal.len() <= RECENT_METADATA_ENTRIES);
    assert!(
        terminal
            .into_iter()
            .map(crate::kit::bg_task_identity::display_reserved)
            .sum::<usize>()
            <= RECENT_METADATA_BYTES
    );
    assert!(display.capacity() <= display.len() * 2 + 64);
    drop(display);
    let identities = BG_TASK_IDENTITY.state();
    let identities = identities.read();
    assert!(identities.len() <= protected + RECENT_METADATA_ENTRIES);
    assert!(
        identities
            .values()
            .all(|identity| identity.summary.capacity() <= METADATA_TEXT_BYTES)
    );
    assert!(identities.capacity() <= identities.len() * 2 + 64);
    let tasks = BG_TASKS.state();
    let tasks = tasks.read();
    assert!(tasks.capacity() <= tasks.len() * 2 + 64);
}

/// [回归测试] 不等待3秒渲染回收，也能限制1000个已结束任务的UI元数据。
#[test]
#[serial_test::serial]
fn terminal_event_burst_bounds_display_identity_and_vector_capacity() {
    let _restore = Restore::empty();
    let mut state = crate::kit::acp_events::acp_events_test::make_fold_test_state();
    let summary = "display description".repeat(2048);
    for index in 0..1000 {
        let entry = task(format!("burst-{index}"), "running", &summary);
        handle_bg_task_started(&mut state, &entry);
        handle_bg_task_completed(&entry.task_id, true, 1, Some("done".into()), None);
    }
    assert!(BG_TASKS.state().read().is_empty());
    assert_recent_bounds(0);
    assert!(!BG_TASK_IDENTITY.state().read().contains_key("burst-0"));
    handle_bg_task_completed("burst-0", true, 1, Some(summary), None);
    assert!(!BG_TASK_IDENTITY.state().read().contains_key("burst-0"));
    assert!(!BG_LIVE_DETAIL.state().read().contains_key("burst-0"));
}

/// [回归测试] 旧终态快照不能无限复制；600个活动任务与pending仍完整接纳。
#[test]
#[serial_test::serial]
fn large_snapshot_bounds_only_terminal_metadata_and_preserves_pending_tasks() {
    let _restore = Restore::empty();
    let mut tasks: Vec<_> = (0..1000)
        .map(|index| task(format!("old-{index}"), "completed", &"x".repeat(8192)))
        .collect();
    tasks.extend((0..600).map(|index| task(format!("active-{index}"), "running", "active")));
    tasks.push(task(
        "pending".into(),
        "pending_delivery",
        "awaiting delivery",
    ));
    assert!(apply_bg_task_snapshot(
        &tasks,
        Some(10),
        BgTaskSnapshotSource::Live
    ));
    assert_eq!(*BG_TASK_REVISION.state().read(), Some(10));
    let projected = BG_TASKS.state();
    let projected = projected.read();
    assert_eq!(
        projected
            .iter()
            .filter(|entry| entry.status.as_deref() == Some("running"))
            .count(),
        600
    );
    assert!(
        projected.iter().any(|entry| entry.task_id == "pending"
            && entry.status.as_deref() == Some("pending_delivery"))
    );
    assert!(projected.len() <= 601 + RECENT_METADATA_ENTRIES);
    drop(projected);
    assert_recent_bounds(601);
    assert!(!apply_bg_task_snapshot(
        &[],
        Some(9),
        BgTaskSnapshotSource::Live
    ));
    assert_eq!(
        BG_DISPLAY
            .state()
            .read()
            .iter()
            .filter(|row| row.is_active)
            .count(),
        600
    );
    assert!(apply_bg_task_snapshot(
        &[],
        Some(11),
        BgTaskSnapshotSource::Live
    ));
    assert!(BG_TASKS.state().read().is_empty());
    assert!(BG_DISPLAY.state().read().is_empty());
    assert!(
        BG_LIVE_DETAIL
            .state()
            .read()
            .values()
            .all(|detail| detail.status != crate::kit::atoms::BgLiveStatus::Running)
    );
    assert_recent_bounds(0);
}

/// Historical running/lost labels are not evidence of active work. Their rows
/// share one bounded recent pool while a current live display stays intact.
#[test]
#[serial_test::serial]
fn historical_nonterminal_snapshot_is_bounded_and_preserves_live_display() {
    let _restore = Restore::empty();
    let live = task("live-shell".into(), "running", "independent shell");
    assert!(apply_bg_task_snapshot(
        &[live],
        Some(1),
        BgTaskSnapshotSource::Live,
    ));
    BG_LIVE_DETAIL
        .state()
        .write()
        .get_mut("live-shell")
        .unwrap()
        .output_preview = Some("current output".into());
    let historical: Vec<_> = (0..1000)
        .map(|index| {
            task(
                format!("historical-{index}"),
                if index % 2 == 0 { "running" } else { "lost" },
                &"old description".repeat(1024),
            )
        })
        .collect();
    assert!(apply_bg_task_snapshot(
        &historical,
        Some(2),
        BgTaskSnapshotSource::Historical,
    ));
    let projected = BG_TASKS.state();
    let projected = projected.read();
    assert!(projected.len() <= RECENT_METADATA_ENTRIES);
    let reserved: usize = projected
        .iter()
        .map(|task| {
            std::mem::size_of::<BgTaskEntry>()
                + task.task_id.capacity()
                + task.kind.capacity()
                + task.summary.capacity()
                + task.started_at.capacity()
                + task.status.as_ref().map_or(0, String::capacity)
        })
        .sum();
    assert!(reserved <= RECENT_METADATA_BYTES);
    assert!(!projected.iter().any(|task| task.task_id == "historical-0"));
    drop(projected);
    assert_eq!(BG_DISPLAY.state().read().len(), 1);
    assert!(BG_DISPLAY.state().read()[0].is_active);
    assert_eq!(BG_DISPLAY.state().read()[0].id, "live-shell");
    let details = BG_LIVE_DETAIL.state();
    let details = details.read();
    assert_eq!(
        details["live-shell"].status,
        crate::kit::atoms::BgLiveStatus::Running
    );
    assert_eq!(
        details["live-shell"].output_preview.as_deref(),
        Some("current output")
    );
    assert!(details.iter().all(|(id, detail)| id == "live-shell"
        || detail.status == crate::kit::atoms::BgLiveStatus::Unobserved));
    drop(details);
    assert!(BG_TASK_IDENTITY.state().read().contains_key("live-shell"));
    assert_recent_bounds(1);
}
