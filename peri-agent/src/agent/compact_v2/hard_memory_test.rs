use peri_acp_types::goal::{GoalStatus, GoalViewSnapshot};
use peri_acp_types::tasks::{
    BgTaskKind, ExternalTaskOwner, TaskArtifactRef, TaskOwnerHandle, TaskRecord, TaskSnapshot,
};

use super::{
    from_parts, HardMemorySnapshot, HardMemorySource, HardMemoryTask, RecoveryTools,
    RuntimeHardMemory, HARD_MEMORY_HEADER,
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
        external_owner: None,
    }
}

fn task(task_id: &str, kind: BgTaskKind) -> HardMemoryTask {
    HardMemoryTask {
        kind,
        task_id: task_id.into(),
        child_thread_id: None,
        external_owner: None,
        summary: format!("{task_id} summary"),
        status: "running".into(),
        started_at: "2026-11-11T00:00:00Z".into(),
    }
}

fn external_task(task_id: &str, kind: BgTaskKind, owner: ExternalTaskOwner) -> HardMemoryTask {
    HardMemoryTask {
        external_owner: Some(owner),
        ..task(task_id, kind)
    }
}

fn snapshot_with(tasks: Vec<HardMemoryTask>, tools: RecoveryTools) -> HardMemorySnapshot {
    HardMemorySnapshot {
        tasks,
        goal: None,
        tools,
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
            task("mcp-0199-workflow", BgTaskKind::Workflow),
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
        tools: RecoveryTools::all(),
    };

    let rendered = snapshot.render().expect("non-empty snapshot must render");
    assert!(rendered.starts_with(HARD_MEMORY_HEADER));
    assert!(rendered.contains("Status can become stale"));
    assert!(rendered.contains("Use a newer matching completion reminder"));
    assert!(rendered.contains("Check live status only if it changes the next authorized action"));
    assert!(rendered.contains("Recovery examples grant no authorization"));
    assert!(rendered.contains("## Active background tasks (5)"));
    assert!(rendered.contains("The listed `task_id` is for host task controls"));
    assert!(rendered.contains("Tasks panel or ACP client"));
    assert!(rendered.contains("use `ToolSearch`"));
    assert!(rendered.contains("call it through `ToolInvoke`"));
    assert!(rendered.contains("task_id `shell-0199-shell`"));
    assert!(rendered.contains("If the next step requires the result, wait for the reminder"));
    assert!(rendered.contains("thread_id: `0199-child-thread`"));
    assert!(rendered.contains("Agent({"));
    assert!(rendered.contains("\"resume_thread_id\":\"0199-child-thread\""));
    assert!(rendered.contains("\"send_only\":true"));
    assert!(rendered.contains("\"prompt\":\"<message>\""));
    assert!(rendered.contains("owner `run_id` from the launch receipt"));
    assert!(rendered.contains("A `queued` reply acknowledges message delivery to the queue"));
    assert!(rendered.contains("`WorkflowStatus` with `{}` lists recent runs"));
    assert!(rendered.contains("Pass the owner ID as `runId`"));
    assert!(rendered.contains("`WorkflowKill` for an authorized cancellation"));
    assert!(!rendered.contains("\"<task_id>\""));
    assert!(rendered.contains("`monitor_id` from the start receipt"));
    assert!(rendered.contains("`taskId` from an unambiguous match in `monitor_list`"));
    assert!(rendered.contains("Pass the owner ID as `task_id` to `monitor_read`"));
    assert!(rendered.contains("`monitor_stop` for an authorized stop"));
    assert!(rendered.contains("Resolve these names in the current catalog"));
    assert!(rendered.contains("`nextSeq` of the last page handled for this monitor"));
    assert!(rendered.contains("If unknown, omit the cursor"));
    assert!(!rendered.contains("mcp__peri__"));
    assert!(!rendered.contains("mcp__workflow__"));
    assert!(!rendered.contains("mcp__monitor__"));
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
    let rendered = HardMemorySnapshot {
        tasks,
        goal: None,
        tools: RecoveryTools::all(),
    }
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
        tools: RecoveryTools::all(),
    }
    .render()
    .expect("non-empty snapshot must render");

    assert!(rendered.contains("summary: first line "));
    assert!(!rendered.contains("\nfirst line"));
    assert!(rendered.contains('…'));
}

