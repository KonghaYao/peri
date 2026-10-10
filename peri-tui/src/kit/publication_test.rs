//! 真实 handler → intent → scheduler 回归，不使用会强制补发的 dispatch_and_notify。
use super::*;
use crate::kit::stream_data::{TuiReasoningChunk, TuiTextChunk};
use crate::kit::tui_render_unit::{FoldKey, FoldState, TuiRenderUnit};
use serial_test::serial;

struct ModeGuard {
    mode: Option<String>,
    view: atoms::ViewModelsSnapshot,
    acp: atoms::AcpStateSnapshot,
    folds: std::collections::HashMap<FoldKey, FoldState>,
}
impl ModeGuard {
    fn new(mode: &str) -> Self {
        atoms::init_atoms();
        let view = atoms::VIEW_MODELS.state().read().clone();
        let acp = atoms::ACP_STATE.state().read().clone();
        let folds = atoms::FOLD_OVERRIDES.state().read().clone();
        let handle = atoms::TUI_CONFIG_HANDLE.get_or_init(|| {
            std::sync::Arc::new(parking_lot::RwLock::new(crate::config::TuiConfig::default()))
        });
        let old = handle.write().streaming_mode.replace(mode.into());
        Self {
            mode: old,
            view,
            acp,
            folds,
        }
    }
    fn set(mode: &str) {
        atoms::TUI_CONFIG_HANDLE
            .get()
            .unwrap()
            .write()
            .streaming_mode = Some(mode.into());
    }
}
impl Drop for ModeGuard {
    fn drop(&mut self) {
        atoms::TUI_CONFIG_HANDLE
            .get()
            .unwrap()
            .write()
            .streaming_mode = self.mode.take();
        atoms::VIEW_MODELS.set(self.view.clone());
        atoms::ACP_STATE.set(self.acp.clone());
        atoms::FOLD_OVERRIDES.set(self.folds.clone());
    }
}

fn state() -> BridgeState {
    let state = synthetic_scheduler_state();
    atoms::FOLD_OVERRIDES.state().write().clear();
    atoms::VIEW_MODELS.set(Default::default());
    state
}

fn chunk(text: &str, agent: Option<&str>, reasoning: bool) -> AcpEventData {
    if reasoning {
        AcpEventData::ReasoningChunk(TuiReasoningChunk {
            text: text.into(),
            message_id: Some("message".into()),
            agent_id: agent.map(str::to_owned),
        })
    } else {
        AcpEventData::TextChunk(TuiTextChunk {
            text: text.into(),
            message_id: Some("message".into()),
            agent_id: agent.map(str::to_owned),
        })
    }
}

fn deliver(
    state: &mut BridgeState,
    scheduler: &mut PublicationScheduler,
    event: AcpEventData,
    now: peri_time::Instant,
) {
    let intent = acp_events::dispatch_for_bridge(state, &event);
    scheduler.accept_at(intent, state, now);
}

/// 首块立即可见，后续只在 fixed deadline 物化；明确读取不能吞掉发布事实。
#[test]
#[serial]
fn test_publication_main_stream_survives_projection_read() {
    let _mode = ModeGuard::new("streaming");
    for reasoning in [false, true] {
        let mut state = state();
        let mut scheduler = PublicationScheduler::default();
        let now = peri_time::Instant::now();
        reset_perf_counters();
        for _ in 0..100 {
            deliver(
                &mut state,
                &mut scheduler,
                chunk("字", None, reasoning),
                now,
            );
        }
        assert_eq!(state.generation, 1, "首块之外不得逐 chunk 发布");
        assert_eq!(perf_counters().projections, 1, "view_count 不应提前物化");
        assert_eq!(
            state.current_turn.view_model_count(),
            state.current_turn.view_models().len()
        );
        assert!(!state.current_turn.has_unprojected_changes());
        assert!(!scheduler.fire_at(&mut state, now + PUBLICATION_INTERVAL / 2));
        assert!(scheduler.fire_at(&mut state, now + PUBLICATION_INTERVAL));
        assert_eq!(state.generation, 2);
        let snapshot = atoms::VIEW_MODELS.state().read().clone();
        let TuiRenderUnit::TuiAssistantBubble(b) = &snapshot.items[0] else {
            panic!("应为主回复")
        };
        assert_eq!(
            if reasoning {
                &b.reasoning.as_ref().unwrap().text
            } else {
                &b.text
            },
            &"字".repeat(100)
        );
    }
}

