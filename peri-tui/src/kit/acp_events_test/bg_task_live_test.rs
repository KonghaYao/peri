use super::*;
use crate::kit::acp_types::AcpEventData;
use crate::kit::atoms::{BG_DISPLAY, BG_LIVE_DETAIL, BG_TASK_REVISION, BgLiveStatus};
use crate::kit::stream_data::{TuiTextChunk, TuiToolEnded, TuiToolStarted};
use crate::kit::tui_render_unit::TuiRenderUnit;
use serial_test::serial;

struct RestoreSnapshotAtoms {
    tasks: Vec<crate::kit::acp_types::BgTaskEntry>,
    display: Vec<crate::kit::atoms::BgDisplayEntry>,
    live: std::collections::HashMap<String, crate::kit::atoms::BgLiveDetail>,
    identity: std::collections::HashMap<String, crate::kit::atoms::BgTaskIdentity>,
    agent_ids: std::collections::HashSet<String>,
    revision: Option<u64>,
}

impl RestoreSnapshotAtoms {
    fn empty() -> Self {
        crate::kit::atoms::init_atoms();
        let saved = Self {
            tasks: crate::kit::atoms::BG_TASKS.state().read().clone(),
            display: BG_DISPLAY.state().read().clone(),
            live: BG_LIVE_DETAIL.state().read().clone(),
            identity: crate::kit::atoms::BG_TASK_IDENTITY.state().read().clone(),
            agent_ids: crate::kit::atoms::BG_AGENT_IDS.state().read().clone(),
            revision: *BG_TASK_REVISION.state().read(),
        };
        crate::kit::atoms::BG_TASKS.state().write().clear();
        BG_DISPLAY.state().write().clear();
        BG_LIVE_DETAIL.state().write().clear();
        crate::kit::atoms::BG_TASK_IDENTITY.state().write().clear();
        crate::kit::atoms::BG_AGENT_IDS.state().write().clear();
        BG_TASK_REVISION.set(None);
        saved
    }
}

impl Drop for RestoreSnapshotAtoms {
    fn drop(&mut self) {
        crate::kit::atoms::BG_TASKS.set(self.tasks.clone());
        BG_DISPLAY.set(self.display.clone());
        BG_LIVE_DETAIL.set(self.live.clone());
        crate::kit::atoms::BG_TASK_IDENTITY.set(self.identity.clone());
        crate::kit::atoms::BG_AGENT_IDS.set(self.agent_ids.clone());
        BG_TASK_REVISION.set(self.revision);
    }
}

#[test]
#[serial]
fn background_started_does_not_restore_parent_loading() {
    let _restore = RestoreSnapshotAtoms::empty();
    let mut state = make_fold_test_state();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "background-observed".into(),
            agent_name: "worker".into(),
            is_background: true,
            parent_tool_call_id: None,
        },
    );
    assert_eq!(state.phase, SessionPhase::Idle);
    assert!(!ACP_STATE.state().read().is_loading);
    dispatch_and_notify(
        &mut state,
        &AcpEventData::PromptSubmitted {
            request_id: Some("live-parent".into()),
        },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "second-background-observed".into(),
            agent_name: "worker".into(),
            is_background: true,
            parent_tool_call_id: None,
        },
    );
    assert_eq!(state.phase, SessionPhase::PromptRunning);
    assert!(ACP_STATE.state().read().is_loading);
}

#[test]
#[serial]
fn live_lost_update_revokes_activity_without_snapshot_resurrection() {
    let _restore = RestoreSnapshotAtoms::empty();
    let mut state = make_fold_test_state();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::BgTaskStarted(crate::kit::acp_types::BgTaskEntry {
            task_id: "lost-worker".into(),
            kind: "agent".into(),
            summary: "current worker".into(),
            started_at: String::new(),
            pid: None,
            revision: Some(1),
            status: Some("running".into()),
        }),
    );
    assert!(BG_DISPLAY.state().read()[0].is_active);
    super::super::system::handle_bg_task_updated("lost-worker", "lost", Some(2));
    assert!(BG_DISPLAY.state().read().is_empty());
    assert_eq!(
        BG_LIVE_DETAIL.state().read()["lost-worker"].status,
        BgLiveStatus::Unobserved
    );
    super::super::system::handle_bg_task_updated("lost-worker", "running", Some(3));
    assert!(BG_DISPLAY.state().read().is_empty());
    assert_eq!(
        BG_LIVE_DETAIL.state().read()["lost-worker"].status,
        BgLiveStatus::Unobserved
    );
}

