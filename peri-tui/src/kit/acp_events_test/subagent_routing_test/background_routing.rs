//! Background routing regression scenarios.

use super::AcpEventData;
use super::BridgeState;
use super::SessionPhase;
use super::TuiRenderUnit;

/// [回归测试 M2] 固定后台 child 长期 running 时，父会话连续 TurnDone 必须
/// 整段归档；CurrentTurn 的 raw/VM/segments/tool_cards 不随轮数增长。
#[test]
#[serial]
fn test_turn_done_with_only_background_child_keeps_current_turn_bounded() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    crate::kit::atoms::BG_AGENT_IDS.state().write().clear();
    let mut state = super::make_state(SessionPhase::PromptRunning, 0);

    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "bg-long".into(),
            agent_name: "bg-long".into(),
            is_background: true,
            parent_tool_call_id: None,
        },
    );
    assert!(state.current_turn.has_running_subagent());
    assert!(!state.current_turn.has_running_sync_subagent());

    for round in 0..1000 {
        super::dispatch_and_notify(
            &mut state,
            &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
                text: format!("parent round {round}"),
                message_id: Some(format!("parent-{round}")),
                agent_id: None,
            }),
        );
        super::dispatch_and_notify(
            &mut state,
            &AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
                agent_id: None,
                tool_name: "Read".into(),
                tool_id: format!("tool-{round}"),
                input_summary: "file.rs".into(),
                raw_input: serde_json::Value::Null,
                origin_message_id: None,
                origin_readable: None,
            }),
        );
        super::dispatch_and_notify(
            &mut state,
            &AcpEventData::ToolEnded(crate::kit::stream_data::TuiToolEnded {
                images: Vec::new(),
                agent_id: None,
                tool_id: format!("tool-{round}"),
                output_summary: "ok".into(),
                is_error: false,
            }),
        );
        super::dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

        assert!(
            state.current_turn.is_empty(),
            "round {round}: TurnDone 后 CurrentTurn 必须为空"
        );
        assert_eq!(
            state.current_turn.view_model_count(),
            0,
            "round {round}: CurrentTurn VM 不得随轮数增长"
        );
        assert_eq!(
            state.current_turn.raw_memory_bytes().body,
            0,
            "round {round}: CurrentTurn raw 正文不得随轮数增长"
        );
        assert!(
            state.current_turn.tool_cards.is_empty(),
            "round {round}: 工具卡不得滞留"
        );
    }
    crate::kit::atoms::BG_AGENT_IDS.state().write().clear();
}