/// 子流共享调度窗口；其首块按自己的 occurrence/segment 判定。
#[test]
#[serial]
fn test_publication_subagent_stream_and_block_coalesce() {
    for mode in ["streaming", "block"] {
        let _mode = ModeGuard::new(mode);
        for reasoning in [false, true] {
            let mut state = state();
            state
                .current_turn
                .start_subagent("child".into(), "child".into(), None);
            let mut scheduler = PublicationScheduler::default();
            let now = peri_time::Instant::now();
            reset_perf_counters();
            for _ in 0..100 {
                deliver(
                    &mut state,
                    &mut scheduler,
                    chunk("x", Some("child"), reasoning),
                    now,
                );
            }
            assert_eq!(state.generation, 1);
            let first_projection_count = perf_counters().projections;
            assert_eq!(first_projection_count, 2, "首块仅投影主/子 turn 各一次");
            assert!(scheduler.fire_at(&mut state, now + PUBLICATION_INTERVAL));
            assert_eq!(state.generation, 2);
            let snapshot = atoms::VIEW_MODELS.state().read().clone();
            let TuiRenderUnit::TuiSubAgentGroup(g) = &snapshot.items[0] else {
                panic!("应为子组")
            };
            let TuiRenderUnit::TuiAssistantBubble(b) = &g.view_models[0] else {
                panic!("应为子回复")
            };
            assert_eq!(
                if reasoning {
                    &b.reasoning.as_ref().unwrap().text
                } else {
                    &b.text
                },
                &"x".repeat(100)
            );
        }
    }
}

#[test]
#[serial]
fn test_publication_none_switch_cancels_stream_deadline_but_terminal_flushes() {
    let _mode = ModeGuard::new("streaming");
    let mut state = state();
    let mut scheduler = PublicationScheduler::default();
    let now = peri_time::Instant::now();
    deliver(&mut state, &mut scheduler, chunk("a", None, false), now);
    deliver(&mut state, &mut scheduler, chunk("b", None, false), now);
    ModeGuard::set("none");
    assert!(!scheduler.fire_at(&mut state, now + PUBLICATION_INTERVAL));
    deliver(&mut state, &mut scheduler, chunk("c", None, false), now);
    assert_eq!(state.generation, 1);
    deliver(&mut state, &mut scheduler, AcpEventData::TurnDone, now);
    assert_eq!(
        state.generation, 2,
        "终态只发布一次，不能被 scheduler 重复发布"
    );
    assert!(!scheduler.unpublished);
    assert!(!scheduler.fire_at(&mut state, now + PUBLICATION_INTERVAL * 2));
    let snapshot = atoms::VIEW_MODELS.state().read().clone();
    let TuiRenderUnit::TuiAssistantBubble(b) = &snapshot.items[0] else {
        panic!("应有终态回复")
    };
    assert_eq!(b.text, "abc");
    assert!(!atoms::ACP_STATE.state().read().clone().is_loading);
}

#[test]
#[serial]
fn test_publication_none_resume_and_replay_are_not_lost() {
    let _mode = ModeGuard::new("none");
    let mut state = state();
    let mut scheduler = PublicationScheduler::default();
    let now = peri_time::Instant::now();
    deliver(&mut state, &mut scheduler, chunk("a", None, false), now);
    assert_eq!(state.generation, 0);
    assert_eq!(state.current_turn.view_models().len(), 1);
    ModeGuard::set("streaming");
    deliver(&mut state, &mut scheduler, chunk("b", None, false), now);
    assert_eq!(state.generation, 1, "恢复可见模式时显示已有积累");
    deliver(
        &mut state,
        &mut scheduler,
        AcpEventData::CommittedAssistantText {
            text: "history".into(),
            reasoning: None,
        },
        now,
    );
    ModeGuard::set("none");
    assert!(
        scheduler.fire_at(&mut state, now + PUBLICATION_INTERVAL),
        "历史回放不因 None 丢失"
    );
    assert_eq!(state.generation, 2);
}

#[test]
#[serial]
fn test_publication_receiver_close_flushes_even_after_explicit_projection() {
    let _mode = ModeGuard::new("none");
    let mut state = state();
    let mut scheduler = PublicationScheduler::default();
    deliver(
        &mut state,
        &mut scheduler,
        chunk("pending", None, false),
        peri_time::Instant::now(),
    );
    state.current_turn.view_models();
    let mut reset = atoms::BRIDGE_RESET_COUNTER.get();
    flush_on_receiver_close(&mut state, &mut scheduler, &mut reset);
    assert_eq!(state.generation, 1);
    assert!(!scheduler.unpublished);
}

