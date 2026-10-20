//! F06-b：运行中 SubAgent 不再让整个 turn 常驻；稳定前缀在 flush 边界归档。

use super::*;

fn todo_start() -> AcpEventData {
    AcpEventData::ToolStarted(crate::kit::stream_data::TuiToolStarted {
        tool_id: "todo-1".into(),
        tool_name: "TodoWrite".into(),
        input_summary: "todos: 1".into(),
        raw_input: json!({
            "todos": [{"content": "任务", "status": "pending"}]
        }),
        agent_id: None,
    })
}

fn todo_end() -> AcpEventData {
    AcpEventData::ToolEnded(crate::kit::stream_data::TuiToolEnded {
        tool_id: "todo-1".into(),
        output_summary: "+[0]".into(),
        is_error: false,
        agent_id: None,
    })
}

#[test]
#[serial]
fn flush_archives_stable_prefix_while_subagent_runs() {
    let mut state = make_fold_test_state();
    state.phase = SessionPhase::PromptRunning;

    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: "before child".into(),
            message_id: Some("root-1".into()),
            agent_id: None,
        }),
    );
    dispatch_and_notify(&mut state, &todo_start());
    dispatch_and_notify(&mut state, &todo_end());
    assert!(state.last_successful_todos.is_some());
    let baseline = state.last_successful_todo_sequence;

    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "child".into(),
            agent_name: "child".into(),
            is_background: false,
            parent_tool_call_id: None,
        },
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);

    assert!(state.current_turn.has_running_subagent());
    assert_eq!(
        state.committed.len(),
        2,
        "文本与已完成的 TodoWrite 卡片应在 flush 边界归档"
    );
    assert!(state.current_turn.text.is_empty(), "稳定正文不得继续驻留");
    assert!(
        state.current_turn.tool_cards.is_empty(),
        "稳定工具卡不得驻留"
    );
    assert_eq!(
        state.current_turn.view_model_count(),
        1,
        "容器只保留运行中子 Agent 段"
    );

    // 重复 flush 不重复归档。
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    assert_eq!(state.committed.len(), 2);

    // 已归档 TodoWrite 的迟到结束事件：不 panic、不推进基线、不重建卡片。
    dispatch_and_notify(&mut state, &todo_end());
    assert_eq!(
        state.last_successful_todo_sequence, baseline,
        "迟到事件不得推进 Todo 基线"
    );
    assert_eq!(state.committed.len(), 2);
    assert!(state.current_turn.tool_cards.is_empty());

    // 运行中子 Agent 的路由窗口仍然有效。
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(crate::kit::stream_data::TuiTextChunk {
            text: " still here".into(),
            message_id: None,
            agent_id: Some("child".into()),
        }),
    );
    assert_eq!(state.current_turn.subagents.len(), 1);
    assert_eq!(
        state.current_turn.subagents[0].child_turn.text,
        " still here"
    );

    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStopped {
            agent_id: "child".into(),
            result: String::new(),
            is_error: false,
        },
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    assert!(state.current_turn.is_empty());
    assert_eq!(state.committed.len(), 3, "停止后的分组完成归档");
}
