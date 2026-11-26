use super::AcpEventData;
use super::BridgeState;
use super::SessionPhase;
use super::TuiRenderUnit;
use super::TuiToolEnded;
use super::TuiToolStarted;
use super::json;
use super::serial;

fn replay_start(state: &mut BridgeState, tool_id: &str) {
    super::dispatch_and_notify(
        state,
        &AcpEventData::ReplayToolStarted {
            tool_id: tool_id.into(),
            tool_name: "Read".into(),
            input_summary: "src/main.rs".into(),
            raw_input: json!({"file_path": "src/main.rs"}),
        },
    );
}

fn history_card(state: &BridgeState, tool_id: &str) -> crate::kit::tui_render_unit::TuiToolCard {
    state
        .committed
        .iter()
        .find_map(|unit| match unit {
            TuiRenderUnit::TuiToolCard(card) if card.tool_id == tool_id => Some(card.clone()),
            _ => None,
        })
        .expect("historical tool card")
}

#[test]
#[serial]
fn history_without_result_is_static_and_does_not_claim_success() {
    let mut state = super::make_fold_test_state();
    state.phase = SessionPhase::Idle;
    replay_start(&mut state, "history-missing");

    let card = history_card(&state, "history-missing");
    assert!(!card.is_running);
    assert!(card.is_error);
    assert_eq!(
        card.output_summary,
        crate::i18n::tr("msg-history-tool-result-missing")
    );
    assert_eq!(card.running_duration_ms, None);
    assert_eq!(card.completed_duration_ms, None);
    assert_eq!(card.diff, None);
    assert_eq!(state.phase, SessionPhase::Idle);
    assert!(state.current_turn.is_empty());
    assert!(
        VIEW_MODELS
            .state()
            .read()
            .items
            .iter()
            .all(|unit| !unit.is_animating())
    );
}

#[test]
#[serial]
fn history_result_updates_static_card_for_success_and_error() {
    for is_error in [false, true] {
        let mut state = super::make_fold_test_state();
        state.phase = SessionPhase::Idle;
        replay_start(&mut state, "history-result");
        let before = history_card(&state, "history-result");
        super::dispatch_and_notify(
            &mut state,
            &AcpEventData::ReplayToolEnded {
                tool_id: "history-result".into(),
                output_summary: "recorded result".into(),
                is_error,
            },
        );

        let after = history_card(&state, "history-result");
        assert_eq!(after.output_summary, "recorded result");
        assert_eq!(after.is_error, is_error);
        assert!(!after.is_running);
        assert_ne!(before.content_hash, after.content_hash);
        assert_eq!(state.committed.len(), 1);
        assert_eq!(state.phase, SessionPhase::Idle);
    }
}

#[test]
#[serial]
fn history_unknown_result_does_not_change_other_cards() {
    let mut state = super::make_fold_test_state();
    state.phase = SessionPhase::Idle;
    replay_start(&mut state, "history-missing");
    let before = history_card(&state, "history-missing");
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ReplayToolEnded {
            tool_id: "unknown-tool".into(),
            output_summary: "result".into(),
            is_error: false,
        },
    );
    assert_eq!(
        history_card(&state, "history-missing").content_hash,
        before.content_hash
    );
}

#[test]
#[serial]
fn live_tool_still_runs_after_history_replay() {
    let mut state = super::make_fold_test_state();
    state.phase = SessionPhase::Idle;
    replay_start(&mut state, "history-missing");
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::PromptSubmitted {
            request_id: Some("live-request".into()),
        },
    );
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolStarted(TuiToolStarted {
            tool_id: "live-tool".into(),
            tool_name: "Read".into(),
            input_summary: "src/main.rs".into(),
            raw_input: json!({"file_path": "src/main.rs"}),
            agent_id: None,
            origin_message_id: None,
            origin_readable: None,
        }),
    );
    assert_eq!(state.phase, SessionPhase::PromptRunning);
    assert!(state.current_turn.view_models().iter().any(|unit| matches!(unit, TuiRenderUnit::TuiToolCard(card) if card.tool_id == "live-tool" && card.is_running)));
    assert!(!history_card(&state, "history-missing").is_running);
    super::dispatch_and_notify(
        &mut state,
        &AcpEventData::ToolEnded(TuiToolEnded {
            images: Vec::new(),
            images_omitted: 0,
            tool_id: "live-tool".into(),
            output_summary: "live result".into(),
            is_error: false,
            agent_id: None,
        }),
    );
    assert!(state.current_turn.view_models().iter().any(|unit| matches!(unit, TuiRenderUnit::TuiToolCard(card) if card.tool_id == "live-tool" && !card.is_running && card.output_summary == "live result")));
}

use crate::kit::atoms::VIEW_MODELS;