#[test]
#[serial]
fn test_publication_loading_reset_flushes_and_cancels_deadline() {
    let _mode = ModeGuard::new("streaming");
    let mut state = state();
    let mut scheduler = PublicationScheduler::default();
    let now = peri_time::Instant::now();
    deliver(&mut state, &mut scheduler, chunk("a", None, true), now);
    deliver(&mut state, &mut scheduler, chunk("b", None, true), now);
    deliver(
        &mut state,
        &mut scheduler,
        AcpEventData::LocalLoadingReset,
        now,
    );
    assert_eq!(state.generation, 2);
    assert!(!scheduler.unpublished);
    assert!(!scheduler.fire_at(&mut state, now + PUBLICATION_INTERVAL));
    assert!(!atoms::ACP_STATE.state().read().is_loading);
    let snapshot = atoms::VIEW_MODELS.state().read().clone();
    let TuiRenderUnit::TuiAssistantBubble(b) = &snapshot.items[0] else {
        panic!("应保留取消前已接收的推理")
    };
    let reasoning = b.reasoning.as_ref().unwrap();
    assert_eq!(reasoning.text, "ab");
    assert!(!reasoning.is_running);
    assert!(reasoning.started_at.is_none());
    deliver(
        &mut state,
        &mut scheduler,
        AcpEventData::LocalLoadingReset,
        now,
    );
    assert_eq!(state.generation, 2, "重复复位无副作用");
}

#[test]
#[serial]
fn test_publication_block_and_empty_chunks_respect_boundaries() {
    let _mode = ModeGuard::new("block");
    let mut state = state();
    let mut scheduler = PublicationScheduler::default();
    let now = peri_time::Instant::now();
    deliver(&mut state, &mut scheduler, chunk("", None, false), now);
    assert_eq!(state.generation, 0);
    deliver(&mut state, &mut scheduler, chunk("first", None, false), now);
    deliver(
        &mut state,
        &mut scheduler,
        chunk(" second", None, false),
        now,
    );
    assert_eq!(state.generation, 1);
    assert!(!scheduler.fire_at(&mut state, now + PUBLICATION_INTERVAL));
    deliver(
        &mut state,
        &mut scheduler,
        chunk("\n\nnext", None, false),
        now,
    );
    assert_eq!(state.generation, 2);
    assert_eq!(state.current_turn.text, "first second\n\nnext");
    deliver(
        &mut state,
        &mut scheduler,
        AcpEventData::TextChunk(TuiTextChunk {
            text: "new message".into(),
            message_id: Some("next-message".into()),
            agent_id: None,
        }),
        now,
    );
    assert_eq!(state.generation, 2, "新 message 不绕过主 Block 的边界规则");
    assert!(!scheduler.fire_at(&mut state, now + PUBLICATION_INTERVAL));
}

#[test]
#[serial]
fn test_publication_two_children_resume_and_bg_without_group() {
    let _mode = ModeGuard::new("streaming");
    let mut state = state();
    let mut scheduler = PublicationScheduler::default();
    let now = peri_time::Instant::now();
    for id in ["a", "b"] {
        state
            .current_turn
            .start_subagent(id.into(), id.into(), None);
    }
    for id in ["a", "b"] {
        deliver(
            &mut state,
            &mut scheduler,
            chunk("first", Some(id), false),
            now,
        );
        deliver(
            &mut state,
            &mut scheduler,
            chunk(" tail", Some(id), false),
            now,
        );
    }
    assert_eq!(state.generation, 2);
    state.current_turn.stop_subagent("a", false, "");
    state
        .current_turn
        .start_subagent("a".into(), "resumed".into(), None);
    deliver(
        &mut state,
        &mut scheduler,
        chunk("resumed", Some("a"), false),
        now,
    );
    assert_eq!(state.generation, 3);
    assert_eq!(state.current_turn.subagents.len(), 3);
    atoms::BG_AGENT_IDS
        .state()
        .write()
        .insert("missing-bg".into());
    deliver(
        &mut state,
        &mut scheduler,
        chunk("background", Some("missing-bg"), false),
        now,
    );
    atoms::BG_AGENT_IDS.state().write().remove("missing-bg");
    assert_eq!(state.generation, 3);
    assert!(state.current_turn.text.is_empty());
}

fn replay_card(state: &mut BridgeState, i: usize, ended: bool) {
    acp_events::dispatch_for_bridge(
        state,
        &AcpEventData::ReplayToolStarted {
            tool_id: format!("tool-{i}"),
            tool_name: "Bash".into(),
            input_summary: "synthetic".into(),
            raw_input: serde_json::Value::Null,
        },
    );
    if ended {
        acp_events::dispatch_for_bridge(
            state,
            &AcpEventData::ReplayToolEnded {
                tool_id: format!("tool-{i}"),
                output_summary: "x".repeat(4096),
                is_error: false,
            },
        );
    }
}

