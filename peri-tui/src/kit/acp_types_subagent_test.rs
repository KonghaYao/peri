use super::*;

#[test]
fn test_stopped_subagent_late_tool_start_keeps_children_terminal() {
    let mut turn = CurrentTurn::new();
    turn.start_subagent("child".into(), "coder".into(), None);
    turn.start_subagent_tool(
        "child",
        ToolCardAccumulator::with_input(
            "early".into(),
            "Shell".into(),
            "null".into(),
            serde_json::Value::Null,
            None,
        ),
    );
    turn.stop_subagent("child", false, "done");
    turn.start_subagent_tool(
        "child",
        ToolCardAccumulator::with_input(
            "early".into(),
            "Shell".into(),
            "echo done".into(),
            serde_json::json!({"command": "echo done"}),
            None,
        ),
    );
    turn.start_subagent_tool(
        "child",
        ToolCardAccumulator::with_input(
            "late".into(),
            "Shell".into(),
            "pwd".into(),
            serde_json::json!({"command": "pwd"}),
            None,
        ),
    );
    let TuiRenderUnit::TuiSubAgentGroup(group) = &turn.view_models()[0] else {
        panic!("expected subagent group");
    };
    assert!(!group.is_running);
    assert_eq!(group.view_models.len(), 2);
    let TuiRenderUnit::TuiToolCard(early) = &group.view_models[0] else {
        panic!("expected tool card");
    };
    assert_eq!(early.input_summary, "echo done");
    assert!(group.view_models.iter().all(|unit| matches!(unit,
        TuiRenderUnit::TuiToolCard(card) if !card.is_running
    )));
}

#[test]
fn test_current_turn_subagent_streaming_builds_nested_group() {
    let mut ct = CurrentTurn::new();
    ct.start_subagent("agent-1".into(), "researcher".into(), None);
    assert!(ct.append_subagent_text("agent-1", "hello"));
    assert!(ct.start_subagent_tool(
        "agent-1",
        ToolCardAccumulator::new("tc-1".into(), "Read".into(), "path: foo.rs".into()),
    ));
    assert!(ct.end_subagent_tool("agent-1", "tc-1", "10 lines".into(), false));

    let vms: Vec<_> = ct.view_models().iter().cloned().collect();
    assert_eq!(vms.len(), 1);
    match &vms[0] {
        TuiRenderUnit::TuiSubAgentGroup(group) => {
            assert_eq!(group.agent_id, "agent-1");
            assert_eq!(group.agent_name, "researcher");
            assert_eq!(group.view_models.len(), 2);
        }
        other => panic!("expected TuiSubAgentGroup, got {other:?}"),
    }
}

/// [回归测试] 同一个 child thread 在当前主 turn 内恢复时会复用 agent_id。
/// 已停止的旧分组必须保持封闭，新一轮 SubagentStarted 应创建并关联到新的
/// Agent ToolCard；恢复后的事件只能进入新分组。
#[test]
fn test_current_turn_resumed_subagent_routes_to_new_agent_group() {
    let mut ct = CurrentTurn::new();
    ct.start_tool(ToolCardAccumulator::new(
        "agent-call-1".into(),
        "Agent".into(),
        "start coder".into(),
    ));
    ct.start_subagent("child-1".into(), "coder".into(), None);
    assert!(ct.append_subagent_text("child-1", "before interruption"));
    assert!(ct.start_subagent_tool(
        "child-1",
        ToolCardAccumulator::new("child-tool-1".into(), "Read".into(), "old.rs".into()),
    ));
    assert!(ct.end_subagent_tool("child-1", "child-tool-1", "old output".into(), false));
    ct.stop_subagent("child-1", true, "model stream interrupted");
    assert!(ct.end_tool("agent-call-1", "child_thread_id: child-1".into(), true));

    ct.start_tool(ToolCardAccumulator::new(
        "agent-call-2".into(),
        "Agent".into(),
        "continue child-1".into(),
    ));
    ct.start_subagent("child-1".into(), "coder".into(), None);
    assert!(ct.append_subagent_text("child-1", "after resume"));
    assert!(ct.append_subagent_reasoning("child-1", "resumed reasoning"));
    assert!(ct.start_subagent_tool(
        "child-1",
        ToolCardAccumulator::new("child-tool-2".into(), "Shell".into(), "cargo test".into()),
    ));
    assert!(ct.end_subagent_tool("child-1", "child-tool-2", "passed".into(), false));
    ct.stop_subagent("child-1", false, "completed");

    assert_ne!(
        ct.subagents[0].instance_id, ct.subagents[1].instance_id,
        "恢复 occurrence 必须获得新的 TUI instance_id"
    );
    assert_eq!(ct.subagents[0].agent_id, ct.subagents[1].agent_id);
    assert_eq!(ct.subagents.len(), 2, "恢复应创建第二个可见分组");
    assert_eq!(
        ct.subagents[0].child_turn.text, "before interruption",
        "旧失败分组不得接收恢复后的消息"
    );
    assert!(!ct.subagents[0].is_running);
    assert_eq!(ct.subagents[1].child_turn.text, "after resume");
    assert_eq!(ct.subagents[1].child_turn.reasoning, "resumed reasoning");
    assert!(!ct.subagents[1].is_running);
    assert!(!ct.subagents[1].is_error);
    assert_eq!(ct.subagents[0].child_turn.tool_cards.len(), 1);
    assert_eq!(
        ct.subagents[0].child_turn.tool_cards[0].tool_id,
        "child-tool-1"
    );
    assert_eq!(ct.subagents[1].child_turn.tool_cards.len(), 1);
    assert_eq!(
        ct.subagents[1].child_turn.tool_cards[0].tool_id,
        "child-tool-2"
    );
    assert!(
        ct.tool_cards.iter().all(|tool| tool.claimed_by_subagent),
        "原始与恢复 Agent 调用都应关联各自的 Subagent 分组"
    );

    let vms = ct.view_models().clone();
    assert_eq!(vms.len(), 4, "应按 Agent/分组/Agent/分组交错显示");
    assert!(matches!(vms[0], TuiRenderUnit::TuiToolCard(_)));
    assert!(matches!(vms[1], TuiRenderUnit::TuiSubAgentGroup(_)));
    assert!(matches!(vms[2], TuiRenderUnit::TuiToolCard(_)));
    assert!(matches!(vms[3], TuiRenderUnit::TuiSubAgentGroup(_)));
}

