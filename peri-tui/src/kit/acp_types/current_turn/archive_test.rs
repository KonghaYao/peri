use super::*;
use crate::kit::acp_types::ToolCardAccumulator;

fn tool(tool_id: &str, tool_name: &str) -> ToolCardAccumulator {
    ToolCardAccumulator::new(tool_id.into(), tool_name.into(), String::new())
}

fn bubble_text(unit: &TuiRenderUnit) -> &str {
    match unit {
        TuiRenderUnit::TuiAssistantBubble(bubble) => &bubble.text,
        other => panic!("expected assistant bubble, got {other:?}"),
    }
}

#[test]
fn prefix_archive_rebases_remaining_offsets_and_keeps_trailing() {
    let mut turn = CurrentTurn::new();
    turn.append_text("hello", Some("m1"));
    turn.start_tool(tool("t1", "Read"));
    turn.end_tool("t1", "ok".into(), false);
    turn.append_text(" world", Some("m2"));
    // 运行中的工具阻断切点：其前的文本/工具都稳定。
    turn.start_tool(tool("t2", "Bash"));

    let prefix = turn.take_archivable_prefix();
    assert_eq!(prefix.len(), 3);
    assert_eq!(bubble_text(&prefix[0]), "hello");
    assert_eq!(bubble_text(&prefix[2]), " world");
    assert_eq!(turn.text, "", "前缀正文必须整体移出容器");
    assert_eq!(turn.last_text_flush, 0, "flush 基点随正文重基准");
    assert_eq!(turn.segments.len(), 1);
    assert_eq!(turn.view_models().len(), 1, "只剩运行中工具卡");

    // 阻断工具结束后，剩余正文归档；未 flush 的 trailing 仍留在容器。
    turn.end_tool("t2", "done".into(), false);
    turn.append_text("!", Some("m3"));
    let prefix = turn.take_archivable_prefix();
    assert_eq!(prefix.len(), 1);
    assert_eq!(turn.text, "!");
    assert_eq!(
        turn.view_models().len(),
        1,
        "trailing bubble 不得被前缀吞掉"
    );
}

#[test]
fn running_tool_blocks_prefix_at_its_segment() {
    let mut turn = CurrentTurn::new();
    turn.append_text("done", Some("m1"));
    turn.start_tool(tool("t1", "Bash"));

    let prefix = turn.take_archivable_prefix();
    assert_eq!(prefix.len(), 1);
    assert_eq!(bubble_text(&prefix[0]), "done");
    assert_eq!(turn.tool_cards.len(), 1, "运行中卡片留在容器");
    assert_eq!(turn.segments.len(), 1);
    assert!(
        turn.take_archivable_prefix().is_empty(),
        "同一运行中工具不得被重复归档"
    );

    turn.end_tool("t1", "ok".into(), false);
    let prefix = turn.take_archivable_prefix();
    assert_eq!(prefix.len(), 1);
    assert!(turn.tool_cards.is_empty());
    assert!(turn.segments.is_empty());
}

#[test]
fn completed_prefix_archives_while_running_subagent_blocks() {
    let mut turn = CurrentTurn::new();
    turn.append_text("before", Some("m1"));
    turn.start_tool(tool("a1", "Agent"));
    turn.end_tool("a1", "done".into(), false);
    turn.start_subagent("done".into(), "done".into(), Some("a1".into()));
    turn.stop_subagent("done", false, "");
    turn.start_tool(tool("a2", "Agent"));
    turn.end_tool("a2", "done".into(), false);
    turn.start_subagent("running".into(), "running".into(), Some("a2".into()));

    let prefix = turn.take_archivable_prefix();
    assert_eq!(prefix.len(), 4, "文本、两张已完成卡片、已完成分组均稳定");
    assert!(matches!(
        &prefix[1],
        TuiRenderUnit::TuiToolCard(card) if card.tool_id == "a1"
    ));
    assert!(matches!(
        &prefix[2],
        TuiRenderUnit::TuiSubAgentGroup(group) if group.agent_id == "done"
    ));
    assert_eq!(turn.subagents.len(), 1);
    assert_eq!(turn.subagents[0].agent_id, "running");
    assert!(turn.subagents[0].is_running);
    assert!(turn.tool_cards.is_empty());
    assert_eq!(turn.segments.len(), 1);

    // 已归档子 Agent 的迟到事件被丢弃，不得复活容器。
    assert!(!turn.append_subagent_text("done", "late"));
}

#[test]
fn pending_subagent_group_blocks_cut_until_card_arrives() {
    let mut turn = CurrentTurn::new();
    turn.append_text("before", Some("m1"));
    turn.start_subagent("pending".into(), "pending".into(), Some("late-card".into()));
    turn.stop_subagent("pending", false, "");

    // 文本稳定，但分组尚未与父 Agent 卡片配对，提前归档会固化错误顺序。
    let prefix = turn.take_archivable_prefix();
    assert_eq!(prefix.len(), 1);
    assert_eq!(bubble_text(&prefix[0]), "before");
    assert_eq!(turn.subagents.len(), 1, "待配对分组留在容器");
    assert_eq!(turn.segments.len(), 1);

    turn.start_tool(tool("late-card", "Agent"));
    turn.end_tool("late-card", "done".into(), false);
    let prefix = turn.take_archivable_prefix();
    assert_eq!(prefix.len(), 2);
    assert!(matches!(
        &prefix[0],
        TuiRenderUnit::TuiToolCard(card) if card.tool_id == "late-card"
    ));
    assert!(matches!(
        &prefix[1],
        TuiRenderUnit::TuiSubAgentGroup(group) if group.agent_id == "pending"
    ));
    assert!(turn.segments.is_empty());
    assert!(turn.subagents.is_empty());
}

#[test]
fn archived_tool_ignores_late_end_event() {
    let mut turn = CurrentTurn::new();
    turn.start_tool(tool("t1", "Read"));
    turn.end_tool("t1", "ok".into(), false);
    turn.start_tool(tool("t2", "Bash"));

    assert_eq!(turn.take_archivable_prefix().len(), 1);
    assert!(
        !turn.end_tool("t1", "late".into(), false),
        "已归档工具的迟到结束事件是 no-op"
    );
    assert_eq!(turn.tool_cards.len(), 1, "不得重建已归档卡片");
}