#[test]
#[serial]
fn test_publication_stable_replay_history_has_zero_fold_and_hash_work() {
    let _mode = ModeGuard::new("streaming");
    for n in [100, 1000] {
        let mut state = state();
        for i in 0..n {
            replay_card(&mut state, i, true);
        }
        // 用户覆盖也必须复用，而不能只优化默认 fold。
        atoms::FOLD_OVERRIDES
            .state()
            .write()
            .insert(FoldKey::Tool("tool-0".into()), FoldState::Expanded);
        acp_events::push_view_models(&mut state);
        reset_perf_counters();
        for _ in 0..20 {
            acp_events::push_view_models(&mut state);
        }
        let counters = perf_counters();
        assert_eq!(
            (
                counters.fold_pass_writes,
                counters.history_fold_visits,
                counters.tool_hash_calls,
                counters.tool_hash_bytes
            ),
            (0, 0, 0, 0)
        );
        for item in &state.committed {
            let TuiRenderUnit::TuiToolCard(card) = item else {
                panic!("应为工具")
            };
            assert_eq!(card.fold, FoldState::Collapsed);
            assert!(!card.user_modified, "用户覆盖不能污染 canonical");
        }
        atoms::FOLD_OVERRIDES.state().write().clear();
        acp_events::push_view_models(&mut state);
        let cached = atoms::VIEW_MODELS.state().read().clone().items;
        state.folded_history = Default::default();
        acp_events::push_view_models(&mut state);
        assert_eq!(
            cached,
            atoms::VIEW_MODELS.state().read().clone().items,
            "恢复默认后与冷投影逐字段等价"
        );
    }
}

#[test]
#[serial]
fn test_publication_history_cache_invalidates_same_length_update_and_replacement() {
    let _mode = ModeGuard::new("streaming");
    let mut state = state();
    for i in 0..100 {
        replay_card(&mut state, i, false);
    }
    acp_events::push_view_models(&mut state);
    acp_events::dispatch_for_bridge(
        &mut state,
        &AcpEventData::ReplayToolEnded {
            tool_id: "tool-0".into(),
            output_summary: "failed".into(),
            is_error: true,
        },
    );
    acp_events::push_view_models(&mut state);
    let cached = atoms::VIEW_MODELS.state().read().clone().items;
    let TuiRenderUnit::TuiToolCard(card) = &cached[0] else {
        panic!("应为失败工具")
    };
    assert_eq!(card.output_summary, "failed");
    assert_eq!(card.fold, FoldState::Collapsed);
    state.folded_history = Default::default();
    acp_events::push_view_models(&mut state);
    assert_eq!(cached, atoms::VIEW_MODELS.state().read().clone().items);
    state.committed.clear();
    replay_card(&mut state, 0, true);
    acp_events::push_view_models(&mut state);
    assert_eq!(
        atoms::VIEW_MODELS.state().read().clone().items.len(),
        1,
        "缩短并复用 ID 不能复用旧历史"
    );
}

#[test]
#[serial]
fn test_publication_archived_duration_is_stable_across_phase_and_override() {
    let _mode = ModeGuard::new("streaming");
    for end in [
        AcpEventData::TurnDone,
        AcpEventData::TurnSuspended,
        AcpEventData::TurnInterrupted {
            reason: "cancel".into(),
            request_id: None,
        },
    ] {
        let mut state = state();
        let mut scheduler = PublicationScheduler::default();
        let now = peri_time::Instant::now();
        deliver(
            &mut state,
            &mut scheduler,
            chunk("thinking", None, true),
            now,
        );
        deliver(
            &mut state,
            &mut scheduler,
            chunk("answer", None, false),
            now,
        );
        deliver(&mut state, &mut scheduler, end, now);
        let TuiRenderUnit::TuiAssistantBubble(old) = state.committed[0].clone() else {
            panic!("应归档回复")
        };
        assert!(old.started_at.is_none());
        assert!(old.reasoning.as_ref().unwrap().started_at.is_none());
        assert!(!old.reasoning.as_ref().unwrap().is_running);
        state.phase = SessionPhase::PromptRunning;
        atoms::FOLD_OVERRIDES
            .state()
            .write()
            .insert(FoldKey::Reasoning("message".into()), FoldState::Expanded);
        acp_events::push_view_models(&mut state);
        atoms::FOLD_OVERRIDES.state().write().clear();
        state.folded_history = Default::default();
        acp_events::push_view_models(&mut state);
        assert_eq!(
            atoms::VIEW_MODELS.state().read().clone().items[0],
            TuiRenderUnit::TuiAssistantBubble(old)
        );
    }
}