#[test]
fn external_registration_keeps_typed_owner_handle_and_artifact_into_hard_memory() {
    use peri_acp_types::tasks::{ExternalTaskRegistration, TaskRegistrationTicket};
    use std::sync::Arc;

    let manager = crate::agent::async_tasks::TaskManager::new();
    let cancel: peri_acp_types::tasks::ExternalCancelFn =
        Arc::new(|| Box::pin(async { Ok::<(), String>(()) }));
    let on_terminal: peri_acp_types::tasks::ExternalNotifyFn =
        Arc::new(|_, _| Box::pin(async { Ok::<(), String>(()) }));
    let owner = ExternalTaskOwner::workflow("run-server-typed").with_artifact(TaskArtifactRef {
        reference: "state.json".into(),
        version: Some("v7".into()),
    });
    let _ticket: TaskRegistrationTicket = manager
        .register_external(ExternalTaskRegistration {
            session_id: "session-509".into(),
            initiator_session_id: Some("session-509".into()),
            owner_identity: "workflow".into(),
            owner: owner.clone(),
            kind: BgTaskKind::Workflow,
            summary: "typed workflow".into(),
            started_at: Some("2026-11-11T00:00:00Z".into()),
            cancel,
            on_terminal,
        })
        .expect("typed external registration must be admitted");

    let record = manager
        .snapshot()
        .tasks
        .into_iter()
        .next()
        .expect("registered task must be projected");
    assert_eq!(record.external_owner.as_ref(), Some(&owner));
    assert_ne!(
        record.task_id, "run-server-typed",
        "host task id is independent"
    );

    let snapshot = RuntimeHardMemory::new(Some(Arc::new(manager)), None).snapshot();
    assert_eq!(snapshot.tasks.len(), 1);
    assert_eq!(snapshot.tasks[0].external_owner.as_ref(), Some(&owner));
}

#[test]
fn workflow_handle_renders_direct_call_without_unbounded_index_fallback() {
    let task = external_task(
        "mcp-host-workflow",
        BgTaskKind::Workflow,
        ExternalTaskOwner::workflow("run-server-1"),
    );
    let rendered = snapshot_with(vec![task], RecoveryTools::all())
        .render()
        .expect("non-empty snapshot must render");

    assert!(rendered.contains("owner: workflow run_id `run-server-1`"));
    assert!(rendered.contains("WorkflowStatus({\"runId\":\"run-server-1\"})"));
    assert!(rendered.contains("WorkflowKill({\"runId\":\"run-server-1\"})"));
    assert!(
        !rendered.contains("WorkflowStatus` with `{}`"),
        "已知唯一 runId 时不得回退到有界索引"
    );
    assert!(!rendered.contains("owner `run_id` from the launch receipt"));
}

#[test]
fn monitor_handle_renders_after_seq_only_from_the_same_monitor_page() {
    let task = external_task(
        "mcp-host-monitor",
        BgTaskKind::Mcp,
        ExternalTaskOwner {
            handle: TaskOwnerHandle::Monitor {
                task_id: "monitor-server-1".into(),
                after_seq: Some(7),
            },
            artifact_ref: None,
        },
    );
    let rendered = snapshot_with(vec![task], RecoveryTools::all())
        .render()
        .expect("non-empty snapshot must render");

    assert!(rendered.contains("monitor_read({"));
    assert!(rendered.contains("\"task_id\":\"monitor-server-1\""));
    assert!(rendered.contains("\"after_seq\":7"));
    assert!(rendered.contains("monitor_stop({\"task_id\":\"monitor-server-1\"})"));
    assert!(rendered.contains("`nextSeq` of the last page handled for this monitor"));
    assert!(rendered.contains("not from an alert hit sequence"));
    assert!(
        !rendered.contains("monitor_list"),
        "已知唯一 monitor id 时不得回退到有界索引"
    );
}

