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
    let old_run = origin("shared-child", 31);
    let new_run = origin("shared-child", 32);

    dispatch_and_notify(
        &mut state,
        &AcpEventData::BgTaskStarted(task_entry("bg-old", "shared-child")),
    );
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
    // 新 registry Started 先把新任务行绑定到复用的 child id。
    dispatch_and_notify(
        &mut state,
        &AcpEventData::BgTaskStarted(task_entry("bg-new", "shared-child")),
    );
    // 旧 Stop 仍按旧 execution origin 到达。
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
    dispatch_run(
        &mut state,
        &new_run,
        AcpEventData::TextChunk(TuiTextChunk {
            text: "new run output".into(),
            message_id: None,
            agent_id: Some("shared-child".into()),
        }),
    );
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
            live["bg-old"].status,
            crate::kit::atoms::BgLiveStatus::Succeeded
        );
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
