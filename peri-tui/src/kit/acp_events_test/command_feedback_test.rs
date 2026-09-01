use super::*;

/// Phase 4 步骤 8：CommandFeedback 消费——inject_system_note 后 current_turn
/// 含 SystemNote，level 映射 Info→Info / Warning→Warning / Error→Error
/// （tui_render_unit.rs:553）；UiOnly/Session 两通道均走同一通路。
#[test]
#[serial]
fn test_command_feedback_injects_system_note() {
    crate::kit::atoms::init_atoms();
    *VIEW_MODELS.state().write() = ViewModelsSnapshot::default();
    let mut state = BridgeState {
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
    };

    // Info（UiOnly 通道）
    dispatch_and_notify(
        &mut state,
        &AcpEventData::CommandFeedback(TuiCommandFeedback {
            level: FeedbackLevel::Info,
            message: "命令完成".into(),
            channel: FeedbackChannel::UiOnly,
        }),
    );
    // Warning（UiOnly 通道）
    dispatch_and_notify(
        &mut state,
        &AcpEventData::CommandFeedback(TuiCommandFeedback {
            level: FeedbackLevel::Warning,
            message: "配置未生效".into(),
            channel: FeedbackChannel::UiOnly,
        }),
    );
    // Error（Session 通道——v1 同样走 inject_system_note，不进 ACP 消息）
    dispatch_and_notify(
        &mut state,
        &AcpEventData::CommandFeedback(TuiCommandFeedback {
            level: FeedbackLevel::Error,
            message: "命令执行失败".into(),
            channel: FeedbackChannel::Session,
        }),
    );

    let vms = state.current_turn.view_models();
    let notes: Vec<(String, TuiNoteLevel)> = vms
        .iter()
        .filter_map(|vm| match vm {
            TuiRenderUnit::TuiSystemNote(n) => Some((n.text.clone(), n.level.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        notes,
        vec![
            ("命令完成".to_string(), TuiNoteLevel::Info),
            ("配置未生效".to_string(), TuiNoteLevel::Warning),
            ("命令执行失败".to_string(), TuiNoteLevel::Error),
        ],
        "三条 CommandFeedback 应按时序注入 current_turn 内部 SystemNote"
    );
}