/// [回归测试] Agent ToolCard 晚于 SubagentStarted 到达 TUI（并行多 Agent 批次里
/// 非首个工具调用的 ToolStarted 只在 dispatch 阶段发出，经 forwarder 两个 hop，
/// 与子 Agent invoke 内直发的 SubagentStarted 竞争）时，子分组段不得滞留在上一个
/// 仍在 loading 的 Agent 调用之下：迟到的 Agent 卡片必须认领早到的子分组段。
#[test]
fn test_late_agent_card_adopts_early_subagent_group() {
    let mut ct = CurrentTurn::new();
    // 第一个 Agent 调用：卡片先到（流式提前 ToolStarted），子分组紧随其后。
    ct.start_tool(ToolCardAccumulator::new(
        "agent-call-1".into(),
        "Agent".into(),
        "start coder".into(),
    ));
    ct.start_subagent("child-1".into(), "coder".into(), None);
    assert!(ct.start_subagent_tool(
        "child-1",
        ToolCardAccumulator::new("child-tool-1".into(), "Read".into(), "a.rs".into()),
    ));

    // 第二个 Agent 的子 Agent 先启动（SubagentStarted 抢在父卡片之前到达），
    // 此时主 turn 里没有未认领的 Agent 卡片。
    ct.start_subagent("child-2".into(), "reviewer".into(), None);
    assert!(ct.start_subagent_tool(
        "child-2",
        ToolCardAccumulator::new("child-tool-2".into(), "Grep".into(), "foo".into()),
    ));

    // 迟到的第二个 Agent 卡片。
    ct.start_tool(ToolCardAccumulator::new(
        "agent-call-2".into(),
        "Agent".into(),
        "start reviewer".into(),
    ));

    let vms: Vec<_> = ct.view_models().iter().cloned().collect();
    assert_eq!(vms.len(), 4, "应按 Agent/分组/Agent/分组交错显示");
    match (&vms[0], &vms[1], &vms[2], &vms[3]) {
        (
            TuiRenderUnit::TuiToolCard(first),
            TuiRenderUnit::TuiSubAgentGroup(first_group),
            TuiRenderUnit::TuiToolCard(second),
            TuiRenderUnit::TuiSubAgentGroup(second_group),
        ) => {
            assert_eq!(first.tool_id, "agent-call-1");
            assert_eq!(second.tool_id, "agent-call-2");
            assert_eq!(
                first_group.agent_id, "child-1",
                "仍在 loading 的第一个 Agent 调用之下只能是它自己的子分组"
            );
            assert_eq!(
                second_group.agent_id, "child-2",
                "迟到的 Agent 卡片必须紧跟它自己的子分组"
            );
        }
        other => panic!("expected Agent/group/Agent/group interleaving, got {other:?}"),
    }
    assert!(
        ct.tool_cards.iter().all(|tool| tool.claimed_by_subagent),
        "两张 Agent 卡片都应认领分组"
    );
}