#[test]
fn monitor_handle_without_cursor_omits_after_seq_instead_of_inventing_zero() {
    let task = external_task(
        "mcp-host-monitor",
        BgTaskKind::Mcp,
        ExternalTaskOwner::monitor("monitor-server-2"),
    );
    let rendered = snapshot_with(vec![task], RecoveryTools::all())
        .render()
        .expect("non-empty snapshot must render");

    assert!(rendered.contains("monitor_read({"));
    assert!(rendered.contains("\"task_id\":\"monitor-server-2\""));
    assert!(!rendered.contains("\"after_seq\""));
    assert!(rendered.contains("Omit `after_seq`"));
    assert!(rendered.contains("never derive it from an alert hit sequence"));
}

#[test]
fn duplicate_owner_handle_is_ambiguous_and_never_yields_a_direct_call() {
    let tasks = vec![
        external_task(
            "mcp-host-a",
            BgTaskKind::Workflow,
            ExternalTaskOwner::workflow("run-duplicated"),
        ),
        external_task(
            "mcp-host-b",
            BgTaskKind::Workflow,
            ExternalTaskOwner::workflow("run-duplicated"),
        ),
    ];
    let rendered = snapshot_with(tasks, RecoveryTools::all())
        .render()
        .expect("non-empty snapshot must render");

    assert!(!rendered.contains("WorkflowStatus({"));
    assert!(rendered.contains("not unique"));
    assert!(rendered.contains("Do not guess"));
    assert!(rendered.contains("WorkflowStatus` with `{}`"));
}

#[test]
fn missing_owner_handle_keeps_the_target_unknown() {
    let workflow = task("mcp-host-workflow", BgTaskKind::Workflow);
    let mut monitor = task("mcp-host-monitor", BgTaskKind::Mcp);
    // 旧记录没有 typed handle；monitor 只能由旧 summary 前缀识别，仍须保留未知。
    monitor.summary = "monitor_start".into();
    let rendered = snapshot_with(vec![workflow, monitor], RecoveryTools::all())
        .render()
        .expect("non-empty snapshot must render");

    assert!(!rendered.contains("WorkflowStatus({"));
    assert!(!rendered.contains("monitor_read({"));
    assert!(rendered.contains("owner ID is unknown"));
    assert!(rendered.contains("do not infer it from the host task_id"));
    assert!(rendered.contains("WorkflowStatus` with `{}`"));
    assert!(rendered.contains("unambiguous match in `monitor_list`"));
}

#[test]
fn disabled_catalog_capabilities_suppress_executable_calls() {
    let workflow = external_task(
        "mcp-host-workflow",
        BgTaskKind::Workflow,
        ExternalTaskOwner::workflow("run-server-2"),
    );
    let monitor = external_task(
        "mcp-host-monitor",
        BgTaskKind::Mcp,
        ExternalTaskOwner::monitor("monitor-server-3"),
    );
    let mut agent = task("bg-agent", BgTaskKind::Agent);
    agent.child_thread_id = Some("child-thread-3".into());

    let rendered = snapshot_with(vec![workflow, monitor, agent], RecoveryTools::none())
        .render()
        .expect("non-empty snapshot must render");

    assert!(!rendered.contains("WorkflowStatus({"));
    assert!(!rendered.contains("WorkflowKill({"));
    assert!(!rendered.contains("monitor_read({"));
    assert!(!rendered.contains("monitor_stop({"));
    assert!(!rendered.contains("Agent({"));
    assert!(rendered.contains("run-server-2"), "登记句柄仍应展示");
    assert!(rendered.contains("not directly available in the current catalog"));
}

#[test]
fn agent_without_child_thread_does_not_invent_a_resume_target() {
    let agent = task("bg-agent", BgTaskKind::Agent);
    let rendered = snapshot_with(vec![agent], RecoveryTools::all())
        .render()
        .expect("non-empty snapshot must render");

    assert!(!rendered.contains("Agent({"));
    assert!(rendered.contains("`thread_id` is unknown"));
    assert!(rendered.contains("do not infer it from task_id"));
}

#[test]
fn artifact_versions_are_rendered_with_their_provenance() {
    let mut workflow = external_task(
        "mcp-host-workflow",
        BgTaskKind::Workflow,
        ExternalTaskOwner::workflow("run-server-3"),
    );
    workflow
        .external_owner
        .as_mut()
        .expect("fixture owner")
        .artifact_ref = Some(TaskArtifactRef {
        reference: "reports/state.json".into(),
        version: Some("v9".into()),
    });

    let rendered = snapshot_with(vec![workflow], RecoveryTools::all())
        .render()
        .expect("non-empty snapshot must render");

    assert!(rendered.contains("artifact: `reports/state.json` @ `v9`"));
    assert!(rendered.contains("reuse the recorded conclusion"));
}

