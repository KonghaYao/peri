//! Cross-layer task ↔ child-run identity regressions.
//!
//! A snapshot must carry `child_thread_id`, and a reused child id must never let
//! an old Stop settle the task of a newer run.

use super::*;
use crate::kit::acp_types::{AcpEventData, BgTaskEntry, BgTaskSnapshotSource};
use crate::kit::atoms::{BG_AGENT_IDS, BG_DISPLAY, BG_LIVE_DETAIL, BG_TASK_IDENTITY, BG_TASKS};
use crate::kit::stream_data::TuiTextChunk;
use crate::kit::tui_render_unit::TuiRenderUnit;
use peri_acp_types::event::ChildEventOrigin;
use peri_acp_types::identity::AttemptId;
use peri_acp_types::session::{ExecutionBinding, TurnId};
use peri_acp_types::tasks::{BgTaskKind, TaskRecord, TaskSnapshot};
use serial_test::serial;

struct RestoreBgAtoms {
    tasks: Vec<BgTaskEntry>,
    display: Vec<crate::kit::atoms::BgDisplayEntry>,
    live: std::collections::HashMap<String, crate::kit::atoms::BgLiveDetail>,
    identity: std::collections::HashMap<String, crate::kit::atoms::BgTaskIdentity>,
    agent_ids: std::collections::HashSet<String>,
    revision: Option<u64>,
}

impl RestoreBgAtoms {
    fn empty() -> Self {
        let saved = Self {
            tasks: BG_TASKS.state().read().clone(),
            display: BG_DISPLAY.state().read().clone(),
            live: BG_LIVE_DETAIL.state().read().clone(),
            identity: BG_TASK_IDENTITY.state().read().clone(),
            agent_ids: BG_AGENT_IDS.state().read().clone(),
            revision: *crate::kit::atoms::BG_TASK_REVISION.state().read(),
        };
        BG_TASKS.state().write().clear();
        BG_DISPLAY.state().write().clear();
        BG_LIVE_DETAIL.state().write().clear();
        BG_TASK_IDENTITY.state().write().clear();
        BG_AGENT_IDS.state().write().clear();
        crate::kit::atoms::BG_TASK_REVISION.set(None);
        saved
    }
}

impl Drop for RestoreBgAtoms {
    fn drop(&mut self) {
        BG_TASKS.set(self.tasks.clone());
        BG_DISPLAY.set(self.display.clone());
        BG_LIVE_DETAIL.set(self.live.clone());
        BG_TASK_IDENTITY.set(self.identity.clone());
        BG_AGENT_IDS.set(self.agent_ids.clone());
        crate::kit::atoms::BG_TASK_REVISION.set(self.revision);
    }
}

fn origin(child: &str, run_order: u64) -> ChildEventOrigin {
    ChildEventOrigin {
        child_thread_id: child.into(),
        execution: ExecutionBinding {
            turn_id: TurnId::new(),
            attempt_id: AttemptId::new(),
        },
        run_order,
        sequence: 1,
    }
}

fn dispatch_run(state: &mut BridgeState, origin: &ChildEventOrigin, event: AcpEventData) {
    let _ = crate::kit::acp_events::dispatch_trusted_structured_for_bridge_with_origin(
        state,
        event,
        Some(origin),
    );
}

fn task_entry(task_id: &str, child: &str) -> BgTaskEntry {
    BgTaskEntry {
        task_id: task_id.into(),
        child_thread_id: Some(child.into()),
        kind: "agent".into(),
        summary: "worker".into(),
        started_at: String::new(),
        pid: None,
        revision: None,
        status: Some("running".into()),
    }
}

