use peri_acp_types::goal::{GoalStatus, GoalViewSnapshot};
use peri_acp_types::tasks::{BgTaskKind, TaskRecord, TaskSnapshot};

use super::{
    from_parts, HardMemorySnapshot, HardMemorySource, HardMemoryTask, RuntimeHardMemory,
    HARD_MEMORY_HEADER,
};

fn record(task_id: &str, kind: BgTaskKind, status: &str) -> TaskRecord {
    TaskRecord {
        task_id: task_id.into(),
        kind,
        summary: format!("{task_id} summary"),
        started_at: "2026-11-11T00:00:00Z".into(),
        status: status.into(),
        duration_ms: 0,
        output_preview: None,
        initiator_session_id: None,
        child_thread_id: None,
    }
}

fn task(task_id: &str, kind: BgTaskKind) -> HardMemoryTask {
    HardMemoryTask {
        kind,
        task_id: task_id.into(),
        child_thread_id: None,
        summary: format!("{task_id} summary"),
        status: "running".into(),
        started_at: "2026-11-11T00:00:00Z".into(),
    }
}

#[test]
fn empty_snapshot_renders_none_so_compact_output_is_unchanged() {
    let snapshot = HardMemorySnapshot::default();
    assert!(snapshot.is_empty());
    assert!(snapshot.render().is_none());
}

#[test]
fn renders_runtime_ids_and_kind_specific_helpers() {
    let mut agent = task("bg-0199-agent", BgTaskKind::Agent);
    agent.child_thread_id = Some("0199-child-thread".into());
    let mut monitor = task("mcp-monitor", BgTaskKind::Mcp);
    monitor.summary = "monitor_start".into();
    let snapshot = HardMemorySnapshot {
        tasks: vec![
            task("shell-0199-shell", BgTaskKind::Shell),
            agent,
            task("run-0199-workflow", BgTaskKind::Workflow),
            monitor,
            task("mcp-other", BgTaskKind::Mcp),
        ],
        goal: Some(super::HardMemoryGoal {
            goal_id: Some("goal-0199".into()),
            objective: Some("ship compact hard memory".into()),
            status: GoalStatus::Active,
            token_budget: Some(1000),
            tokens_used: 42,
            blocked_reason: None,
        }),
    };

    let rendered = snapshot.render().expect("non-empty snapshot must render");
    assert!(rendered.starts_with(HARD_MEMORY_HEADER));
    assert!(rendered.contains("## Active background tasks (5)"));
    assert!(rendered.contains("task_id `shell-0199-shell`"));
    assert!(rendered.contains("thread_id: `0199-child-thread`"));
    assert!(rendered.contains("resume_thread_id"));
    assert!(rendered.contains("mcp__workflow__WorkflowStatus"));
    assert!(rendered.contains("mcp__workflow__WorkflowKill"));
    assert!(rendered.contains("mcp__monitor__monitor_list"));
    assert!(rendered.contains("monitor_id"));
    assert!(rendered.contains("goal-0199"));
    assert!(rendered.contains("objective: ship compact hard memory"));
    assert!(rendered.contains("42 / 1000 tokens"));
}

#[test]
fn snapshot_from_parts_keeps_only_running_tasks_and_active_goal() {
    let snapshot = from_parts(
        TaskSnapshot {
            revision: 7,
            tasks: vec![
                record("shell-running", BgTaskKind::Shell, "running"),
                record("shell-done", BgTaskKind::Shell, "completed"),
                record("bg-agent", BgTaskKind::Agent, "running"),
            ],
        },
        Some(GoalViewSnapshot {
            goal_id: Some("goal-active".into()),
            objective: Some("continue".into()),
            status: Some(GoalStatus::Active),
            ..Default::default()
        }),
    );
    assert_eq!(snapshot.tasks.len(), 2);
    assert!(snapshot
        .tasks
        .iter()
        .all(|task| task.status == "running" && task.task_id != "shell-done"));
    assert_eq!(
        snapshot.goal.expect("active goal must be retained").goal_id,
        Some("goal-active".into())
    );
}

#[test]
fn terminal_or_absent_goal_is_not_hard_memory() {
    let completed = from_parts(
        TaskSnapshot {
            revision: 0,
            tasks: Vec::new(),
        },
        Some(GoalViewSnapshot {
            status: Some(GoalStatus::Complete),
            ..Default::default()
        }),
    );
    assert!(completed.is_empty());

    let absent = from_parts(
        TaskSnapshot {
            revision: 0,
            tasks: Vec::new(),
        },
        Some(GoalViewSnapshot::default()),
    );
    assert!(absent.is_empty());
}

#[test]
fn runtime_source_without_registries_is_empty() {
    let snapshot = super::snapshot_from_runtime(None, None);
    assert!(snapshot.is_empty());
    assert!(RuntimeHardMemory::new(None, None).snapshot().is_empty());
}

#[test]
fn runtime_source_uses_task_manager_registration_ids() {
    use peri_acp_types::tasks::{BgTaskKind, BgTaskRegistration, TaskManager as _};

    let manager = crate::agent::async_tasks::TaskManager::new();
    manager
        .register(BgTaskRegistration {
            task_id: "shell-real-runtime-id".into(),
            kind: BgTaskKind::Shell,
            summary: "npm run dev".into(),
            pid: None,
            kill: Some(Box::new(|| {})),
        })
        .expect("the test task registration must be admitted");
    let snapshot = RuntimeHardMemory::new(Some(std::sync::Arc::new(manager)), None).snapshot();
    assert_eq!(snapshot.tasks.len(), 1);
    assert_eq!(snapshot.tasks[0].task_id, "shell-real-runtime-id");
    assert_eq!(snapshot.tasks[0].summary, "npm run dev");
}

#[test]
fn task_cap_reports_omissions_without_inventing_ids() {
    let tasks = (0..22)
        .map(|index| {
            let mut task = task(&format!("bg-{index}"), BgTaskKind::Agent);
            task.child_thread_id = Some(format!("thread-{index}"));
            task
        })
        .collect();
    let rendered = HardMemorySnapshot { tasks, goal: None }
        .render()
        .expect("non-empty snapshot must render");

    assert!(rendered.contains("background subagent tasks (22)"));
    assert!(rendered.contains("bg-19"));
    assert!(
        !rendered.contains("`bg-20`"),
        "超出上限的 id 不得编造或截断列出"
    );
    assert!(rendered.contains("2 more agent task(s) omitted"));
    assert!(rendered.contains("Do not assume their ids or statuses"));
}

#[test]
fn summaries_are_flattened_and_bounded() {
    let mut shell = task("shell-long", BgTaskKind::Shell);
    shell.summary = format!("first line\n{}", "x".repeat(300));
    let rendered = HardMemorySnapshot {
        tasks: vec![shell],
        goal: None,
    }
    .render()
    .expect("non-empty snapshot must render");

    assert!(rendered.contains("summary: first line "));
    assert!(!rendered.contains("\nfirst line"));
    assert!(rendered.contains('…'));
}