struct CatalogTool {
    name: &'static str,
    original: Option<&'static str>,
    direct: bool,
    visible: bool,
}

#[async_trait::async_trait]
impl crate::tools::BaseTool for CatalogTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "catalog fixture"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    fn is_direct(&self) -> bool {
        self.direct
    }
    fn visible_to_model(&self) -> bool {
        self.visible
    }
    fn mcp_tool_name(&self) -> Option<&str> {
        self.original
    }
    async fn invoke(
        &self,
        _: serde_json::Value,
        _: crate::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        unreachable!()
    }
}

fn catalog(tools: Vec<CatalogTool>) -> crate::session::tool_catalog::SessionToolCatalogSnapshot {
    use crate::session::tool_catalog::{CatalogToolEntry, ToolSource};
    use std::collections::BTreeMap;
    use std::sync::Arc;

    let tools = tools
        .into_iter()
        .map(|tool| {
            let name = tool.name.to_string();
            (
                name,
                CatalogToolEntry {
                    tool: Arc::new(tool) as Arc<dyn crate::tools::BaseTool>,
                    source: ToolSource::CoreOrMiddleware,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    crate::session::tool_catalog::SessionToolCatalogSnapshot {
        generation: 1,
        tools,
        ..Default::default()
    }
}

#[test]
fn deferred_catalog_tool_renders_through_the_current_invoke_proxy() {
    let tools = RecoveryTools::from_catalog(&catalog(vec![
        CatalogTool {
            name: "ToolInvoke",
            original: None,
            direct: true,
            visible: true,
        },
        CatalogTool {
            name: "mcp__peri__WorkflowStatus",
            original: Some("WorkflowStatus"),
            direct: false,
            visible: true,
        },
    ]));
    let route = tools
        .workflow_status
        .as_ref()
        .expect("deferred workflow status must be routed through ToolInvoke");
    assert_eq!(
        route.invocation,
        super::RecoveryInvocation::Deferred {
            invoke_tool: "ToolInvoke".into()
        }
    );

    let task = external_task(
        "mcp-host-workflow",
        BgTaskKind::Workflow,
        ExternalTaskOwner::workflow("run-server-4"),
    );
    let rendered = snapshot_with(vec![task], tools)
        .render()
        .expect("non-empty snapshot must render");
    assert!(rendered.contains(
        "ToolInvoke({\"name\":\"mcp__peri__WorkflowStatus\",\"arguments\":{\"runId\":\"run-server-4\"}})"
    ));
    assert!(!rendered.contains("WorkflowStatus({\"runId\""));
}

#[test]
fn legacy_catalog_tool_uses_the_name_visible_to_that_session() {
    let tools = RecoveryTools::from_catalog(&catalog(vec![CatalogTool {
        name: "mcp__workflow__WorkflowStatus",
        original: Some("WorkflowStatus"),
        direct: true,
        visible: true,
    }]));
    assert_eq!(
        tools
            .workflow_status
            .as_ref()
            .expect("direct legacy route")
            .tool_name,
        "mcp__workflow__WorkflowStatus"
    );

    let task = external_task(
        "mcp-host-workflow",
        BgTaskKind::Workflow,
        ExternalTaskOwner::workflow("run-server-5"),
    );
    let rendered = snapshot_with(vec![task], tools)
        .render()
        .expect("non-empty snapshot must render");
    assert!(rendered.contains("mcp__workflow__WorkflowStatus({\"runId\":\"run-server-5\"})"));
}

#[test]
fn empty_or_hidden_catalog_keeps_capabilities_off() {
    let tools = RecoveryTools::from_catalog(&catalog(Vec::new()));
    assert_eq!(tools, RecoveryTools::none());

    let hidden = RecoveryTools::from_catalog(&catalog(vec![CatalogTool {
        name: "WorkflowStatus",
        original: None,
        direct: true,
        visible: false,
    }]));
    assert_eq!(hidden, RecoveryTools::none());
}
