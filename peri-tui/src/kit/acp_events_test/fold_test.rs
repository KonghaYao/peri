use super::AcpEventData;
use super::EntryStatus;
use super::FoldKey;
use super::FoldState;
use super::TuiReasoningChunk;
use super::TuiRenderUnit;
use super::TuiTextChunk;
use super::TuiToolEnded;
use super::TuiToolStarted;
use super::serial;

use crate::kit::tui_render_unit::TuiCollapsedGroup;

// ── Slice 2：折叠状态机（spec §7 表）经 push_view_models 单点 pass ──────────

fn tool_card_of(
    snapshot: &ViewModelsSnapshot,
    idx: usize,
) -> &crate::kit::tui_render_unit::TuiToolCard {
    crate::kit::tui_render_unit::flatten_units(&snapshot.items)
        .into_iter()
        .filter_map(|vm| match vm {
            TuiRenderUnit::TuiToolCard(t) => Some(t),
            _ => None,
        })
        .nth(idx)
        .unwrap_or_else(|| panic!("expected TuiToolCard #{idx}, got {:?}", snapshot.items))
}

fn group_of(snapshot: &ViewModelsSnapshot, idx: usize) -> &TuiCollapsedGroup {
    snapshot
        .items
        .iter()
        .filter_map(|vm| match vm {
            TuiRenderUnit::TuiCollapsedGroup(group) => Some(group),
            _ => None,
        })
        .nth(idx)
        .unwrap_or_else(|| {
            panic!(
                "expected TuiCollapsedGroup #{idx}, got {:?}",
                snapshot.items
            )
        })
}

