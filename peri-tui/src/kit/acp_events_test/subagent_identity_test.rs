use super::AcpEventData;
use super::BridgeState;
use super::TuiRenderUnit;
use super::TuiToolStarted;
use super::serial;

fn start_parent(state: &mut BridgeState, index: usize) {
    super::dispatch_and_notify(
        state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: format!("model-call:{index}"),
            tool_name: "Agent".into(),
            input_summary: "code-reviewer".into(),
            raw_input: serde_json::Value::Null,
            agent_id: None,
            origin_message_id: None,
            origin_readable: None,
        }),
    );
}

fn assert_owned_groups(state: &mut BridgeState, stopped: bool) {
    let snapshot = VIEW_MODELS.state().read().clone();
    assert_eq!(snapshot.items.len(), 6);
    for index in 0..3 {
        // 父调用工具卡属过程区，单独成组（组行紧跟其子 Agent 组）。
        let TuiRenderUnit::TuiCollapsedGroup(parent) = &snapshot.items[index * 2] else {
            panic!("each child group must follow its own parent card group");
        };
        let TuiRenderUnit::TuiToolCard(card) = &parent.view_models[0] else {
            panic!("parent group must contain the launcher tool card");
        };
        assert_eq!(card.tool_id, format!("model-call:{index}"));
        let TuiRenderUnit::TuiSubAgentGroup(group) = &snapshot.items[index * 2 + 1] else {
            panic!("parent card must be followed by its own child group");
        };
        assert_eq!(group.agent_id, format!("child:{index}"));
        assert_eq!(group.is_running, !stopped);
        let children: Vec<_> = group
            .view_models
            .iter()
            .filter_map(|unit| match unit {
                TuiRenderUnit::TuiToolCard(tool) => Some(tool.tool_id.as_str()),
                _ => None,
            })
            .collect();
        let expected: Vec<_> = (0..3)
            .map(|tool_index| format!("child-tool:{index}:{tool_index}"))
            .collect();
        assert_eq!(children, expected);
    }
    assert_eq!(state.current_turn.tool_cards.len(), 3);
}

#[test]
#[serial]
fn three_same_named_subagents_keep_distinct_owners_across_arrival_orders() {
    for parents_first in [false, true] {
        for order in [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ] {
            let mut state = super::make_fold_test_state();
            if parents_first {
                for index in 0..3 {
                    start_parent(&mut state, index);
                }
            }
            for index in order {
                super::dispatch_and_notify(
                    &mut state,
                    &AcpEventData::SubagentStarted {
                        agent_id: format!("child:{index}"),
                        agent_name: "code-reviewer".into(),
                        is_background: false,
                        parent_tool_call_id: Some(format!("model-call:{index}")),
                    },
                );
                for tool_index in 0..3 {
                    super::dispatch_and_notify(
                        &mut state,
                        &AcpEventData::ToolStarted(TuiToolStarted {
                            tool_id: format!("child-tool:{index}:{tool_index}"),
                            tool_name: "Read".into(),
                            input_summary: format!("owner-{index}-{tool_index}.rs"),
                            raw_input: serde_json::Value::Null,
                            agent_id: Some(format!("child:{index}")),
                            origin_message_id: None,
                            origin_readable: None,
                        }),
                    );
                }
            }
            if !parents_first {
                for index in 0..3 {
                    start_parent(&mut state, index);
                }
            }
            assert_owned_groups(&mut state, false);
            for index in order {
                super::dispatch_and_notify(
                    &mut state,
                    &AcpEventData::SubagentStopped {
                        agent_id: format!("child:{index}"),
                        result: "done".into(),
                        is_error: false,
                    },
                );
            }
            assert_owned_groups(&mut state, true);
        }
    }
}

use crate::kit::atoms::VIEW_MODELS;