/// [回归测试] `SubagentStarted.parent_tool_call_id` 是配对的权威依据：并发批次里
/// 子 Agent 的启动顺序与工具卡片顺序无关，身份配对必须不看到达顺序。
#[test]
fn test_subagent_group_pairs_by_parent_tool_call_id_not_arrival_order() {
    let mut ct = CurrentTurn::new();
    // 并行批次：两张 Agent 卡片都已就位（ToolStarted 先到）。
    ct.start_tool(ToolCardAccumulator::new(
        "agent-call-a".into(),
        "Agent".into(),
        "start coder".into(),
    ));
    ct.start_tool(ToolCardAccumulator::new(
        "agent-call-b".into(),
        "Agent".into(),
        "start reviewer".into(),
    ));

    // 子 Agent 启动顺序与卡片顺序相反（并发竞态）：B 先到，A 后到。
    ct.start_subagent(
        "child-b".into(),
        "reviewer".into(),
        Some("agent-call-b".into()),
    );
    assert!(ct.start_subagent_tool(
        "child-b",
        ToolCardAccumulator::new("child-tool-b".into(), "Grep".into(), "b.rs".into()),
    ));
    ct.start_subagent(
        "child-a".into(),
        "coder".into(),
        Some("agent-call-a".into()),
    );
    assert!(ct.start_subagent_tool(
        "child-a",
        ToolCardAccumulator::new("child-tool-a".into(), "Read".into(), "a.rs".into()),
    ));

    let vms: Vec<_> = ct.view_models().iter().cloned().collect();
    match (&vms[0], &vms[1], &vms[2], &vms[3]) {
        (
            TuiRenderUnit::TuiToolCard(card_a),
            TuiRenderUnit::TuiSubAgentGroup(group_a),
            TuiRenderUnit::TuiToolCard(card_b),
            TuiRenderUnit::TuiSubAgentGroup(group_b),
        ) => {
            assert_eq!(card_a.tool_id, "agent-call-a");
            assert_eq!(card_b.tool_id, "agent-call-b");
            assert_eq!(
                group_a.agent_id, "child-a",
                "身份配对：A 卡片下必须是 A 的子分组，不能因 B 先到而互换"
            );
            assert_eq!(
                group_b.agent_id, "child-b",
                "身份配对：B 卡片下必须是 B 的子分组"
            );
        }
        other => panic!("expected Agent/group/Agent/group interleaving, got {other:?}"),
    }
}

/// [回归测试] 分组先到 + 有父身份：迟到的 Agent 卡片按身份认领自己的分组，
/// 即使时间线上还存在另一张未认领的 Agent 卡片（顺序兜底会配错）。
#[test]
fn test_late_agent_card_claims_group_by_parent_tool_call_id() {
    let mut ct = CurrentTurn::new();
    // 卡片 A 先到并已认领自己的分组。
    ct.start_tool(ToolCardAccumulator::new(
        "agent-call-a".into(),
        "Agent".into(),
        "start coder".into(),
    ));
    ct.start_subagent(
        "child-a".into(),
        "coder".into(),
        Some("agent-call-a".into()),
    );

    // B 的分组先到（父卡片尚未到达）：此时 A 已认领，顺序兜底会把它挂到 A 之下。
    ct.start_subagent(
        "child-b".into(),
        "reviewer".into(),
        Some("agent-call-b".into()),
    );
    assert!(ct.start_subagent_tool(
        "child-b",
        ToolCardAccumulator::new("child-tool-b".into(), "Grep".into(), "b.rs".into()),
    ));

    // 迟到的卡片 B。
    ct.start_tool(ToolCardAccumulator::new(
        "agent-call-b".into(),
        "Agent".into(),
        "start reviewer".into(),
    ));

    let vms: Vec<_> = ct.view_models().iter().cloned().collect();
    match (&vms[0], &vms[1], &vms[2], &vms[3]) {
        (
            TuiRenderUnit::TuiToolCard(card_a),
            TuiRenderUnit::TuiSubAgentGroup(group_a),
            TuiRenderUnit::TuiToolCard(card_b),
            TuiRenderUnit::TuiSubAgentGroup(group_b),
        ) => {
            assert_eq!(card_a.tool_id, "agent-call-a");
            assert_eq!(group_a.agent_id, "child-a");
            assert_eq!(card_b.tool_id, "agent-call-b");
            assert_eq!(
                group_b.agent_id, "child-b",
                "迟到的卡片必须按身份认领，而不是认领更早的待配对分组"
            );
        }
        other => panic!("expected Agent/group/Agent/group interleaving, got {other:?}"),
    }
}