#[test]
#[serial]
fn historical_nonterminal_snapshots_do_not_create_live_activity() {
    let _restore = RestoreSnapshotAtoms::empty();
    for (revision, status) in [
        Some("running"),
        Some("lost"),
        Some("reconciling"),
        Some("delivery_pending"),
        Some("unknown"),
        None,
    ]
    .into_iter()
    .enumerate()
    {
        let task = crate::kit::acp_types::BgTaskEntry {
            task_id: "historical-agent".into(),
            kind: "agent".into(),
            summary: "saved task".into(),
            started_at: "2026-10-01T00:00:00Z".into(),
            pid: Some(123),
            revision: None,
            status: status.map(str::to_string),
        };
        assert!(super::super::system::apply_bg_task_snapshot(
            &[task.clone()],
            Some(revision as u64),
        ));
        assert!(BG_DISPLAY.state().read().is_empty());
        assert_eq!(
            BG_LIVE_DETAIL.state().read()[&task.task_id].status,
            BgLiveStatus::Unobserved
        );
        assert_eq!(
            crate::kit::atoms::BG_TASKS.state().read()[0].status,
            task.status
        );
    }
}

#[test]
#[serial]
fn live_started_event_remains_active_but_snapshot_cannot_resurrect_it() {
    let _restore = RestoreSnapshotAtoms::empty();
    let mut state = make_fold_test_state();
    let mut task = crate::kit::acp_types::BgTaskEntry {
        task_id: "observed-shell".into(),
        kind: "shell".into(),
        summary: "sleep 1".into(),
        started_at: String::new(),
        pid: None,
        revision: Some(1),
        status: Some("running".into()),
    };
    assert!(super::super::system::apply_bg_task_snapshot(
        &[task.clone()],
        Some(1)
    ));
    assert!(BG_DISPLAY.state().read().is_empty());
    task.revision = Some(2);
    dispatch_and_notify(&mut state, &AcpEventData::BgTaskStarted(task.clone()));
    let started_at = BG_DISPLAY.state().read()[0].created_at;
    BG_LIVE_DETAIL
        .state()
        .write()
        .get_mut(&task.task_id)
        .unwrap()
        .tool_cards
        .push(crate::kit::acp_types::ToolCardAccumulator::with_input(
            "unconfirmed-read".into(),
            "Read".into(),
            "file".into(),
            serde_json::json!({"file_path": "file"}),
            None,
        ));
    assert!(super::super::system::apply_bg_task_snapshot(
        &[task.clone()],
        Some(3)
    ));
    assert!(BG_DISPLAY.state().read()[0].is_active);
    assert_eq!(BG_DISPLAY.state().read()[0].created_at, started_at);
    assert_eq!(
        BG_LIVE_DETAIL.state().read()[&task.task_id].status,
        BgLiveStatus::Running
    );
    task.status = Some("lost".into());
    assert!(super::super::system::apply_bg_task_snapshot(
        &[task.clone()],
        Some(4)
    ));
    assert!(BG_DISPLAY.state().read().is_empty());
    assert_eq!(
        BG_LIVE_DETAIL.state().read()[&task.task_id].status,
        BgLiveStatus::Unobserved
    );
    let live = BG_LIVE_DETAIL.state().read().clone();
    let TuiRenderUnit::TuiToolCard(card) = &live[&task.task_id].nested_units[0] else {
        panic!("expected an unconfirmed tool record");
    };
    assert!(!card.is_running);
    assert!(card.is_error);
    assert_eq!(
        card.output_summary,
        crate::i18n::tr("shell-detail-status-unobserved")
    );
    task.status = Some("running".into());
    assert!(super::super::system::apply_bg_task_snapshot(
        &[task],
        Some(5)
    ));
    assert!(BG_DISPLAY.state().read().is_empty());
}