fn bubbles(detail: &crate::kit::atoms::BgLiveDetail) -> String {
    detail
        .nested_units
        .iter()
        .filter_map(|unit| match unit {
            TuiRenderUnit::TuiAssistantBubble(bubble) => Some(bubble.text.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
#[serial]
fn live_task_snapshot_keeps_child_identity_for_followup_child_events() {
    let _restore = RestoreBgAtoms::empty();
    let mut state = make_fold_test_state();

    let snapshot = TaskSnapshot {
        revision: 41,
        tasks: vec![
            TaskRecord {
                task_id: "bg-snapshot-child".into(),
                kind: BgTaskKind::Agent,
                summary: "child worker".into(),
                started_at: "2026-11-06T00:00:00Z".into(),
                status: "running".into(),
                duration_ms: 0,
                output_preview: None,
                initiator_session_id: None,
                child_thread_id: Some("child-snapshot".into()),
            },
            TaskRecord {
                task_id: "bg-snapshot-shell".into(),
                kind: BgTaskKind::Shell,
                summary: "echo".into(),
                started_at: "2026-11-06T00:00:00Z".into(),
                status: "running".into(),
                duration_ms: 0,
                output_preview: None,
                initiator_session_id: None,
                child_thread_id: None,
            },
            TaskRecord {
                task_id: "bg-snapshot-recovered".into(),
                kind: BgTaskKind::Agent,
                summary: "missed start".into(),
                started_at: "2026-11-06T00:00:00Z".into(),
                status: "running".into(),
                duration_ms: 0,
                output_preview: None,
                initiator_session_id: None,
                child_thread_id: Some("child-recovered".into()),
            },
        ],
    };
    // 生产序列化（TaskSnapshot::to_wire_value）→ TUI decode/apply。
    let wire = snapshot.to_wire_value();
    assert_eq!(wire["tasks"][0]["child_thread_id"], "child-snapshot");
    let decoded = AcpEventData::decode("bg-task-snapshot", wire);
    let AcpEventData::BgTaskSnapshot {
        tasks,
        revision,
        source,
    } = &decoded
    else {
        panic!("snapshot wire must decode as BgTaskSnapshot");
    };
    assert_eq!(*revision, Some(41));
    assert_eq!(*source, BgTaskSnapshotSource::Live);
    assert_eq!(tasks.len(), 3);
    dispatch_and_notify(&mut state, &decoded);

    // 快照本身必须把精确 task 行绑定到 child，而 shell 任务保持无 child 归属。
    {
        let display = BG_DISPLAY.state();
        let display = display.read();
        let child_row = display
            .iter()
            .find(|row| row.id == "bg-snapshot-child")
            .expect("child task row");
        assert_eq!(child_row.linked_agent_id.as_deref(), Some("child-snapshot"));
        assert!(child_row.is_active);
        let shell_row = display
            .iter()
            .find(|row| row.id == "bg-snapshot-shell")
            .expect("shell task row");
        assert!(shell_row.linked_agent_id.is_none());
    }
    assert_eq!(
        BG_TASK_IDENTITY.state().read()["bg-snapshot-child"]
            .agent_id
            .as_deref(),
        Some("child-snapshot")
    );

    let run = origin("child-snapshot", 21);
    dispatch_run(
        &mut state,
        &run,
        AcpEventData::SubagentStarted {
            agent_id: "child-snapshot".into(),
            agent_name: "coder".into(),
            is_background: true,
            parent_tool_call_id: None,
        },
    );
    dispatch_run(
        &mut state,
        &run,
        AcpEventData::TextChunk(TuiTextChunk {
            text: "snapshot recovered output".into(),
            message_id: None,
            agent_id: Some("child-snapshot".into()),
        }),
    );
    dispatch_run(
        &mut state,
        &run,
        AcpEventData::SubagentStopped {
            agent_id: "child-snapshot".into(),
            result: "done".into(),
            is_error: false,
        },
    );

    {
        let live = BG_LIVE_DETAIL.state();
        let live = live.read();
        let detail = &live["bg-snapshot-child"];
        assert_eq!(detail.agent_id.as_deref(), Some("child-snapshot"));
        assert_eq!(detail.status, crate::kit::atoms::BgLiveStatus::Succeeded);
        assert_eq!(bubbles(detail), "snapshot recovered output");
    }
    let tasks = BG_TASKS.state();
    let tasks = tasks.read();
    assert!(!tasks.iter().any(|task| task.task_id == "bg-snapshot-child"));
    assert!(tasks.iter().any(|task| task.task_id == "bg-snapshot-shell"));
    assert!(
        tasks
            .iter()
            .any(|task| task.task_id == "bg-snapshot-recovered")
    );
    drop(tasks);

    // 漏 Started（Lagged）恢复：快照已把 child 绑定到任务，Stop 仍能精确结算。
    let recovered = origin("child-recovered", 22);
    dispatch_run(
        &mut state,
        &recovered,
        AcpEventData::SubagentStopped {
            agent_id: "child-recovered".into(),
            result: "recovered done".into(),
            is_error: false,
        },
    );
    {
        let live = BG_LIVE_DETAIL.state();
        let live = live.read();
        let detail = &live["bg-snapshot-recovered"];
        assert_eq!(detail.agent_id.as_deref(), Some("child-recovered"));
        assert_eq!(detail.status, crate::kit::atoms::BgLiveStatus::Succeeded);
    }
    assert!(
        !BG_TASKS
            .state()
            .read()
            .iter()
            .any(|task| task.task_id == "bg-snapshot-recovered")
    );
}

#[test]
#[serial]
fn old_child_stop_settles_only_the_previous_run() {
    let _restore = RestoreBgAtoms::empty();
    let mut state = make_fold_test_state();
    let mut old_run = origin("shared-child", 31);
    let mut new_run = origin("shared-child", 32);

    // 旧 child Started 先到：registry 行还不存在，occurrence 只能暂存。
    dispatch_run(
        &mut state,
        &old_run,
        AcpEventData::SubagentStarted {
            agent_id: "shared-child".into(),
            agent_name: "coder".into(),
            is_background: true,
            parent_tool_call_id: None,
        },
    );
    // 旧 registry Started 随后出现，必须把暂存的 occurrence 回填到行上。
    dispatch_and_notify(
        &mut state,
        &AcpEventData::BgTaskStarted(task_entry("bg-old", "shared-child")),
    );
    // 新 registry Started 先把新任务行绑定到复用的 child id。
    dispatch_and_notify(
        &mut state,
        &AcpEventData::BgTaskStarted(task_entry("bg-new", "shared-child")),
    );
    assert_eq!(
        BG_TASK_IDENTITY.state().read()["bg-old"].run_order,
        Some(31),
        "registry 行必须绑定 Started 先到的 occurrence"
    );
    // 旧 Stop 仍按旧 execution origin 到达（sequence 真实递增）。
    old_run.sequence += 1;
    dispatch_run(
        &mut state,
        &old_run,
        AcpEventData::SubagentStopped {
            agent_id: "shared-child".into(),
            result: "old done".into(),
            is_error: false,
        },
    );

    {
        let live = BG_LIVE_DETAIL.state();
        let live = live.read();
        assert_eq!(
            live["bg-old"].status,
            crate::kit::atoms::BgLiveStatus::Succeeded,
            "旧 Stop 只能结算旧任务"
        );
        assert_eq!(
            live["bg-new"].status,
            crate::kit::atoms::BgLiveStatus::Running,
            "旧 Stop 不得结束新任务行"
        );
    }
    let tasks = BG_TASKS.state();
    let tasks = tasks.read();
    assert!(!tasks.iter().any(|task| task.task_id == "bg-old"));
    assert!(
        tasks.iter().any(|task| task.task_id == "bg-new"),
        "新任务必须仍处于 active 注册态"
    );
    drop(tasks);

    // 随后新 run 的 Started/文本/Stop 必须完整归属新任务。
    dispatch_run(
        &mut state,
        &new_run,
        AcpEventData::SubagentStarted {
            agent_id: "shared-child".into(),
            agent_name: "coder".into(),
            is_background: true,
            parent_tool_call_id: None,
        },
    );
    new_run.sequence += 1;
    dispatch_run(
        &mut state,
        &new_run,
        AcpEventData::TextChunk(TuiTextChunk {
            text: "new run output".into(),
            message_id: None,
            agent_id: Some("shared-child".into()),
        }),
    );
    new_run.sequence += 1;
    dispatch_run(
        &mut state,
        &new_run,
        AcpEventData::SubagentStopped {
            agent_id: "shared-child".into(),
            result: "new done".into(),
            is_error: false,
        },
    );

    {
        let live = BG_LIVE_DETAIL.state();
        let live = live.read();
        assert_eq!(
            live["bg-new"].status,
            crate::kit::atoms::BgLiveStatus::Succeeded
        );
        assert_eq!(bubbles(&live["bg-old"]), "");
        assert_eq!(bubbles(&live["bg-new"]), "new run output");
    }
    assert!(
        !BG_TASKS
            .state()
            .read()
            .iter()
            .any(|task| task.task_id == "bg-new")
    );
}

#[test]
#[serial]
fn old_child_tool_events_label_only_the_previous_run() {
    let _restore = RestoreBgAtoms::empty();
    let mut state = make_fold_test_state();
    let mut old_run = origin("shared-child", 41);

    dispatch_run(
        &mut state,
        &old_run,
        AcpEventData::SubagentStarted {
            agent_id: "shared-child".into(),
            agent_name: "coder".into(),
            is_background: true,
            parent_tool_call_id: None,
        },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::BgTaskStarted(task_entry("bg-old", "shared-child")),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::BgTaskStarted(task_entry("bg-new", "shared-child")),
    );

    old_run.sequence += 1;
    dispatch_run(
        &mut state,
        &old_run,
        AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
            agent_id: Some("shared-child".into()),
            tool_id: "tool-old".into(),
            tool_name: "Read".into(),
            input_summary: "old input".into(),
            raw_input: serde_json::Value::Null,
            origin_message_id: None,
            origin_readable: None,
        }),
    );
    {
        let display = BG_DISPLAY.state();
        let display = display.read();
        let old = display.iter().find(|row| row.id == "bg-old").unwrap();
        let new = display.iter().find(|row| row.id == "bg-new").unwrap();
        assert_eq!(old.current_tool.as_deref(), Some("Read"));
        assert_eq!(new.current_tool, None, "旧 run 不得改写新任务行标签");
    }

    old_run.sequence += 1;
    dispatch_run(
        &mut state,
        &old_run,
        AcpEventData::ToolEnded(crate::kit::stream_data::TuiToolEnded {
            agent_id: Some("shared-child".into()),
            tool_id: "tool-old".into(),
            output_summary: "ok".into(),
            is_error: false,
        }),
    );
    let display = BG_DISPLAY.state();
    let display = display.read();
    let old = display.iter().find(|row| row.id == "bg-old").unwrap();
    let new = display.iter().find(|row| row.id == "bg-new").unwrap();
    assert_eq!(old.current_tool, None);
    assert_eq!(old.tool_count, 1);
    assert_eq!(new.tool_count, 0, "旧 run 的计数不得累加到新任务行");
}

#[test]
#[serial]
fn old_child_system_reminder_stays_with_its_occurrence() {
    use peri_acp_types::system_reminder::{
        ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
        ReminderSource, SYSTEM_REMINDER_VERSION, SystemReminder,
    };

    let _restore = RestoreBgAtoms::empty();
    let mut state = make_fold_test_state();
    let old_run = origin("shared-child", 51);

    dispatch_run(
        &mut state,
        &old_run,
        AcpEventData::SubagentStarted {
            agent_id: "shared-child".into(),
            agent_name: "coder".into(),
            is_background: true,
            parent_tool_call_id: None,
        },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::BgTaskStarted(task_entry("bg-old", "shared-child")),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::BgTaskStarted(task_entry("bg-new", "shared-child")),
    );
    let reminder = SystemReminder {
        version: SYSTEM_REMINDER_VERSION,
        category: ReminderCategory::Task,
        source: ReminderSource("child_task".into()),
        kind: "update".into(),
        severity: ReminderSeverity::Info,
        delivery: ReminderDelivery::Required,
        audiences: ReminderAudiences(vec![ReminderAudience::Tui]),
        body: "old run reminder".into(),
        summary: Some("child update".into()),
        metadata: serde_json::json!({}),
    };
    reminder.validate().unwrap();
    dispatch_run(
        &mut state,
        &old_run,
        AcpEventData::SystemReminder {
            reminder,
            source_agent_id: Some("shared-child".into()),
            replay: false,
        },
    );

    let live = BG_LIVE_DETAIL.state();
    let live = live.read();
    let has_reminder = |task: &str| {
        live[task]
            .nested_units
            .iter()
            .any(|unit| matches!(unit, TuiRenderUnit::TuiSystemReminder(_)))
    };
    assert!(has_reminder("bg-old"), "提醒必须归属旧 occurrence");
    assert!(!has_reminder("bg-new"), "提醒不得泄漏到新任务明细");
}

#[test]
#[serial]
fn lost_child_start_still_settles_its_registry_row() {
    let _restore = RestoreBgAtoms::empty();
    let mut state = make_fold_test_state();

    // Child Started 丢失（Lagged）但 registry 行存在：没有任何待绑定
    // occurrence，Stop 仍可按 registry 归属结算。生产 bridge 会在
    // ChildRunRouting 拒绝未知 run，此用例单独覆盖 dispatch 层语义。
    dispatch_and_notify(
        &mut state,
        &AcpEventData::BgTaskStarted(task_entry("bg-snapshot", "lost-child")),
    );
    let run = origin("lost-child", 61);
    dispatch_run(
        &mut state,
        &run,
        AcpEventData::SubagentStopped {
            agent_id: "lost-child".into(),
            result: "done".into(),
            is_error: false,
        },
    );
    assert_eq!(
        BG_LIVE_DETAIL.state().read()["bg-snapshot"].status,
        crate::kit::atoms::BgLiveStatus::Succeeded
    );
    assert!(
        !BG_TASKS
            .state()
            .read()
            .iter()
            .any(|task| task.task_id == "bg-snapshot")
    );
}

#[test]
#[serial]
fn ambiguous_snapshot_occurrences_never_bind_a_later_row() {
    let _restore = RestoreBgAtoms::empty();
    let mut state = make_fold_test_state();
    let mut old = origin("shared-child", 71);
    let new = origin("shared-child", 72);
    let started = || AcpEventData::SubagentStarted {
        agent_id: "shared-child".into(),
        agent_name: "coder".into(),
        is_background: true,
        parent_tool_call_id: None,
    };

    // 两个 child occurrence 都先于任何 registry 行；快照只有一行，无法证明
    // 哪个 occurrence 属于它。
    dispatch_run(&mut state, &old, started());
    dispatch_run(&mut state, &new, started());
    dispatch_and_notify(
        &mut state,
        &AcpEventData::BgTaskSnapshot {
            tasks: vec![task_entry("bg-current", "shared-child")],
            revision: Some(1),
            source: BgTaskSnapshotSource::Live,
        },
    );
    {
        let identities = BG_TASK_IDENTITY.state();
        let identities = identities.read();
        assert_eq!(identities["bg-current"].run_order, None);
        assert_eq!(
            identities["bg-current"].run_floor,
            Some(72),
            "歧义 occurrence 只能留下下界，不能选定 run"
        );
    }
    // 后续新行也不得取走任一条旧 occurrence。
    dispatch_and_notify(
        &mut state,
        &AcpEventData::BgTaskStarted(task_entry("bg-next", "shared-child")),
    );
    {
        let identities = BG_TASK_IDENTITY.state();
        let identities = identities.read();
        assert_eq!(identities["bg-next"].run_order, None);
        assert_eq!(identities["bg-next"].run_floor, Some(72));
    }
    // 延迟的旧 Stop 同样不得结算任一可见行。
    old.sequence += 1;
    dispatch_run(
        &mut state,
        &old,
        AcpEventData::SubagentStopped {
            agent_id: "shared-child".into(),
            result: "old done".into(),
            is_error: false,
        },
    );
    {
        let live = BG_LIVE_DETAIL.state();
        let live = live.read();
        assert_eq!(
            live["bg-current"].status,
            crate::kit::atoms::BgLiveStatus::Running
        );
        assert_eq!(
            live["bg-next"].status,
            crate::kit::atoms::BgLiveStatus::Running
        );
    }
    let tasks = BG_TASKS.state();
    let tasks = tasks.read();
    assert!(tasks.iter().any(|task| task.task_id == "bg-current"));
    assert!(tasks.iter().any(|task| task.task_id == "bg-next"));
}

fn task_entry_with_status(task_id: &str, child: &str, status: &str) -> BgTaskEntry {
    let mut entry = task_entry(task_id, child);
    entry.status = Some(status.into());
    entry
}

fn bg_started() -> AcpEventData {
    AcpEventData::SubagentStarted {
        agent_id: "shared-child".into(),
        agent_name: "coder".into(),
        is_background: true,
        parent_tool_call_id: None,
    }
}

fn bg_text(text: &str) -> AcpEventData {
    AcpEventData::TextChunk(TuiTextChunk {
        text: text.into(),
        message_id: None,
        agent_id: Some("shared-child".into()),
    })
}

fn bg_stopped(result: &str) -> AcpEventData {
    AcpEventData::SubagentStopped {
        agent_id: "shared-child".into(),
        result: result.into(),
        is_error: false,
    }
}

#[test]
#[serial]
fn terminal_snapshot_row_does_not_leave_old_occurrence_for_next_row() {
    let _restore = RestoreBgAtoms::empty();
    let mut state = make_fold_test_state();
    let mut old = origin("shared-child", 31);

    // 旧 child Started 先到；revision gap 时旧任务已终态，快照没有 active 行。
    dispatch_run(&mut state, &old, bg_started());
    dispatch_and_notify(
        &mut state,
        &AcpEventData::BgTaskSnapshot {
            tasks: vec![task_entry_with_status(
                "bg-old",
                "shared-child",
                "completed",
            )],
            revision: Some(1),
            source: BgTaskSnapshotSource::Live,
        },
    );
    assert!(
        state
            .child_runs
            .take_unbound_start("shared-child")
            .is_none(),
        "snapshot 已解释的旧 occurrence 不得留在队列"
    );
    // 后续 registry 行不得取走旧 31。
    dispatch_and_notify(
        &mut state,
        &AcpEventData::BgTaskStarted(task_entry("bg-new", "shared-child")),
    );
    {
        let identities = BG_TASK_IDENTITY.state();
        let identities = identities.read();
        assert_eq!(identities["bg-new"].run_order, None);
        assert_eq!(identities["bg-new"].run_floor, Some(31));
    }
    // 延迟旧 Stop 只能命中旧终态行，不能结算 bg-new。
    old.sequence += 1;
    dispatch_run(&mut state, &old, bg_stopped("old done"));
    {
        let live = BG_LIVE_DETAIL.state();
        let live = live.read();
        assert_eq!(
            live["bg-new"].status,
            crate::kit::atoms::BgLiveStatus::Running
        );
    }
    assert!(
        BG_TASKS
            .state()
            .read()
            .iter()
            .any(|task| task.task_id == "bg-new")
    );

    // 新 run 的 Started/文本/Stop 仍完整归属 bg-new。
    let mut new = origin("shared-child", 32);
    dispatch_run(&mut state, &new, bg_started());
    assert_eq!(
        BG_TASK_IDENTITY.state().read()["bg-new"].run_order,
        Some(32)
    );
    new.sequence += 1;
    dispatch_run(&mut state, &new, bg_text("new run output"));
    new.sequence += 1;
    dispatch_run(&mut state, &new, bg_stopped("new done"));
    {
        let live = BG_LIVE_DETAIL.state();
        let live = live.read();
        assert_eq!(
            live["bg-new"].status,
            crate::kit::atoms::BgLiveStatus::Succeeded
        );
        assert_eq!(bubbles(&live["bg-new"]), "new run output");
    }
    assert!(
        !BG_TASKS
            .state()
            .read()
            .iter()
            .any(|task| task.task_id == "bg-new")
    );
}

#[test]
#[serial]
fn terminal_and_active_snapshot_rows_do_not_bind_old_occurrence_to_new_row() {
    let _restore = RestoreBgAtoms::empty();
    let mut state = make_fold_test_state();
    let mut old = origin("shared-child", 31);

    dispatch_run(&mut state, &old, bg_started());
    // 快照同时包含旧终态行与复用同一 child 的新 active 行。
    dispatch_and_notify(
        &mut state,
        &AcpEventData::BgTaskSnapshot {
            tasks: vec![
                task_entry_with_status("bg-old", "shared-child", "completed"),
                task_entry("bg-new", "shared-child"),
            ],
            revision: Some(1),
            source: BgTaskSnapshotSource::Live,
        },
    );
    assert!(
        state
            .child_runs
            .take_unbound_start("shared-child")
            .is_none(),
        "snapshot 行已能解释的旧 occurrence 不得留在队列"
    );
    {
        let identities = BG_TASK_IDENTITY.state();
        let identities = identities.read();
        assert_eq!(
            identities["bg-new"].run_order, None,
            "旧 occurrence 不得绑定新行"
        );
        assert_eq!(identities["bg-new"].run_floor, Some(31));
    }
    // 旧 Stop 不得结算 bg-new。
    old.sequence += 1;
    dispatch_run(&mut state, &old, bg_stopped("old done"));
    {
        let live = BG_LIVE_DETAIL.state();
        let live = live.read();
        assert_eq!(
            live["bg-new"].status,
            crate::kit::atoms::BgLiveStatus::Running
        );
    }
    assert!(
        BG_TASKS
            .state()
            .read()
            .iter()
            .any(|task| task.task_id == "bg-new")
    );

    // 后续自己的 run 仍正常归属 bg-new。
    let mut new = origin("shared-child", 32);
    dispatch_run(&mut state, &new, bg_started());
    assert_eq!(
        BG_TASK_IDENTITY.state().read()["bg-new"].run_order,
        Some(32)
    );
    new.sequence += 1;
    dispatch_run(&mut state, &new, bg_text("new run output"));
    new.sequence += 1;
    dispatch_run(&mut state, &new, bg_stopped("new done"));
    {
        let live = BG_LIVE_DETAIL.state();
        let live = live.read();
        assert_eq!(
            live["bg-new"].status,
            crate::kit::atoms::BgLiveStatus::Succeeded
        );
        assert_eq!(bubbles(&live["bg-new"]), "new run output");
    }
}

#[test]
#[serial]
fn older_terminal_snapshot_row_does_not_block_new_run_before_registry_row() {
    let _restore = RestoreBgAtoms::empty();
    let mut state = make_fold_test_state();
    let old = origin("shared-child", 30);

    // 旧任务先绑定 run30 并完成；其终态仍会出现在后续 live snapshot 中。
    dispatch_run(&mut state, &old, bg_started());
    dispatch_and_notify(
        &mut state,
        &AcpEventData::BgTaskStarted(task_entry("bg-old", "shared-child")),
    );
    assert_eq!(
        BG_TASK_IDENTITY.state().read()["bg-old"].run_order,
        Some(30)
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::BgTaskCompleted {
            task_id: "bg-old".into(),
            kind: Some("agent".into()),
            success: true,
            duration_ms: 1,
            output_preview: None,
            revision: None,
        },
    );

    // 新 run31 的 child Started 先于它的 registry 行到达。
    let mut new = origin("shared-child", 31);
    dispatch_run(&mut state, &new, bg_started());
    assert_eq!(
        state.child_runs.pending_run_ceiling("shared-child"),
        Some(31)
    );

    // 快照含旧终态行与新 active 行；31 严格新于已知 run30，仍应绑定新行。
    dispatch_and_notify(
        &mut state,
        &AcpEventData::BgTaskSnapshot {
            tasks: vec![
                task_entry_with_status("bg-old", "shared-child", "completed"),
                task_entry("bg-new", "shared-child"),
            ],
            revision: Some(1),
            source: BgTaskSnapshotSource::Live,
        },
    );
    assert!(
        state
            .child_runs
            .take_unbound_start("shared-child")
            .is_none(),
        "可归属新行的 occurrence 不应留在队列"
    );
    assert_eq!(
        BG_TASK_IDENTITY.state().read()["bg-new"].run_order,
        Some(31)
    );

    new.sequence += 1;
    dispatch_run(&mut state, &new, bg_text("new run output"));
    new.sequence += 1;
    dispatch_run(&mut state, &new, bg_stopped("new done"));
    {
        let live = BG_LIVE_DETAIL.state();
        let live = live.read();
        assert_eq!(
            live["bg-new"].status,
            crate::kit::atoms::BgLiveStatus::Succeeded
        );
        assert_eq!(bubbles(&live["bg-new"]), "new run output");
    }
}
