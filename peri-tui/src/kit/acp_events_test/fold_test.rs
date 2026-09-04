use super::*;

// ── Slice 2：折叠状态机（spec §7 表）经 push_view_models 单点 pass ──────────

fn tool_card_of(
    snapshot: &ViewModelsSnapshot,
    idx: usize,
) -> &crate::kit::tui_render_unit::TuiToolCard {
    match &snapshot.items[idx] {
        TuiRenderUnit::TuiToolCard(t) => t,
        other => panic!("expected TuiToolCard at [{idx}], got {other:?}"),
    }
}

/// [回归测试] 工具运行中取消 turn 后，归档卡片必须停止 loading 动画。
///
/// `TurnInterrupted` 不会再收到对应的 `ToolEnded`；归档路径必须根据 turn 已停止
/// 的事实把在途工具渲染为非 running，否则最后一张工具卡会永久显示 spinner。
#[test]
#[serial]
fn test_turn_interrupted_stops_running_tool_card_animation() {
    let mut state = make_fold_test_state();

    dispatch_and_notify(
        &mut state,
        &AcpEventData::PromptSubmitted {
            request_id: Some("r1".into()),
        },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "t1".into(),
            tool_name: "Bash".into(),
            input_summary: "sleep 10".into(),
            raw_input: serde_json::json!({"command": "sleep 10"}),
            agent_id: None,
        }),
    );
    let running = VIEW_MODELS.state().read().clone();
    assert!(tool_card_of(&running, 0).is_running);

    dispatch_and_notify(
        &mut state,
        &AcpEventData::TurnInterrupted {
            reason: "user cancelled".into(),
            request_id: Some("r1".into()),
        },
    );

    let interrupted = VIEW_MODELS.state().read().clone();
    let tool = tool_card_of(&interrupted, 0);
    assert!(!tool.is_running, "取消后工具卡不得继续显示 running");
    assert!(
        !interrupted.items[0].is_animating(),
        "取消后工具卡不得继续驱动 spinner 动画"
    );
}