/// [回归测试] 工具运行中取消 turn 后，归档卡片必须停止 loading 动画。
///
/// `TurnInterrupted` 不会再收到对应的 `ToolEnded`；归档路径必须根据 turn 已停止
/// 的事实把在途工具渲染为非 running，否则最后一张工具卡会永久显示 spinner。
#[test]
#[serial]
fn test_turn_interrupted_stops_running_tool_card_animation() {
    let mut state = super::make_fold_test_state();

    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::PromptSubmitted {
            request_id: Some("r1".into()),
        },
    );
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "t1".into(),
            tool_name: "Bash".into(),
            input_summary: "sleep 10".into(),
            raw_input: serde_json::json!({"command": "sleep 10"}),
            agent_id: None,
            origin_message_id: None,
            origin_readable: None,
        }),
    );
    let running = VIEW_MODELS.state().read().clone();
    assert!(tool_card_of(&running, 0).is_running);

    super::dispatch_and_notify(
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

/// 纯 reasoning 段（无正文）属过程区：进入正文空档折叠组，组默认单行；
/// 组内成员保留折叠 pass 给出的单行折叠（不再强制展开）。文本到达后同一
/// bubble 变成正文，reasoning 回到正文气泡内的单行摘要。
#[test]
#[serial]
fn test_fold_pass_reasoning_only_grouped_then_body_summary() {
    let mut state = super::make_fold_test_state();

    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ReasoningChunk(TuiReasoningChunk {
            text: "正在检查消息类型……".into(),
            message_id: Some("msg_r1".into()),
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    let group = group_of(&snap, 0);
    assert_eq!(group.fold, FoldState::Collapsed, "组默认折叠为一行");
    assert_eq!(group.count, 1, "纯 reasoning 段成组");
    let r = crate::kit::tui_render_unit::flatten_units(&snap.items)
        .into_iter()
        .find_map(|vm| match vm {
            TuiRenderUnit::TuiAssistantBubble(b) => b.reasoning.as_ref(),
            _ => None,
        })
        .expect("组内应含 reasoning");
    assert_eq!(
        r.status,
        EntryStatus::Running,
        "流式中 reasoning 应为 Running"
    );
    assert!(r.is_running);
    assert_eq!(
        r.fold,
        FoldState::Collapsed,
        "组内成员保留折叠 pass 的单行折叠（可单独点开）"
    );

    // 追加文本 chunk——文本到达 = 本消息 thinking 块结束（方案 1）：
    // 推理块立即冻结为 Completed/Collapsed（`◐ Thinking…` 停止），正文继续流式；
    // 同一 bubble 成为正文，reasoning 是正文前的单行摘要。
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "回复内容".into(),
            message_id: Some("msg_r1".into()),
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    assert!(
        snap.items
            .iter()
            .all(|vm| !matches!(vm, TuiRenderUnit::TuiCollapsedGroup(_))),
        "正文到达后不再是过程组"
    );
    let r = super::reasoning_of(&snap, 0);
    assert_eq!(
        r.status,
        EntryStatus::Completed,
        "文本到达后推理应冻结为 Completed"
    );
    assert!(!r.is_running, "文本到达后推理不再 running");
    assert_eq!(
        r.fold,
        FoldState::Collapsed,
        "正文前的 thinking 收束为单行摘要"
    );
    assert!(
        r.duration_ms.is_some(),
        "推理冻结时应携带时长（Thought for Ns）"
    );

    super::dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let snap = VIEW_MODELS.state().read().clone();
    let r = super::reasoning_of(&snap, 0);
    assert_eq!(
        r.status,
        EntryStatus::Completed,
        "TurnDone 后 reasoning 应为 Completed"
    );
    assert!(!r.is_running);
    assert_eq!(r.fold, FoldState::Collapsed);
    assert!(r.collapsed());
}

/// 工具属过程区：进入正文空档折叠组，组行默认单行（running / success / error
/// 同一高度）；组内工具按原样展开。失败数在组行摘要上保持可见。
#[test]
#[serial]
fn test_fold_pass_tool_grouped_across_lifecycle() {
    let mut state = super::make_fold_test_state();

    // running：ToolStarted
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "t1".into(),
            tool_name: "Read".into(),
            input_summary: "README.md".into(),
            raw_input: serde_json::json!({"path": "README.md"}),
            agent_id: None,
            origin_message_id: None,
            origin_readable: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(
        group_of(&snap, 0).fold,
        FoldState::Collapsed,
        "组默认单行折叠"
    );
    assert_eq!(
        tool_card_of(&snap, 0).fold,
        FoldState::Collapsed,
        "组内工具保留默认单行折叠（可单独点开）"
    );
    assert!(!tool_card_of(&snap, 0).user_modified);

    // success：ToolEnded（is_error=false）
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(TuiToolEnded {
            tool_id: "t1".into(),
            output_summary: "ok".into(),
            is_error: false,
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(
        group_of(&snap, 0).fold,
        FoldState::Collapsed,
        "终态不改变组行高度"
    );
    assert_eq!(group_of(&snap, 0).failed_count, 0);
    assert!(!tool_card_of(&snap, 0).is_running);

    // error：新工具失败——仍属同一组，组行保留失败计数。
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "t2".into(),
            tool_name: "Bash".into(),
            input_summary: "false".into(),
            raw_input: serde_json::json!({"command": "false"}),
            agent_id: None,
            origin_message_id: None,
            origin_readable: None,
        }),
    );
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(TuiToolEnded {
            tool_id: "t2".into(),
            output_summary: "exit 1".into(),
            is_error: true,
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    let group = group_of(&snap, 0);
    assert_eq!(group.count, 2, "同一空档内的工具同组");
    assert_eq!(group.failed_count, 1, "失败数在组行保持可见");
    assert!(tool_card_of(&snap, 1).is_error);
    assert_eq!(
        tool_card_of(&snap, 1).fold,
        FoldState::Collapsed,
        "组内失败工具同样保留单行折叠（失败信号由组行摘要承载）"
    );
}

/// §7 subagent 行：running 与 completed 均为 Collapsed（running 靠 live summary 表达）。
#[test]
#[serial]
fn test_fold_pass_subagent_running_and_completed_collapsed() {
    let mut state = super::make_fold_test_state();

    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "sa-1".into(),
            agent_name: "explorer".into(),
            is_background: false,
            parent_tool_call_id: None,
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

    super::dispatch_and_notify(
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
    let mut state = super::make_fold_test_state();

    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "sa-1".into(),
            agent_name: "explorer".into(),
            is_background: false,
            parent_tool_call_id: None,
        },
    );
    super::dispatch_and_notify(
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
    let mut state = super::make_fold_test_state();

    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "sa-1".into(),
            agent_name: "explorer".into(),
            is_background: false,
            parent_tool_call_id: None,
        },
    );
    super::dispatch_and_notify(
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
    let mut state = super::make_fold_test_state();

    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::SubagentStarted {
            agent_id: "sa-1".into(),
            agent_name: "explorer".into(),
            is_background: false,
            parent_tool_call_id: None,
        },
    );
    // 子工具启动（agent_id 路由到子 turn）
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "t1".into(),
            tool_name: "Grep".into(),
            input_summary: "src".into(),
            raw_input: serde_json::json!({"pattern": "x"}),
            agent_id: Some("sa-1".into()),
            origin_message_id: None,
            origin_readable: None,
        }),
    );
    // 子工具失败
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(TuiToolEnded {
            tool_id: "t1".into(),
            output_summary: "Error: something went wrong".into(),
            is_error: true,
            agent_id: Some("sa-1".into()),
        }),
    );
    // parent 完成（is_error=false——subagent 整体成功，失败工具重试后继续）
    super::dispatch_and_notify(
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
    let mut state = super::make_fold_test_state();

    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ReasoningChunk(TuiReasoningChunk {
            text: "第一轮思考".into(),
            message_id: Some("msg_r1".into()),
            agent_id: None,
        }),
    );
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "第一轮回复".into(),
            message_id: Some("msg_r1".into()),
            agent_id: None,
        }),
    );
    super::dispatch_and_notify(&mut state, &AcpEventData::TurnDone);
    let snap = VIEW_MODELS.state().read().clone();
    assert!(super::reasoning_of(&snap, 0).collapsed());

    // 用户手动展开（键盘 handler 的持久化等价操作）
    FOLD_OVERRIDES
        .state()
        .write()
        .insert(FoldKey::Reasoning("msg_r1".into()), FoldState::Expanded);

    // 新一轮开始——push 重建后手动展开必须保持
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble {
            text: "第二个问题".into(),
            attachments: Vec::new(),
        },
    );
    let snap = VIEW_MODELS.state().read().clone();
    // items = [Assistant(msg_r1), UserBubble]——LocalUserBubble append 到 committed 尾部
    let r = super::reasoning_of(&snap, 0);
    assert_eq!(
        r.fold,
        FoldState::Expanded,
        "手动展开后跨 turn 不得被自动折叠"
    );
}