/// [回归测试 M2] 后台 A 长期 running、同步 B 已完成时：
/// - 用户气泡边界与普通 TurnDone 都整段归档父消息，展示顺序不变；
/// - 迟到的 A 文本/Stop 只进 BG_LIVE_DETAIL，不进入主回复。
#[test]
#[serial]
fn test_turn_boundary_archives_parent_and_routes_late_background_events() {
    use crate::kit::atoms::{BG_DISPLAY, BG_LIVE_DETAIL, BG_TASK_IDENTITY, BG_TASKS};
    use crate::kit::stream_data::TuiTextChunk;

    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    BG_TASKS.state().write().clear();
    BG_DISPLAY.state().write().clear();
    BG_LIVE_DETAIL.state().write().clear();
    BG_TASK_IDENTITY.state().write().clear();
    crate::kit::atoms::BG_AGENT_IDS.state().write().clear();
    crate::kit::atoms::BG_TASK_REVISION.set(None);
    let mut state = super::make_state(SessionPhase::PromptRunning, 0);

    let root_text = |text: &str, message_id: &str| {
        AcpEventData::TextChunk(TuiTextChunk {
            text: text.into(),
            message_id: Some(message_id.into()),
            agent_id: None,
        })
    };
    let child_text = |agent_id: &str, text: &str, message_id: &str| {
        AcpEventData::TextChunk(TuiTextChunk {
            text: text.into(),
            message_id: Some(message_id.into()),
            agent_id: Some(agent_id.into()),
        })
    };

    // 后台 A 的任务身份：迟到事件需要能落进 BG_LIVE_DETAIL。
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::BgTaskStarted(crate::kit::acp_types::BgTaskEntry {
            task_id: "task-a".into(),
            kind: "agent".into(),
            summary: "bg".into(),
            started_at: String::new(),
            pid: None,
            revision: None,
            status: None,
            child_thread_id: Some("bg-a".into()),
        }),
    );
    super::dispatch_and_notify(&mut state, &root_text("p0", "m0"));
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "bg-a".into(),
            agent_name: "bg-a".into(),
            is_background: true,
            parent_tool_call_id: None,
        },
    );
    super::dispatch_and_notify(&mut state, &child_text("bg-a", "a-live", "bg-live"));

    // 用户气泡边界：A 仍在运行也要把前面的父正文与 A 一起归档。
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble {
            text: "u1".into(),
            attachments: Vec::new(),
        },
    );
    assert!(
        state.current_turn.is_empty(),
        "用户气泡边界必须整段归档，后台 A 不得保留容器"
    );

    // 同步 B 在本轮内启动并完成；父正文仍排在 B 之后。
    super::dispatch_and_notify(&mut state, &root_text("p1", "m1"));
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "sync-b".into(),
            agent_name: "sync-b".into(),
            is_background: false,
            parent_tool_call_id: None,
        },
    );
    super::dispatch_and_notify(&mut state, &child_text("sync-b", "b-done", "b-msg"));
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStopped {
            agent_id: "sync-b".into(),
            result: String::new(),
            is_error: false,
        },
    );
    super::dispatch_and_notify(&mut state, &root_text("p1-after", "m1b"));
    super::dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    super::dispatch_and_notify(&mut state, &root_text("p2", "m2"));
    super::dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    assert!(
        state.current_turn.is_empty(),
        "多轮 TurnDone 后 CurrentTurn 必须保持为空"
    );

    let layout: Vec<String> = state
        .committed
        .iter()
        .map(|unit| match unit {
            TuiRenderUnit::TuiAssistantBubble(bubble) => format!("text:{}", bubble.text),
            TuiRenderUnit::TuiUserBubble(bubble) => format!("user:{}", bubble.text),
            TuiRenderUnit::TuiSubAgentGroup(group) => format!("group:{}", group.agent_id),
            other => format!("other:{other:?}"),
        })
        .collect();
    assert_eq!(
        layout,
        vec![
            "text:p0",
            "group:bg-a",
            "user:u1",
            "text:p1",
            "group:sync-b",
            "text:p1-after",
            "text:p2",
        ],
        "整段归档后父消息与子分组顺序不变"
    );

    let committed_len = state.committed.len();
    fn group_texts(state: &BridgeState, agent_id: &str) -> Vec<String> {
        state
            .committed
            .iter()
            .find_map(|unit| match unit {
                TuiRenderUnit::TuiSubAgentGroup(group) if group.agent_id == agent_id => Some(
                    group
                        .view_models
                        .iter()
                        .filter_map(|vm| match vm {
                            TuiRenderUnit::TuiAssistantBubble(bubble) => Some(bubble.text.clone()),
                            _ => None,
                        })
                        .collect(),
                ),
                _ => None,
            })
            .unwrap_or_default()
    }
    assert_eq!(
        group_texts(&state, "bg-a"),
        vec!["a-live".to_string()],
        "归档组冻结在 turn 边界那一刻"
    );

    // 迟到的 A 文本：只累积到 BG_LIVE_DETAIL，committed/current_turn 都不变。
    super::dispatch_and_notify(&mut state, &child_text("bg-a", "a-late", "bg-live"));
    assert_eq!(
        state.committed.len(),
        committed_len,
        "迟到后台文本不得进入主回复"
    );
    assert!(state.current_turn.is_empty());
    assert_eq!(group_texts(&state, "bg-a"), vec!["a-live".to_string()]);

    // 迟到的 A Stop：只结算后台预览，不改主回复。
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStopped {
            agent_id: "bg-a".into(),
            result: "done".into(),
            is_error: false,
        },
    );
    assert_eq!(
        state.committed.len(),
        committed_len,
        "迟到后台 Stop 不得进入主回复"
    );
    assert!(state.current_turn.is_empty());

    let live_store = BG_LIVE_DETAIL.state();
    let live = live_store.read();
    let detail = live.get("task-a").expect("bg live detail");
    let live_texts: Vec<String> = detail
        .nested_units
        .iter()
        .filter_map(|vm| match vm {
            TuiRenderUnit::TuiAssistantBubble(bubble) => Some(bubble.text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(live_texts, vec!["a-livea-late".to_string()]);
    assert_eq!(detail.status, crate::kit::atoms::BgLiveStatus::Succeeded);
    drop(live);

    BG_TASKS.state().write().clear();
    BG_DISPLAY.state().write().clear();
    BG_LIVE_DETAIL.state().write().clear();
    BG_TASK_IDENTITY.state().write().clear();
    crate::kit::atoms::BG_AGENT_IDS.state().write().clear();
}

/// [回归 M2 / CodeRabbit] `SubagentStarted` 先于 `BgTaskStarted`：任务行缺失期间
/// 的 child 预览由占位 owner 承接，行到达后绑定并接管后续文本/终态；turn reset
/// 不得吞掉迟到内容，父 `CurrentTurn` 仍保持为空。
#[test]
#[serial]
fn test_background_child_started_before_task_row_keeps_late_preview() {
    use crate::kit::atoms::{BG_DISPLAY, BG_LIVE_DETAIL, BG_TASK_IDENTITY, BG_TASKS};
    use crate::kit::stream_data::TuiTextChunk;

    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    BG_TASKS.state().write().clear();
    BG_DISPLAY.state().write().clear();
    BG_LIVE_DETAIL.state().write().clear();
    BG_TASK_IDENTITY.state().write().clear();
    crate::kit::atoms::BG_AGENT_IDS.state().write().clear();
    crate::kit::atoms::BG_TASK_REVISION.set(None);
    let mut state = super::make_state(SessionPhase::PromptRunning, 0);
    let child_text = |text: &str, message_id: &str| {
        AcpEventData::TextChunk(TuiTextChunk {
            text: text.into(),
            message_id: Some(message_id.into()),
            agent_id: Some("bg-early".into()),
        })
    };

    // 1) 后台 child 先启动；此时还没有任何任务行。
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "bg-early".into(),
            agent_name: "coder".into(),
            is_background: true,
            parent_tool_call_id: None,
        },
    );

    // 2) 父 turn 在任务行到达前结束：父正文归档，CurrentTurn 清空。
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "parent".into(),
            message_id: Some("parent-msg".into()),
            agent_id: None,
        }),
    );
    super::dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    assert!(
        state.current_turn.is_empty(),
        "turn 边界必须清空父 CurrentTurn（后台 child 不阻塞归档）"
    );

    // 3) 任务行之前到达的 child 预览不能丢。
    super::dispatch_and_notify(&mut state, &child_text("early", "child-msg"));

    // 4) 任务行迟到：占位 owner 绑定到真实 task 并接管预览。
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::BgTaskStarted(crate::kit::acp_types::BgTaskEntry {
            task_id: "task-late".into(),
            kind: "agent".into(),
            summary: "worker".into(),
            started_at: String::new(),
            pid: None,
            revision: None,
            status: Some("running".into()),
            child_thread_id: Some("bg-early".into()),
        }),
    );

    // 5) 绑定后的迟到文本与终态。
    super::dispatch_and_notify(&mut state, &child_text("-late", "child-msg"));
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStopped {
            agent_id: "bg-early".into(),
            result: "done".into(),
            is_error: false,
        },
    );

    assert!(
        state.current_turn.is_empty(),
        "迟到 child 事件不得重新占用父 CurrentTurn"
    );
    assert!(
        !state.committed.iter().any(|unit| matches!(
            unit,
            TuiRenderUnit::TuiAssistantBubble(bubble) if bubble.text.contains("early")
        )),
        "child 正文不得进入主回复"
    );

    let live_store = BG_LIVE_DETAIL.state();
    let live = live_store.read();
    assert!(
        !live.contains_key("bg-early"),
        "绑定后占位 owner 必须移交给真实 task"
    );
    let detail = live.get("task-late").expect("bound live detail");
    let live_texts: Vec<String> = detail
        .nested_units
        .iter()
        .filter_map(|vm| match vm {
            TuiRenderUnit::TuiAssistantBubble(bubble) => Some(bubble.text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        live_texts,
        vec!["early-late".to_string()],
        "任务行到达前后的 child 预览都要保留"
    );
    assert_eq!(detail.status, crate::kit::atoms::BgLiveStatus::Succeeded);
    assert_eq!(detail.subagent_result.as_deref(), Some("done"));
    drop(live);

    let display = BG_DISPLAY.state();
    let task = display
        .read()
        .iter()
        .find(|row| row.id == "task-late")
        .cloned()
        .expect("task row");
    assert_eq!(
        task.linked_agent_id.as_deref(),
        Some("bg-early"),
        "迟到的任务行必须认领对应 child"
    );

    BG_TASKS.state().write().clear();
    BG_DISPLAY.state().write().clear();
    BG_LIVE_DETAIL.state().write().clear();
    BG_TASK_IDENTITY.state().write().clear();
    crate::kit::atoms::BG_AGENT_IDS.state().write().clear();
}

/// [回归 / CodeRabbit Major] 任务行乱序到达时按 child_thread_id 精确配对，
/// 不能按占位创建顺序把 A 的预览挂到 B 的行上。
#[test]
#[serial]
fn test_reordered_task_rows_bind_by_child_thread_identity() {
    use crate::kit::atoms::{BG_DISPLAY, BG_LIVE_DETAIL, BG_TASK_IDENTITY, BG_TASKS};
    use crate::kit::stream_data::TuiTextChunk;

    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    BG_TASKS.state().write().clear();
    BG_DISPLAY.state().write().clear();
    BG_LIVE_DETAIL.state().write().clear();
    BG_TASK_IDENTITY.state().write().clear();
    crate::kit::atoms::BG_AGENT_IDS.state().write().clear();
    crate::kit::atoms::BG_TASK_REVISION.set(None);
    let mut state = super::make_state(SessionPhase::PromptRunning, 0);
    let child_text = |agent_id: &str, text: &str| {
        AcpEventData::TextChunk(TuiTextChunk {
            text: text.into(),
            message_id: Some(format!("{agent_id}-msg")),
            agent_id: Some(agent_id.into()),
        })
    };
    let task_started = |task_id: &str, child_thread_id: &str| {
        AcpEventData::BgTaskStarted(crate::kit::acp_types::BgTaskEntry {
            task_id: task_id.into(),
            kind: "agent".into(),
            summary: "worker".into(),
            started_at: String::new(),
            pid: None,
            revision: None,
            status: Some("running".into()),
            child_thread_id: Some(child_thread_id.into()),
        })
    };

    for agent in ["child-a", "child-b"] {
        super::dispatch_and_notify(
            &mut state,
            &AcpEventData::SubagentStarted {
                agent_id: agent.into(),
                agent_name: "coder".into(),
                is_background: true,
                parent_tool_call_id: None,
            },
        );
    }
    super::dispatch_and_notify(&mut state, &child_text("child-a", "body-a"));
    super::dispatch_and_notify(&mut state, &child_text("child-b", "body-b"));

    // 任务行按相反顺序到达：B 的行先出现，A 的行后出现。
    super::dispatch_and_notify(&mut state, &task_started("task-b", "child-b"));
    super::dispatch_and_notify(&mut state, &task_started("task-a", "child-a"));

    let links: Vec<(String, Option<String>)> = BG_DISPLAY
        .state()
        .read()
        .iter()
        .map(|row| (row.id.clone(), row.linked_agent_id.clone()))
        .collect();
    assert!(
        links.contains(&("task-a".into(), Some("child-a".into()))),
        "A 的行必须绑定 child-a，实际: {links:?}"
    );
    assert!(
        links.contains(&("task-b".into(), Some("child-b".into()))),
        "B 的行必须绑定 child-b，实际: {links:?}"
    );

    crate::kit::bg_task_live::publish_pending_streams();
    let live_store = BG_LIVE_DETAIL.state();
    let live = live_store.read();
    assert!(!live.contains_key("child-a") && !live.contains_key("child-b"));
    let text = |task_id: &str| {
        live[task_id]
            .nested_units
            .iter()
            .filter_map(|unit| match unit {
                TuiRenderUnit::TuiAssistantBubble(bubble) => Some(bubble.text.clone()),
                _ => None,
            })
            .collect::<String>()
    };
    assert_eq!(text("task-a"), "body-a", "预览不得串到 B 的任务行");
    assert_eq!(text("task-b"), "body-b", "预览不得串到 A 的任务行");
    drop(live);

    BG_TASKS.state().write().clear();
    BG_DISPLAY.state().write().clear();
    BG_LIVE_DETAIL.state().write().clear();
    BG_TASK_IDENTITY.state().write().clear();
    crate::kit::atoms::BG_AGENT_IDS.state().write().clear();
}

/// [回归 / CodeRabbit Minor] resume 复用 agent_id 时，当前 occurrence 的占位
/// owner 必须优先于旧任务行/旧 identity，旧终态明细不能吞掉新一轮文本。
#[test]
#[serial]
fn test_resumed_background_occurrence_uses_current_pending_owner() {
    use crate::kit::atoms::{BG_DISPLAY, BG_LIVE_DETAIL, BG_TASK_IDENTITY, BG_TASKS};
    use crate::kit::stream_data::TuiTextChunk;

    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    BG_TASKS.state().write().clear();
    BG_DISPLAY.state().write().clear();
    BG_LIVE_DETAIL.state().write().clear();
    BG_TASK_IDENTITY.state().write().clear();
    crate::kit::atoms::BG_AGENT_IDS.state().write().clear();
    crate::kit::atoms::BG_TASK_REVISION.set(None);
    let mut state = super::make_state(SessionPhase::PromptRunning, 0);
    let child_text = |text: &str| {
        AcpEventData::TextChunk(TuiTextChunk {
            text: text.into(),
            message_id: Some("child-msg".into()),
            agent_id: Some("child-x".into()),
        })
    };
    let task_started = |task_id: &str| {
        AcpEventData::BgTaskStarted(crate::kit::acp_types::BgTaskEntry {
            task_id: task_id.into(),
            kind: "agent".into(),
            summary: "worker".into(),
            started_at: String::new(),
            pid: None,
            revision: None,
            status: Some("running".into()),
            child_thread_id: Some("child-x".into()),
        })
    };

    // 旧 occurrence：任务行与 child 正常配对并完成（行保留在显示区，BG_TASKS 移除）。
    super::dispatch_and_notify(&mut state, &task_started("task-old"));
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "child-x".into(),
            agent_name: "coder".into(),
            is_background: true,
            parent_tool_call_id: None,
        },
    );
    super::dispatch_and_notify(&mut state, &child_text("old-body"));
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStopped {
            agent_id: "child-x".into(),
            result: "old done".into(),
            is_error: false,
        },
    );
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::BgTaskCompleted {
            task_id: "task-old".into(),
            kind: Some("agent".into()),
            success: true,
            duration_ms: 10,
            output_preview: None,
            revision: None,
        },
    );
    assert!(
        BG_TASKS
            .state()
            .read()
            .iter()
            .all(|task| task.task_id != "task-old"),
        "旧任务应从活动任务中移除"
    );

    // resume：同一 agent_id 的新 occurrence，任务行尚未出现。
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "child-x".into(),
            agent_name: "coder".into(),
            is_background: true,
            parent_tool_call_id: None,
        },
    );
    super::dispatch_and_notify(&mut state, &child_text("new-body"));
    crate::kit::bg_task_live::publish_pending_streams();

    {
        let live_store = BG_LIVE_DETAIL.state();
        let live = live_store.read();
        let pending = live
            .get("child-x")
            .expect("current occurrence pending owner");
        assert_eq!(pending.status, crate::kit::atoms::BgLiveStatus::Running);
        let pending_text: String = pending
            .nested_units
            .iter()
            .filter_map(|unit| match unit {
                TuiRenderUnit::TuiAssistantBubble(bubble) => Some(bubble.text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(pending_text, "new-body", "新一轮文本必须进入当前占位 owner");
        assert_eq!(
            live["task-old"].status,
            crate::kit::atoms::BgLiveStatus::Succeeded,
            "旧任务明细保持旧终态"
        );
    }

    // 新任务行到达：占位 owner 精确移交，正文随行落到 task-new。
    super::dispatch_and_notify(&mut state, &task_started("task-new"));
    crate::kit::bg_task_live::publish_pending_streams();
    assert!(!BG_LIVE_DETAIL.state().read().contains_key("child-x"));
    assert_eq!(
        BG_LIVE_DETAIL.state().read()["task-new"].status,
        crate::kit::atoms::BgLiveStatus::Running
    );
    let new_text: String = BG_LIVE_DETAIL.state().read()["task-new"]
        .nested_units
        .iter()
        .filter_map(|unit| match unit {
            TuiRenderUnit::TuiAssistantBubble(bubble) => Some(bubble.text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(new_text, "new-body", "移交后 task-new 必须持有新一轮正文");
    assert_eq!(
        BG_DISPLAY
            .state()
            .read()
            .iter()
            .find(|row| row.id == "task-new")
            .and_then(|row| row.linked_agent_id.clone())
            .as_deref(),
        Some("child-x")
    );

    BG_TASKS.state().write().clear();
    BG_DISPLAY.state().write().clear();
    BG_LIVE_DETAIL.state().write().clear();
    BG_TASK_IDENTITY.state().write().clear();
    crate::kit::atoms::BG_AGENT_IDS.state().write().clear();
}

/// [回归 / CodeRabbit Minor] Stop 早于任务行时，终态预览仍要在行到达后移交，
/// 不能因为 child 已离开 BG_AGENT_IDS 就留在无法关联的占位 owner。
#[test]
#[serial]
fn test_background_child_stop_before_task_row_still_attaches_terminal_preview() {
    use crate::kit::atoms::{BG_DISPLAY, BG_LIVE_DETAIL, BG_TASK_IDENTITY, BG_TASKS};
    use crate::kit::stream_data::TuiTextChunk;

    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    BG_TASKS.state().write().clear();
    BG_DISPLAY.state().write().clear();
    BG_LIVE_DETAIL.state().write().clear();
    BG_TASK_IDENTITY.state().write().clear();
    crate::kit::atoms::BG_AGENT_IDS.state().write().clear();
    crate::kit::atoms::BG_TASK_REVISION.set(None);
    let mut state = super::make_state(SessionPhase::PromptRunning, 0);

    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "child-early".into(),
            agent_name: "coder".into(),
            is_background: true,
            parent_tool_call_id: None,
        },
    );
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "early-body".into(),
            message_id: Some("early-msg".into()),
            agent_id: Some("child-early".into()),
        }),
    );
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStopped {
            agent_id: "child-early".into(),
            result: "early done".into(),
            is_error: false,
        },
    );

    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::BgTaskStarted(crate::kit::acp_types::BgTaskEntry {
            task_id: "task-late".into(),
            kind: "agent".into(),
            summary: "worker".into(),
            started_at: String::new(),
            pid: None,
            revision: None,
            status: Some("running".into()),
            child_thread_id: Some("child-early".into()),
        }),
    );

    assert!(!BG_LIVE_DETAIL.state().read().contains_key("child-early"));
    let live_store = BG_LIVE_DETAIL.state();
    let live = live_store.read();
    let detail = live.get("task-late").expect("terminal preview attached");
    assert_eq!(detail.status, crate::kit::atoms::BgLiveStatus::Succeeded);
    assert_eq!(detail.subagent_result.as_deref(), Some("early done"));
    let texts: String = detail
        .nested_units
        .iter()
        .filter_map(|unit| match unit {
            TuiRenderUnit::TuiAssistantBubble(bubble) => Some(bubble.text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, "early-body");
    drop(live);

    assert_eq!(
        BG_DISPLAY
            .state()
            .read()
            .iter()
            .find(|row| row.id == "task-late")
            .and_then(|row| row.linked_agent_id.clone())
            .as_deref(),
        Some("child-early")
    );

    BG_TASKS.state().write().clear();
    BG_DISPLAY.state().write().clear();
    BG_LIVE_DETAIL.state().write().clear();
    BG_TASK_IDENTITY.state().write().clear();
    crate::kit::atoms::BG_AGENT_IDS.state().write().clear();
}

use crate::kit::atoms::{VIEW_MODELS, ViewModelsSnapshot};

use serial_test::serial;