#[test]
#[serial]
fn bg_tool_duplicate_start_upgrades_input_without_restarting() {
    crate::kit::atoms::init_atoms();
    struct RestoreAtoms {
        display: Vec<crate::kit::atoms::BgDisplayEntry>,
        live: std::collections::HashMap<String, crate::kit::atoms::BgLiveDetail>,
    }
    impl Drop for RestoreAtoms {
        fn drop(&mut self) {
            *BG_DISPLAY.state().write() = self.display.clone();
            *BG_LIVE_DETAIL.state().write() = self.live.clone();
        }
    }
    let _restore = RestoreAtoms {
        display: BG_DISPLAY.state().read().clone(),
        live: BG_LIVE_DETAIL.state().read().clone(),
    };
    BG_DISPLAY
        .state()
        .write()
        .push(crate::kit::atoms::BgDisplayEntry {
            id: "task-upgrade".into(),
            linked_agent_id: Some("child-upgrade".into()),
            agent_type: "agent".into(),
            desc: "coder".into(),
            current_tool: None,
            tool_count: 0,
            is_active: true,
            is_error: false,
            created_at: std::time::Instant::now(),
            completed_at: None,
        });
    crate::kit::bg_task_live::init_agent_live_detail(
        "task-upgrade",
        "child-upgrade",
        "coder",
        None,
    );
    let mut started = TuiToolStarted {
        tool_id: "call-upgrade".into(),
        tool_name: "Shell".into(),
        input_summary: "null".into(),
        raw_input: serde_json::Value::Null,
        agent_id: Some("child-upgrade".into()),
    };
    crate::kit::bg_task_live::handle_bg_tool_started("child-upgrade", &started, None);
    let started_at = BG_LIVE_DETAIL.state().read()["task-upgrade"].tool_cards[0].started_at;
    started.raw_input = serde_json::json!({"command": "pwd"});
    started.input_summary = "pwd".into();
    crate::kit::bg_task_live::handle_bg_tool_started("child-upgrade", &started, None);
    crate::kit::bg_task_live::handle_bg_tool_ended(
        "child-upgrade",
        &TuiToolEnded {
            tool_id: started.tool_id.clone(),
            output_summary: "workspace".into(),
            is_error: false,
            agent_id: started.agent_id.clone(),
        },
    );
    crate::kit::bg_task_live::handle_bg_tool_started("child-upgrade", &started, None);
    let live = BG_LIVE_DETAIL.state();
    let guard = live.read();
    let detail = &guard["task-upgrade"];
    assert_eq!(detail.tool_cards.len(), 1);
    assert_eq!(detail.tool_cards[0].started_at, started_at);
    let TuiRenderUnit::TuiToolCard(card) = &detail.nested_units[0] else {
        panic!("expected tool");
    };
    assert_eq!(card.input_summary, "pwd");
    assert!(!card.is_running);
    assert_eq!(card.output_summary, "workspace");
}