/// §7 reasoning 行：流式（PromptRunning + trailing）→ Preview/Running；
/// TurnDone 后 phase 离开 PromptRunning → 全 Completed → Collapsed 单行。
#[test]
#[serial]
fn test_fold_pass_reasoning_running_preview_then_completed_collapsed() {
    let mut state = make_fold_test_state();

    dispatch_and_notify(
        &mut state,
        &AcpEventData::ReasoningChunk(TuiReasoningChunk {
            text: "正在检查消息类型……".into(),
            message_id: Some("msg_r1".into()),
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    let r = reasoning_of(&snap, 0);
    assert_eq!(
        r.status,
        EntryStatus::Running,
        "流式中 reasoning 应为 Running"
    );
    assert!(r.is_running);
    assert_eq!(r.fold, FoldState::Preview, "§7 running 行 → Preview");
    assert!(
        !r.collapsed(),
        "Preview 经 collapsed() 访问器视为展开（body 可见）"
    );

    // 追加文本 chunk——文本到达 = 本消息 thinking 块结束（方案 1）：
    // 推理块立即冻结为 Completed/Collapsed（`◐ Thinking…` 停止），正文继续流式。
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "回复内容".into(),
            message_id: Some("msg_r1".into()),
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    let r = reasoning_of(&snap, 0);
    assert_eq!(
        r.status,
        EntryStatus::Completed,
        "文本到达后推理应冻结为 Completed"
    );
    assert!(!r.is_running, "文本到达后推理不再 running");
    assert_eq!(
        r.fold,
        FoldState::Collapsed,
        "推理结束后自动收束为单行 Collapsed"
    );
    assert!(
        r.duration_ms.is_some(),
        "推理冻结时应携带时长（Thought for Ns）"
    );

    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let snap = VIEW_MODELS.state().read().clone();
    let r = reasoning_of(&snap, 0);
    assert_eq!(
        r.status,
        EntryStatus::Completed,
        "TurnDone 后 reasoning 应为 Completed"
    );
    assert!(!r.is_running);
    assert_eq!(
        r.fold,
        FoldState::Collapsed,
        "§7 completed 行 → Collapsed（自动收束为单行）"
    );
    assert!(r.collapsed());
}

/// §7 tool 行：running → Preview；success/error 终态均 → Collapsed。
#[test]
#[serial]
fn test_fold_pass_tool_preview_then_terminal_collapsed() {
    let mut state = make_fold_test_state();

    // running：ToolStarted
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "t1".into(),
            tool_name: "Read".into(),
            input_summary: "README.md".into(),
            raw_input: serde_json::json!({"path": "README.md"}),
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(tool_card_of(&snap, 0).fold, FoldState::Preview);
    assert!(!tool_card_of(&snap, 0).user_modified);

    // success：ToolEnded（is_error=false）
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(TuiToolEnded {
            tool_id: "t1".into(),
            output_summary: "ok".into(),
            is_error: false,
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    let t = tool_card_of(&snap, 0);
    assert_eq!(
        t.fold,
        FoldState::Collapsed,
        "§7 tool completed → Collapsed"
    );
    assert!(!t.is_running);

    // error：新工具失败
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "t2".into(),
            tool_name: "Bash".into(),
            input_summary: "false".into(),
            raw_input: serde_json::json!({"command": "false"}),
            agent_id: None,
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(TuiToolEnded {
            tool_id: "t2".into(),
            output_summary: "exit 1".into(),
            is_error: true,
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    let t = tool_card_of(&snap, 1);
    assert_eq!(
        t.fold,
        FoldState::Collapsed,
        "§7 tool error → Collapsed，完成时不得自动展开导致高度突变"
    );
}

/// §7 subagent 行：running 与 completed 均为 Collapsed（running 靠 live summary 表达）。
#[test]
#[serial]
fn test_fold_pass_subagent_running_and_completed_collapsed() {
    let mut state = make_fold_test_state();

    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "sa-1".into(),
            agent_name: "explorer".into(),
            is_background: false,
        },
    );
    let snap = VIEW_MODELS.state().read().clone();
    match &snap.items[0] {
        TuiRenderUnit::TuiSubAgentGroup(g) => {
            assert!(g.is_running);
            assert_eq!(
                g.fold,
                FoldState::Collapsed,
                "§7 subagent running → Collapsed + live summary"
            );
        }
        other => panic!("expected TuiSubAgentGroup, got {other:?}"),
    }

    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStopped {
            agent_id: "sa-1".into(),
            result: String::new(),
            is_error: false,
        },
    );
    let snap = VIEW_MODELS.state().read().clone();
    match &snap.items[0] {
        TuiRenderUnit::TuiSubAgentGroup(g) => {
            assert!(!g.is_running);
            assert_eq!(g.fold, FoldState::Collapsed);
        }
        other => panic!("expected TuiSubAgentGroup, got {other:?}"),
    }
}

/// SubagentStopped(is_error=true) → parent 终态 Error：is_running=false、
/// is_error=true、error_reason 保存 stop result；§7 表 (SubAgent, Error)
/// => Expanded（与 tool error 展开语义一致）。
#[test]
#[serial]
fn test_fold_pass_subagent_error_expanded() {
    let mut state = make_fold_test_state();

    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "sa-1".into(),
            agent_name: "explorer".into(),
            is_background: false,
        },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStopped {
            agent_id: "sa-1".into(),
            result: "loop failed: llm error".into(),
            is_error: true,
        },
    );
    let snap = VIEW_MODELS.state().read().clone();
    match &snap.items[0] {
        TuiRenderUnit::TuiSubAgentGroup(g) => {
            assert!(!g.is_running, "stop 后不得再 running");
            assert!(g.is_error, "canonical is_error=true");
            assert_eq!(
                g.error_reason.as_deref(),
                Some("loop failed: llm error"),
                "error_reason 保存 stop result"
            );
            assert_eq!(g.fold, FoldState::Expanded, "§7 subagent Error → Expanded");
        }
        other => panic!("expected TuiSubAgentGroup, got {other:?}"),
    }
}

/// whitespace-only `result` 不产生空白原因行：is_error=true 保持 parent Error
/// （× + §7 Expanded），但 `error_reason=None`——渲染层不输出空白原因行。
#[test]
#[serial]
fn test_subagent_error_whitespace_result_no_reason_line() {
    let mut state = make_fold_test_state();

    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "sa-1".into(),
            agent_name: "explorer".into(),
            is_background: false,
        },
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStopped {
            agent_id: "sa-1".into(),
            result: "   ".into(),
            is_error: true,
        },
    );
    let snap = VIEW_MODELS.state().read().clone();
    match &snap.items[0] {
        TuiRenderUnit::TuiSubAgentGroup(g) => {
            assert!(!g.is_running, "stop 后不得再 running");
            assert!(
                g.is_error,
                "whitespace-only result 不改变 canonical parent Error"
            );
            assert_eq!(
                g.error_reason, None,
                "whitespace-only result 视同无原因（渲染层不输出空白原因行）"
            );
            assert_eq!(g.fold, FoldState::Expanded, "§7 subagent Error → Expanded");
        }
        other => panic!("expected TuiSubAgentGroup, got {other:?}"),
    }
}