/// 手动覆盖免疫：组在流式中被用户展开后，流式继续不得强制收回 Collapsed。
#[test]
#[serial]
fn test_fold_pass_group_manual_override_immune_during_streaming() {
    let mut state = super::make_fold_test_state();

    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ReasoningChunk(TuiReasoningChunk {
            text: "思考中".into(),
            message_id: Some("msg_r2".into()),
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    let key = group_of(&snap, 0).key.clone();
    assert_eq!(key, "reasoning:msg_r2");
    assert_eq!(
        group_of(&snap, 0).fold,
        FoldState::Collapsed,
        "默认单行折叠"
    );

    // 用户手动展开（流式中）
    FOLD_OVERRIDES
        .state()
        .write()
        .insert(FoldKey::Group(key.clone()), FoldState::Expanded);

    // 继续 streaming——不得收回 Collapsed
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ReasoningChunk(TuiReasoningChunk {
            text: "继续思考".into(),
            message_id: Some("msg_r2".into()),
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(group_of(&snap, 0).key, key, "组身份跨流式稳定");
    assert_eq!(
        group_of(&snap, 0).fold,
        FoldState::Expanded,
        "手动展开在流式期间免疫自动策略"
    );
}

/// 无 ACP message_id 的 reasoning（v1 兼容）：分配本地身份后组键稳定，
/// 流式重建间身份不变，用户展开该组的选择不被后续 chunk 收回。
#[test]
#[serial]
fn test_fold_pass_unkeyed_reasoning_gets_stable_local_identity() {
    let mut state = super::make_fold_test_state();

    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ReasoningChunk(TuiReasoningChunk {
            text: "无身份思考".into(),
            message_id: None,
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    let group = group_of(&snap, 0);
    assert_eq!(group.fold, FoldState::Collapsed);
    let local_id = {
        let bubble = crate::kit::tui_render_unit::flatten_units(&snap.items)
            .into_iter()
            .find_map(|vm| match vm {
                TuiRenderUnit::TuiAssistantBubble(bubble) => Some(bubble),
                _ => None,
            })
            .expect("expected TuiAssistantBubble");
        let local_id = bubble.message_id.clone().expect("无 ACP id 时分配本地身份");
        assert!(
            local_id.starts_with("local:"),
            "本地身份必须带区分前缀，实际 {local_id}"
        );
        local_id
    };
    assert_eq!(group.key, format!("reasoning:{local_id}"), "组键取本地身份");
    let key = group.key.clone();
    drop(snap);

    // 用户展开该组（键盘 handler 的持久化等价操作）。
    FOLD_OVERRIDES
        .state()
        .write()
        .insert(FoldKey::Group(key.clone()), FoldState::Expanded);

    // 继续流式：重建后必须仍用同一身份命中覆盖。
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ReasoningChunk(TuiReasoningChunk {
            text: "，继续思考".into(),
            message_id: None,
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(group_of(&snap, 0).key, key, "同一 trailing 段的组键不变");
    assert_eq!(
        group_of(&snap, 0).fold,
        FoldState::Expanded,
        "本地身份在流式重建间稳定，展开不被收回"
    );
    let bubble = crate::kit::tui_render_unit::flatten_units(&snap.items)
        .into_iter()
        .find_map(|vm| match vm {
            TuiRenderUnit::TuiAssistantBubble(bubble) => Some(bubble),
            _ => None,
        })
        .expect("expected TuiAssistantBubble");
    assert_eq!(
        bubble.message_id.as_deref(),
        Some(local_id.as_str()),
        "同一 trailing 段的身份不变"
    );
}

/// 组覆盖：手动展开工具组后，重建（新 prompt 触发装配）仍保持展开。
#[test]
#[serial]
fn test_fold_pass_group_manual_override_survives_rebuild() {
    let mut state = super::make_fold_test_state();

    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "t1".into(),
            tool_name: "Read".into(),
            input_summary: "a".into(),
            raw_input: serde_json::json!({"path": "a"}),
            agent_id: None,
            origin_message_id: None,
            origin_readable: None,
        }),
    );
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(TuiToolEnded {
            tool_id: "t1".into(),
            output_summary: "done".into(),
            is_error: false,
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(group_of(&snap, 0).fold, FoldState::Collapsed);

    FOLD_OVERRIDES
        .state()
        .write()
        .insert(FoldKey::Group("tool:t1".into()), FoldState::Expanded);

    // 新一轮 push（LocalUserBubble 触发 view_models 重建）——组重建后覆盖恢复
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble {
            text: "第二个问题".into(),
            attachments: Vec::new(),
        },
    );
    let snap = VIEW_MODELS.state().read().clone();
    // 已结束的工具组先归档，新 prompt 排在旧输出之后。
    let group = group_of(&snap, 0);
    assert_eq!(group.key, "tool:t1");
    assert_eq!(group.fold, FoldState::Expanded, "手动展开跨重建保持");
    assert_eq!(
        tool_card_of(&snap, 0).fold,
        FoldState::Collapsed,
        "组内成员保留自己的折叠态（不再随组强制展开）"
    );

    // 成员自己的覆盖优先：单独展开组内成员后重建，仍只有该成员展开。
    FOLD_OVERRIDES
        .state()
        .write()
        .insert(FoldKey::Tool("t1".into()), FoldState::Expanded);
    crate::kit::acp_events::push_view_models(&mut state);
    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(group_of(&snap, 0).fold, FoldState::Expanded, "组展开保持");
    assert_eq!(
        tool_card_of(&snap, 0).fold,
        FoldState::Expanded,
        "成员覆盖经折叠 pass 恢复"
    );
}

/// session 复位（BRIDGE_RESET_COUNTER → push_view_models_for_reset）清空覆盖表。
#[test]
#[serial]
fn test_push_view_models_for_reset_clears_fold_overrides() {
    super::make_fold_test_state();
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

    super::push_view_models_for_reset();

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

/// 粘贴图片附件的本地提交：事件携带的附件记录进入用户气泡（消息区因此能显示
/// 「这条消息带了图片」），不是只留文本。
#[test]
#[serial]
fn test_local_user_bubble_carries_attachment_records() {
    let mut state = super::make_fold_test_state();
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble {
            text: "看这张图".into(),
            attachments: vec![
                crate::kit::tui_render_unit::UserAttachment { bytes: 45 },
                crate::kit::tui_render_unit::UserAttachment { bytes: 2048 },
            ],
        },
    );

    let snap = VIEW_MODELS.state().read().clone();
    let bubble = snap
        .items
        .iter()
        .find_map(|item| match item {
            TuiRenderUnit::TuiUserBubble(bubble) => Some(bubble.clone()),
            _ => None,
        })
        .expect("应有用户气泡");
    assert_eq!(bubble.text, "看这张图");
    assert_eq!(bubble.attachments.len(), 2, "两条附件记录随气泡冻结");
    assert_eq!(bubble.attachments[0].bytes, 45);
    assert_eq!(bubble.attachments[1].bytes, 2048);
}

/// 历史冷重建：replay 构造的工具卡与 committed 中的 reasoning 都回到默认单行折叠。
///
/// 旧快照/旧版本可能留下 `Preview` 或 running 形态；冷投影（phase != PromptRunning）
/// 必须归一为 `Collapsed`，与实时流式路径同一默认。
#[test]
#[serial]
fn test_cold_history_rebuild_keeps_default_collapsed() {
    let mut state = super::make_fold_test_state();

    // replay 工具：起始（结果缺失）与终态都按折叠策略构造。
    let started = crate::kit::acp_events::tool::replay_tool_started_unit(
        "h1",
        "Read",
        "a.rs",
        &serde_json::json!({"path": "a.rs"}),
        None,
        false,
    );
    let TuiRenderUnit::TuiToolCard(started_card) = started else {
        panic!("replay tool unit 应为 TuiToolCard")
    };
    let completed = crate::kit::acp_events::tool::completed_tool_card(&started_card, "ok", false);
    assert_eq!(
        completed.fold,
        FoldState::Collapsed,
        "replay 构造即默认单行折叠"
    );

    // committed 中残留 Running/Preview 形态的 reasoning（例如旧快照）。
    let mut bubble = crate::kit::tui_render_unit::TuiAssistantBubble {
        text: "历史正文".into(),
        reasoning: Some(crate::kit::tui_render_unit::TuiReasoningBlock {
            text: "历史思考".into(),
            fold: FoldState::Preview,
            status: EntryStatus::Running,
            is_running: true,
            started_at: None,
            duration_ms: None,
        }),
        message_id: Some("msg_h1".into()),
        started_at: None,
        duration_ms: None,
        content_hash: 0,
    };
    bubble.recompute_hash();

    state.committed = im::Vector::from(vec![
        TuiRenderUnit::TuiToolCard(completed),
        TuiRenderUnit::TuiAssistantBubble(bubble.into()),
    ]);
    state.phase = super::SessionPhase::Idle;
    crate::kit::acp_events::render::push_view_models(&mut state);

    let snap = VIEW_MODELS.state().read().clone();
    assert_eq!(
        group_of(&snap, 0).fold,
        FoldState::Collapsed,
        "冷重建工具组保持单行"
    );
    let tool = tool_card_of(&snap, 0);
    assert_eq!(
        tool.fold,
        FoldState::Collapsed,
        "组内工具按折叠 pass 的单行默认态重建"
    );
    let reasoning = super::reasoning_of(&snap, 1);
    assert_eq!(
        reasoning.status,
        EntryStatus::Completed,
        "冷重建把残留 running 归一为 Completed"
    );
    assert_eq!(
        reasoning.fold,
        FoldState::Collapsed,
        "冷重建 reasoning 保持单行折叠"
    );
}

use crate::kit::atoms::{FOLD_OVERRIDES, VIEW_MODELS, ViewModelsSnapshot};