/// [回归测试] 有父身份的分组绝不按到达顺序猜：身份不匹配的 Agent 卡片出现时
/// 不得认领它；只有身份匹配的卡片才能把该分组段落位。
#[test]
fn test_identified_group_never_adopted_by_unrelated_agent_card() {
    let mut ct = CurrentTurn::new();
    // 分组先到且带身份（父卡片尚未到达）。
    ct.start_subagent(
        "child-late".into(),
        "coder".into(),
        Some("agent-call-late".into()),
    );
    // 另一张无关的 Agent 卡片到达：身份不匹配，不能认领该分组。
    ct.start_tool(ToolCardAccumulator::new(
        "agent-call-other".into(),
        "Agent".into(),
        "start unrelated".into(),
    ));

    let vms: Vec<_> = ct.view_models().iter().cloned().collect();
    match (vms.first(), vms.get(1)) {
        (Some(TuiRenderUnit::TuiSubAgentGroup(_)), Some(TuiRenderUnit::TuiToolCard(card))) => {
            assert_eq!(
                card.tool_id, "agent-call-other",
                "身份不匹配的卡片不得把分组段落位到自己之后"
            );
        }
        other => panic!("expected group then unrelated card, got {other:?}"),
    }

    // 身份匹配的卡片到达：此时才把分组段落位到它之后。
    ct.start_tool(ToolCardAccumulator::new(
        "agent-call-late".into(),
        "Agent".into(),
        "start coder".into(),
    ));
    let vms: Vec<_> = ct.view_models().iter().cloned().collect();
    match (vms.first(), vms.get(1), vms.get(2)) {
        (
            Some(TuiRenderUnit::TuiToolCard(other_card)),
            Some(TuiRenderUnit::TuiToolCard(late_card)),
            Some(TuiRenderUnit::TuiSubAgentGroup(group)),
        ) => {
            assert_eq!(other_card.tool_id, "agent-call-other");
            assert_eq!(late_card.tool_id, "agent-call-late");
            assert_eq!(
                group.agent_id, "child-late",
                "身份匹配后分组落到自己的卡片之后"
            );
        }
        other => panic!("expected card/card/group, got {other:?}"),
    }
}

#[test]
fn test_current_turn_subagent_unknown_route_returns_false() {
    let mut ct = CurrentTurn::new();
    assert!(!ct.append_subagent_text("missing", "hello"));
    assert!(ct.view_models().is_empty());
}

/// [回归测试] ToolStarted 后无 ToolEnded 直接 SubagentStopped：
/// stop_subagent 必须 deactivate child_turn，否则无 output_summary 的
/// 工具卡保持 Running（is_running = turn_active && 无输出），渲染为永久进行中。
#[test]
fn test_stop_subagent_without_tool_ended_deactivates_child_turn() {
    let mut ct = CurrentTurn::new();
    ct.start_subagent("agent-1".into(), "researcher".into(), None);
    assert!(ct.start_subagent_tool(
        "agent-1",
        ToolCardAccumulator::new("tc-1".into(), "Read".into(), "path: foo.rs".into()),
    ));
    // 无 end_subagent_tool，直接 stop
    ct.stop_subagent("agent-1", false, "");

    let s = ct
        .subagents
        .iter_mut()
        .find(|s| s.agent_id == "agent-1")
        .expect("subagent 应存在");
    assert!(
        !s.child_turn.active,
        "stop_subagent 后 child_turn 必须 deactivate（ToolStarted 无 ToolEnded 场景）"
    );
    let vms: Vec<_> = s.child_turn.view_models().iter().cloned().collect();
    assert_eq!(vms.len(), 1, "child_turn 应仍保留工具卡");
    match &vms[0] {
        TuiRenderUnit::TuiToolCard(card) => {
            assert!(
                !card.is_running,
                "ToolStarted 无 ToolEnded 时停止，tool card 不应保持 Running"
            );
        }
        other => panic!("expected TuiToolCard, got {other:?}"),
    }
}

/// M1: SubAgentAccumulator content_hash 随 child VM 内容变化。
/// 相同结构（1 个 child）但不同文本 → 不同 content_hash。
#[test]
fn test_subagent_content_hash_changes_with_child_content() {
    let mut acc1 = SubAgentAccumulator::new("agent-1".into(), "worker".into());
    acc1.append_text("hello");
    let vm1 = acc1.view_model();
    let hash1 = match &vm1 {
        TuiRenderUnit::TuiSubAgentGroup(g) => g.content_hash,
        _ => panic!("expected TuiSubAgentGroup"),
    };

    let mut acc2 = SubAgentAccumulator::new("agent-1".into(), "worker".into());
    acc2.append_text("world");
    let vm2 = acc2.view_model();
    let hash2 = match &vm2 {
        TuiRenderUnit::TuiSubAgentGroup(g) => g.content_hash,
        _ => panic!("expected TuiSubAgentGroup"),
    };

    assert_ne!(
        hash1, hash2,
        "不同 child 内容应产出不同 content_hash（M1 修复前会相等）"
    );
}