/// 核心 bug 回归：nested child tool error 不提升 parent block error。
/// 子工具失败 → parent 后续完成（SubagentStopped is_error=false）→
/// group is_error=false + fold Collapsed；child tool card 保持自身 error。
#[test]
#[serial]
fn test_subagent_completed_with_failed_child_tool_not_error() {
    let mut state = make_fold_test_state();

    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "sa-1".into(),
            agent_name: "explorer".into(),
            is_background: false,
        },
    );
    // 子工具启动（agent_id 路由到子 turn）
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "t1".into(),
            tool_name: "Grep".into(),
            input_summary: "src".into(),
            raw_input: serde_json::json!({"pattern": "x"}),
            agent_id: Some("sa-1".into()),
        }),
    );
    // 子工具失败
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(TuiToolEnded {
            tool_id: "t1".into(),
            output_summary: "Error: something went wrong".into(),
            is_error: true,
            agent_id: Some("sa-1".into()),
        }),
    );
    // parent 完成（is_error=false——subagent 整体成功，失败工具重试后继续）
    dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStopped {
            agent_id: "sa-1".into(),
            result: "done".into(),
            is_error: false,
        },
    );
    let snap = VIEW_MODELS.state().read().clone();
    match &snap.items[0] {
        TuiRenderUnit::TuiSubAgentGroup(g) => {
            assert!(!g.is_running);
            assert!(
                !g.is_error,
                "completed parent 不得因 child tool error 变 Error"
            );
            assert_eq!(g.error_reason, None, "completed parent 无 error_reason");
            assert_eq!(g.fold, FoldState::Collapsed, "§7 completed → Collapsed");
            // child tool card 保持自身 error 展示（局部可见，不提升 parent）
            match &g.view_models[0] {
                TuiRenderUnit::TuiToolCard(t) => {
                    assert!(t.is_error, "child tool error 保持局部可见");
                }
                other => panic!("expected child TuiToolCard, got {other:?}"),
            }
        }
        other => panic!("expected TuiSubAgentGroup, got {other:?}"),
    }
}

/// 手动覆盖免疫：reasoning 完成折叠后用户展开（FOLD_OVERRIDES），
/// 后续 push（新 turn 开始）不得重新折叠（spec §7「本 turn 内不再被自动策略覆盖」）。
#[test]
#[serial]
fn test_fold_pass_reasoning_manual_override_survives_following_turn() {
    let mut state = make_fold_test_state();

    dispatch_and_notify(
        &mut state,
        &AcpEventData::ReasoningChunk(TuiReasoningChunk {
            text: "第一轮思考".into(),
            message_id: Some("msg_r1".into()),
            agent_id: None,
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "第一轮回复".into(),
            message_id: Some("msg_r1".into()),
            agent_id: None,
        }),
    );
    dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let snap = VIEW_MODELS.state().read().clone();
    assert!(reasoning_of(&snap, 0).collapsed());

    // 用户手动展开（键盘 handler 的持久化等价操作）
    FOLD_OVERRIDES
        .state()
        .write()
        .insert(FoldKey::Reasoning("msg_r1".into()), FoldState::Expanded);

    // 新一轮开始——push 重建后手动展开必须保持
    dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble {
            text: "第二个问题".into(),
        },
    );
    let snap = VIEW_MODELS.state().read().clone();
    // items = [Assistant(msg_r1), UserBubble]——LocalUserBubble append 到 committed 尾部
    let r = reasoning_of(&snap, 0);
    assert_eq!(
        r.fold,
        FoldState::Expanded,
        "手动展开后跨 turn 不得被自动折叠"
    );
}