#[test]
#[serial]
fn terminal_snapshot_does_not_resurrect_a_running_task() {
    crate::kit::atoms::init_atoms();
    BG_TASK_REVISION.set(None);
    BG_DISPLAY.state().write().clear();
    BG_LIVE_DETAIL.state().write().clear();
    assert!(super::super::system::apply_bg_task_snapshot(
        &[crate::kit::acp_types::BgTaskEntry {
            task_id: "finished-shell".into(),
            kind: "shell".into(),
            summary: "sleep 1".into(),
            started_at: "2026-10-03T00:00:00Z".into(),
            pid: None,
            revision: None,
            status: Some("completed".into()),
        }],
        Some(42),
    ));
    let entries = BG_DISPLAY.state();
    let entries = entries.read();
    assert_eq!(entries.len(), 1);
    assert!(!entries[0].is_active);
    assert!(entries[0].completed_at.is_some());
    drop(entries);
    assert_eq!(
        BG_LIVE_DETAIL.state().read()["finished-shell"].status,
        BgLiveStatus::Succeeded
    );
    crate::kit::bg_task_live::mark_task_completed(
        "finished-shell",
        true,
        123,
        Some("saved output".into()),
    );
    assert!(super::super::system::apply_bg_task_snapshot(
        &[crate::kit::acp_types::BgTaskEntry {
            task_id: "finished-shell".into(),
            kind: "shell".into(),
            summary: "sleep 1".into(),
            started_at: "2026-10-03T00:00:00Z".into(),
            pid: None,
            revision: None,
            status: Some("completed".into()),
        }],
        Some(43),
    ));
    let live = BG_LIVE_DETAIL.state();
    let details = live.read();
    let detail = &details["finished-shell"];
    assert_eq!(detail.status, BgLiveStatus::Succeeded);
    assert_eq!(detail.duration_ms, Some(123));
    assert_eq!(detail.output_preview.as_deref(), Some("saved output"));
    BG_TASK_REVISION.set(None);
}

#[test]
#[serial]
fn test_mcp_shell_task_events_update_bottom_task_area() {
    crate::kit::atoms::init_atoms();
    BG_DISPLAY.state().write().clear();
    let mut state = make_state();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::BgTaskStarted(crate::kit::acp_types::BgTaskEntry {
            task_id: "shell-mcp".into(),
            kind: "shell".into(),
            summary: "sleep 1".into(),
            started_at: "2026-10-03T00:00:00Z".into(),
            pid: None,
            revision: None,
            status: None,
        }),
    );
    let entries = BG_DISPLAY.state();
    let running = entries.read();
    assert_eq!(running.len(), 1);
    assert_eq!(running[0].agent_type, "shell");
    assert!(running[0].is_active);
    drop(running);

    dispatch_and_notify(
        &mut state,
        &AcpEventData::BgTaskCompleted {
            task_id: "shell-mcp".into(),
            kind: Some("shell".into()),
            success: true,
            duration_ms: 1000,
            output_preview: None,
            revision: None,
        },
    );
    let done = entries.read();
    assert!(!done[0].is_active);
    assert!(!done[0].is_error);
}

#[test]
#[serial]
fn snapshot_watermark_rejects_late_started_event() {
    crate::kit::atoms::init_atoms();
    crate::kit::atoms::BG_TASK_REVISION.set(None);
    crate::kit::atoms::BG_TASKS.state().write().clear();
    BG_DISPLAY.state().write().clear();
    let mut state = make_state();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::BgTaskSnapshot {
            revision: Some(3),
            tasks: vec![],
        },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::BgTaskStarted(crate::kit::acp_types::BgTaskEntry {
            task_id: "already-finished".into(),
            kind: "shell".into(),
            summary: "echo done".into(),
            started_at: String::new(),
            pid: None,
            revision: Some(2),
            status: None,
        }),
    );
    assert!(crate::kit::atoms::BG_TASKS.state().read().is_empty());
    assert!(BG_DISPLAY.state().read().is_empty());
    assert_eq!(*crate::kit::atoms::BG_TASK_REVISION.state().read(), Some(3));
    crate::kit::atoms::BG_TASK_REVISION.set(None);
}

