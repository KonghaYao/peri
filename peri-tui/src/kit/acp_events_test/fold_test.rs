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

/// 过程 entry 默认单行：流式（PromptRunning + trailing）reasoning 也是 Collapsed；
/// TurnDone 后 Completed 仍 Collapsed（同一行摘要，不因终态改变高度）。
#[test]
#[serial]
fn test_fold_pass_reasoning_running_collapsed_then_completed_collapsed() {
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
    let r = super::reasoning_of(&snap, 0);
    assert_eq!(
        r.status,
        EntryStatus::Running,
        "流式中 reasoning 应为 Running"
    );
    assert!(r.is_running);
    assert_eq!(
        r.fold,
        FoldState::Collapsed,
        "过程 entry 默认折叠为单行（running 同样）"
    );
    assert!(
        r.collapsed(),
        "Collapsed 经 collapsed() 访问器视为收起（body 不可见）"
    );

    // 追加文本 chunk——文本到达 = 本消息 thinking 块结束（方案 1）：
    // 推理块立即冻结为 Completed/Collapsed（`◐ Thinking…` 停止），正文继续流式。
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::TextChunk(TuiTextChunk {
            text: "回复内容".into(),
            message_id: Some("msg_r1".into()),
            agent_id: None,
        }),
    );
    let snap = VIEW_MODELS.state().read().clone();
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
        "推理结束后自动收束为单行 Collapsed"
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
    assert_eq!(
        r.fold,
        FoldState::Collapsed,
        "§7 completed 行 → Collapsed（自动收束为单行）"
    );
    assert!(r.collapsed());
}

/// 工具默认单行：running / success / error 一律 Collapsed（终态不改变高度）。
#[test]
#[serial]
fn test_fold_pass_tool_collapsed_across_lifecycle() {
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
        tool_card_of(&snap, 0).fold,
        FoldState::Collapsed,
        "running 工具同样默认单行折叠"
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
    let t = tool_card_of(&snap, 0);
    assert_eq!(
        t.fold,
        FoldState::Collapsed,
        "§7 tool completed → Collapsed"
    );
    assert!(!t.is_running);

    // error：新工具失败
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

/// 手动覆盖免疫：running reasoning 被用户展开后，流式继续不得强制收回 Collapsed。
#[test]
#[serial]
fn test_fold_pass_reasoning_manual_override_immune_during_streaming() {
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
    assert_eq!(
        super::reasoning_of(&snap, 0).fold,
        FoldState::Collapsed,
        "默认单行折叠"
    );

    // 用户手动展开（流式中）
    FOLD_OVERRIDES
        .state()
        .write()
        .insert(FoldKey::Reasoning("msg_r2".into()), FoldState::Expanded);

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
    assert_eq!(
        super::reasoning_of(&snap, 0).fold,
        FoldState::Expanded,
        "手动展开在流式期间免疫自动策略"
    );
}

/// tool 覆盖：手动展开已完成工具后，重建（output 更新）仍保持 + user_modified 恢复。
#[test]
#[serial]
fn test_fold_pass_tool_manual_override_restores_user_modified() {
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
    assert_eq!(tool_card_of(&snap, 0).fold, FoldState::Collapsed);

    FOLD_OVERRIDES
        .state()
        .write()
        .insert(FoldKey::Tool("t1".into()), FoldState::Expanded);

    // 新一轮 push（LocalUserBubble 触发 view_models 重建）——
    // 卡片重建后覆盖与 user_modified 恢复
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::LocalUserBubble {
            text: "第二个问题".into(),
            attachments: Vec::new(),
        },
    );
    let snap = VIEW_MODELS.state().read().clone();
    // 已结束的工具卡先归档，新 prompt 排在旧输出之后。
    let t = tool_card_of(&snap, 0);
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
    let tool = tool_card_of(&snap, 0);
    assert_eq!(tool.fold, FoldState::Collapsed, "冷重建工具卡保持单行");
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