/// 手动覆盖免疫：running reasoning 被用户展开后，流式继续不得强制收回 Preview。
#[test]
#[serial]
fn test_fold_pass_reasoning_manual_override_immune_during_streaming() {
    let mut state = make_fold_test_state();

    dispatch_and_notify(
        &mut state,
        &AcpEventData::ReasoningChunk(TuiReasoningChunk {
            text: "思考中".into(),
            message_id: Some("msg_r2".into()),
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(reasoning_of(&snap, 0).fold, FoldState::Preview);

    // 用户手动展开（流式中）
    FOLD_OVERRIDES
        .state()
        .write()
        .insert(FoldKey::Reasoning("msg_r2".into()), FoldState::Expanded);

    // 继续 streaming——不得收回 Preview
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ReasoningChunk(TuiReasoningChunk {
            text: "继续思考".into(),
            message_id: Some("msg_r2".into()),
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(
        reasoning_of(&snap, 0).fold,
        FoldState::Expanded,
        "手动展开在流式期间免疫自动策略"
    );
}

/// tool 覆盖：手动展开已完成工具后，重建（output 更新）仍保持 + user_modified 恢复。
#[test]
#[serial]
fn test_fold_pass_tool_manual_override_restores_user_modified() {
    let mut state = make_fold_test_state();

    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "t1".into(),
            tool_name: "Read".into(),
            input_summary: "a".into(),
            raw_input: serde_json::json!({"path": "a"}),
            agent_id: None,
        }),
    );
    dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(TuiToolEnded {
            tool_id: "t1".into(),
            output_summary: "done".into(),
            is_error: false,
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(tool_card_of(&snap, 0).fold, FoldState::Collapsed);

    FOLD_OVERRIDES
        .state()
        .write()
        .insert(FoldKey::Tool("t1".into()), FoldState::Expanded);

    // 新一轮 push（LocalUserBubble 触发 view_models 重建）——
    // 卡片重建后覆盖与 user_modified 恢复
    dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble {
            text: "第二个问题".into(),
        },
    );
    let snap = VIEW_MODELS.state().read().clone();
    // items = [UserBubble, ToolCard]——工具卡在 current_turn，LocalUserBubble append 到 committed
    let t = tool_card_of(&snap, 1);
    assert_eq!(t.fold, FoldState::Expanded, "手动展开跨重建保持");
    assert!(
        t.user_modified,
        "覆盖存在的 entry 恢复 user_modified=true（免疫自动策略）"
    );
}

/// session 复位（BRIDGE_RESET_COUNTER → push_view_models_for_reset）清空覆盖表。
#[test]
#[serial]
fn test_push_view_models_for_reset_clears_fold_overrides() {
    make_fold_test_state();
    FOLD_OVERRIDES
        .state()
        .write()
        .insert(FoldKey::Tool("t1".into()), FoldState::Expanded);
    assert!(!FOLD_OVERRIDES.state().read().is_empty());
    // [S2 §3.4] 焦点单一事实源随 session 复位同步清空（slot/key 依赖旧会话
    // 索引与身份，残留会让新会话焦点/免疫错误指向）。
    *crate::kit::atoms::FOCUSED_ENTRY.state().write() = Some(crate::kit::atoms::FocusedEntry {
        slot: 0,
        key: Some(FoldKey::Tool("t1".into())),
    });
    assert!(crate::kit::atoms::FOCUSED_ENTRY.state().read().is_some());

    push_view_models_for_reset();

    assert!(
        FOLD_OVERRIDES.state().read().is_empty(),
        "session 复位必须清空覆盖表（跨 session 身份不唯一）"
    );
    assert!(
        crate::kit::atoms::FOCUSED_ENTRY.state().read().is_none(),
        "session 复位必须清空 entry 焦点"
    );
    assert!(VIEW_MODELS.state().read().items.is_empty());
}