#[test]
#[serial]
fn test_bg_task_cancelled_persists_reason_on_live_detail() {
    crate::kit::atoms::init_atoms();
    BG_LIVE_DETAIL.state().write().clear();
    dispatch_and_notify(
        &mut make_state(),
        &AcpEventData::BgTaskStarted(crate::kit::acp_types::BgTaskEntry {
            task_id: "task-shell".into(),
            kind: "shell".into(),
            summary: "echo".into(),
            started_at: String::new(),
            pid: None,
            revision: None,
            status: None,
        }),
    );
    dispatch_and_notify(
        &mut make_state(),
        &AcpEventData::BgTaskCancelled {
            task_id: "task-shell".into(),
            reason: "user cancelled".into(),
            revision: None,
        },
    );
    let live_store = BG_LIVE_DETAIL.state();
    let live_guard = live_store.read();
    let detail = live_guard.get("task-shell").expect("live detail");
    assert_eq!(detail.cancel_reason.as_deref(), Some("user cancelled"));
}

#[test]
#[serial]
fn test_bg_text_chunk_appends_live_detail_not_view_models() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    BG_DISPLAY.state().write().clear();
    BG_LIVE_DETAIL.state().write().clear();
    let mut state = make_state();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::BgTaskStarted(crate::kit::acp_types::BgTaskEntry {
            task_id: "task-1".into(),
            kind: "agent".into(),
            summary: "bg".into(),
            started_at: String::new(),
            pid: None,
            revision: None,
            status: None,
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "bg-agent".into(),
            agent_name: "coder".into(),
            is_background: true,
            parent_tool_call_id: None,
        },
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnSuspended);
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "live line".into(),
            message_id: None,
            agent_id: Some("bg-agent".into()),
        }),
    );
    let live_store = BG_LIVE_DETAIL.state();
    let live_guard = live_store.read();
    let detail = live_guard.get("task-1").expect("projection");
    assert!(
        !detail.nested_units.is_empty(),
        "bg chunk should append to BG_LIVE_DETAIL"
    );
    assert!(state.current_turn.text.is_empty());
}

#[test]
#[serial]
fn test_bg_tool_sync_preserves_event_order() {
    crate::kit::atoms::init_atoms();
    BG_LIVE_DETAIL.state().write().clear();
    BG_DISPLAY
        .state()
        .write()
        .push(crate::kit::atoms::BgDisplayEntry {
            id: "task-order".into(),
            linked_agent_id: Some("bg-order".into()),
            agent_type: "agent".into(),
            desc: "bg".into(),
            current_tool: None,
            tool_count: 0,
            is_active: true,
            is_error: false,
            created_at: std::time::Instant::now(),
            completed_at: None,
        });
    crate::kit::bg_task_live::seed_live_from_started("task-order", "agent", "bg", None);
    crate::kit::bg_task_live::init_agent_live_detail("task-order", "bg-order", "coder", None);

    crate::kit::bg_task_live::handle_bg_tool_started(
        "bg-order",
        &TuiToolStarted {
            tool_id: "tool-1".into(),
            tool_name: "Bash".into(),
            input_summary: "echo hi".into(),
            raw_input: serde_json::Value::Null,
            agent_id: Some("bg-order".into()),
        },
        None,
    );
    crate::kit::bg_task_live::append_bg_text_chunk(
        "bg-order",
        &TuiTextChunk {
            text: "after".into(),
            message_id: Some("m1".into()),
            agent_id: Some("bg-order".into()),
        },
    );
    crate::kit::bg_task_live::handle_bg_tool_ended(
        "bg-order",
        &TuiToolEnded {
            tool_id: "tool-1".into(),
            output_summary: "done".into(),
            is_error: false,
            agent_id: Some("bg-order".into()),
        },
    );

    let live = BG_LIVE_DETAIL.state();
    let guard = live.read();
    let units = &guard.get("task-order").expect("detail").nested_units;
    assert!(matches!(units.get(0), Some(TuiRenderUnit::TuiToolCard(_))));
    assert!(matches!(
        units.get(1),
        Some(TuiRenderUnit::TuiAssistantBubble(_))
    ));
}

/// [回归] bg subagent 跨 turn 边界后，VIEW_MODELS 中残留的组被冻结、内容只进
/// `BG_LIVE_DETAIL`——这是 `SubAgentDetailPanel` 必须以 live detail 为准的原因
/// （面板侧的解析见 `panels/subagent_detail_test.rs`）。
///
/// TurnSuspended/TurnInterrupted 归档 current_turn 时不走 flush 的
/// running-subagent 守卫，仍然运行的 bg 组会以「下线那一刻的内容 + is_running=true」
/// 落进 committed 并清空 accumulator；此后该组的子事件路由失败，只有 bg 兜底路径
/// （BG_LIVE_DETAIL）继续累积。面板若按 VIEW_MODELS 扫描结果渲染，就永久停在冻结
/// 内容上。
#[test]
#[serial]
fn test_bg_group_frozen_in_view_models_after_turn_suspended() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    BG_DISPLAY.state().write().clear();
    BG_LIVE_DETAIL.state().write().clear();
    crate::kit::atoms::BG_AGENT_IDS.state().write().clear();
    let mut state = make_state();
    dispatch_and_notify(
        &mut state,
        &AcpEventData::BgTaskStarted(crate::kit::acp_types::BgTaskEntry {
            task_id: "task-1".into(),
            kind: "agent".into(),
            summary: "bg".into(),
            started_at: String::new(),
            pid: None,
            revision: None,
            status: None,
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "bg-agent".into(),
            agent_name: "coder".into(),
            is_background: true,
            parent_tool_call_id: None,
        },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "first".into(),
            message_id: None,
            agent_id: Some("bg-agent".into()),
        }),
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnSuspended);
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "+second".into(),
            message_id: None,
            agent_id: Some("bg-agent".into()),
        }),
    );

    let group = {
        let snap = VIEW_MODELS.state();
        let snapshot = snap.read();
        snapshot
            .items
            .iter()
            .find_map(|item| match item {
                TuiRenderUnit::TuiSubAgentGroup(g) if g.agent_id == "bg-agent" => Some(g.clone()),
                _ => None,
            })
            .expect("归档后 VIEW_MODELS 仍留有 bg 组")
    };
    let group_texts: Vec<String> = group
        .view_models
        .iter()
        .filter_map(|vm| match vm {
            TuiRenderUnit::TuiAssistantBubble(b) => Some(b.text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        group_texts,
        vec!["first".to_string()],
        "VIEW_MODELS 组应停在 turn 边界那一刻（冻结）"
    );

    let live_store = BG_LIVE_DETAIL.state();
    let live_guard = live_store.read();
    let detail = live_guard.get("task-1").expect("live detail");
    let live_texts: Vec<String> = detail
        .nested_units
        .iter()
        .filter_map(|vm| match vm {
            TuiRenderUnit::TuiAssistantBubble(b) => Some(b.text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        live_texts,
        vec!["first+second".to_string()],
        "跨边界后的 bg 内容只累积在 BG_LIVE_DETAIL"
    );
    // 面板靠这层对应关系把「选中的组」对到「哪一次运行」：agent_id 在 resume 时
    // 被复用，只有 instance_id 能区分。
    assert_eq!(
        detail.subagent_instance_id.as_deref(),
        Some(group.instance_id.as_str()),
        "live 明细记录的 occurrence 必须与归档组一致"
    );
}

fn make_state() -> BridgeState {
    BridgeState {
        variant: 0,
        committed: im::Vector::new(),
        current_turn: CurrentTurn::new(),
        phase: SessionPhase::Idle,
        popup_kind: None,
        generation: 0,
        active_session_id: String::new(),
        compact_just_completed: false,
        last_submitted_text: None,
        last_pushed_text_len: 0,
        last_pushed_reasoning_len: 0,
        last_successful_todos: None,
        last_successful_todo_sequence: None,
        next_todo_sequence: 0,
        todo_call_inputs: std::collections::HashMap::new(),
        turn_generation: 0,
        last_prompt_generation: 0,
        current_request_id: None,
        pending_cache_usage: None,
        publication_intent: Default::default(),
        folded_history: Default::default(),
    }
}
