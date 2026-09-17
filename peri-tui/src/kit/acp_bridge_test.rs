use super::*;
use crate::kit::acp_types::PendingInteraction;
use peri_acp_types::event_data::{AskUser, HitlPending};
use serial_test::serial;

fn scheduler_state() -> BridgeState {
    crate::kit::atoms::init_atoms();
    BridgeState {
        variant: 0,
        committed: im::Vector::new(),
        current_turn: CurrentTurn::new(),
        phase: SessionPhase::PromptRunning,
        popup_kind: None,
        generation: 0,
        active_session_id: "s1".into(),
        compact_just_completed: false,
        last_submitted_text: None,
        last_pushed_text_len: 0,
        last_pushed_reasoning_len: 0,
        last_successful_todos: None,
        last_successful_todo_sequence: None,
        next_todo_sequence: 0,
        todo_call_inputs: Default::default(),
        turn_generation: 0,
        last_prompt_generation: 0,
        current_request_id: None,
        pending_cache_usage: None,
    }
}

struct ReplayAtomsGuard {
    view: atoms::ViewModelsSnapshot,
    acp: atoms::AcpStateSnapshot,
    steers: crate::kit::steer_state::SteerState,
}

impl ReplayAtomsGuard {
    fn new() -> Self {
        crate::kit::atoms::init_atoms();
        Self {
            view: atoms::VIEW_MODELS.state().read().clone(),
            acp: atoms::ACP_STATE.state().read().clone(),
            steers: crate::kit::steer_state::STEERS.state().read().clone(),
        }
    }
}

impl Drop for ReplayAtomsGuard {
    fn drop(&mut self) {
        *atoms::VIEW_MODELS.state().write() = self.view.clone();
        *atoms::ACP_STATE.state().write() = self.acp.clone();
        *crate::kit::steer_state::STEERS.state().write() = self.steers.clone();
    }
}

/// [回归测试] 历史 assistant 只改 committed；旧 scheduler 只检查 current_turn，
/// 导致最后一个 user 之后的回答永远没有进入 VIEW_MODELS。
#[tokio::test]
#[serial]
async fn test_replay_persisted_history_publishes_final_assistant_in_order() {
    let _restore = ReplayAtomsGuard::new();
    use crate::acp_client::AcpNotification;
    use crate::kit::atoms::VIEW_MODELS;
    use crate::kit::tui_render_unit::TuiRenderUnit;
    use agent_client_protocol_schema::v1::SessionNotification;
    use peri_acp::dispatch::{ReplayError, ReplaySender, replay_persisted_session_history};
    use peri_acp_types::{
        PeriCaps,
        messages::{BaseMessage, ContentBlock, MessageContent},
        store::ThreadStore,
        thread::ThreadMeta,
    };
    use peri_resources::sessions::SqliteThreadStore;
    struct WireSender(mpsc::UnboundedSender<AcpNotification>);
    #[async_trait::async_trait]
    impl ReplaySender for WireSender {
        async fn send(&self, notif: SessionNotification) -> Result<(), ReplayError> {
            self.0
                .send(AcpNotification::SessionUpdate {
                    session_id: notif.session_id.to_string(),
                    params: serde_json::to_value(notif).unwrap(),
                })
                .unwrap();
            Ok(())
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.db");
    let cases = [
        vec![],
        vec![BaseMessage::human("未回答的问题")],
        vec![BaseMessage::ai("只有回答")],
        vec![BaseMessage::human("问题一"), BaseMessage::ai("回答一")],
        vec![
            BaseMessage::human("问题一"),
            BaseMessage::ai("回答一"),
            BaseMessage::human("问题二"),
            BaseMessage::ai(MessageContent::Blocks(vec![
                ContentBlock::Text {
                    text: "最后回答的前半段".into(),
                },
                ContentBlock::Text {
                    text: "最后回答的后半段".into(),
                },
            ])),
        ],
    ];
    for messages in cases {
        let expected: Vec<(bool, String)> = messages
            .iter()
            .flat_map(|message| {
                let content = match message {
                    BaseMessage::Human { content, .. } | BaseMessage::Ai { content, .. } => content,
                    _ => unreachable!(),
                };
                let parts = match content {
                    MessageContent::Text(text) => vec![text.clone()],
                    MessageContent::Blocks(blocks) => blocks
                        .iter()
                        .filter_map(|block| {
                            if let ContentBlock::Text { text } = block {
                                Some(text.clone())
                            } else {
                                None
                            }
                        })
                        .collect(),
                    _ => unreachable!(),
                };
                parts
                    .into_iter()
                    .map(|text| (matches!(message, BaseMessage::Human { .. }), text))
            })
            .collect();
        let store = SqliteThreadStore::new(&path).await.unwrap();
        let id = store
            .create_thread(ThreadMeta::new(dir.path().to_str().unwrap()))
            .await
            .unwrap();
        store.append_messages(&id, &messages).await.unwrap();
        drop(store);
        let reopened = SqliteThreadStore::new(&path).await.unwrap();
        let payloads = reopened.load_context_payloads(&id).await.unwrap();
        assert_eq!(payloads.len(), messages.len(), "磁盘重载不得按角色截断");
        let (notif_tx, notif_rx) = mpsc::unbounded_channel();
        let (bridge_tx, mut bridge_rx) = mpsc::unbounded_channel();
        let shutdown = CancellationToken::new();
        let notifier =
            crate::kit::acp_notifier::spawn_kit_notifier(notif_rx, bridge_tx, shutdown.clone());
        replay_persisted_session_history(
            &id,
            &payloads,
            &WireSender(notif_tx.clone()),
            &PeriCaps {
                replay: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let mut state = scheduler_state();
        state.phase = SessionPhase::Idle;
        state.active_session_id = id;
        *VIEW_MODELS.state().write() = Default::default();
        let mut scheduler = PublicationScheduler::default();
        let now = tokio::time::Instant::now();
        for _ in &expected {
            let event = tokio::time::timeout(std::time::Duration::from_secs(5), bridge_rx.recv())
                .await
                .expect("回放通知应及时到达")
                .unwrap();
            assert_eq!(event.active_session_id, state.active_session_id);
            let intent = acp_events::dispatch_for_bridge(&mut state, &event.event);
            scheduler.accept_at(intent, &mut state, now);
        }
        shutdown.cancel();
        notifier.await.unwrap();
        assert!(
            state.current_turn.is_empty(),
            "回放不应依赖 live turn 的 dirty 标记"
        );
        assert!(!scheduler.fire_at(&mut state, now + PUBLICATION_INTERVAL / 2));
        scheduler.fire_at(&mut state, now + PUBLICATION_INTERVAL);
        let snapshot = VIEW_MODELS.state().read().clone();
        let actual: Vec<_> = snapshot
            .items
            .iter()
            .map(|item| match item {
                TuiRenderUnit::TuiUserBubble(bubble) => (true, bubble.text.clone()),
                TuiRenderUnit::TuiAssistantBubble(bubble) => (false, bubble.text.clone()),
                other => panic!("非预期回放条目：{other:?}"),
            })
            .collect();
        assert_eq!(
            actual, expected,
            "恢复须按顺序发布全部内容，包括最后 assistant"
        );
        let generation = state.generation;
        assert!(!scheduler.fire_at(&mut state, now + PUBLICATION_INTERVAL * 2));
        assert_eq!(state.generation, generation, "完成后不得重复发布");
    }
}

/// [回归测试] channel 关闭发生在合帧 deadline 前时，committed 尾部也必须刷新。
#[test]
#[serial]
fn test_replay_receiver_close_publishes_committed_tail() {
    let _restore = ReplayAtomsGuard::new();
    use crate::kit::atoms::{BRIDGE_RESET_COUNTER, VIEW_MODELS};
    let mut state = scheduler_state();
    *VIEW_MODELS.state().write() = Default::default();
    let mut scheduler = PublicationScheduler::default();
    let intent = acp_events::dispatch_for_bridge(
        &mut state,
        &AcpEventData::CommittedAssistantText {
            text: "连接关闭前的最终回答".into(),
            reasoning: None,
        },
    );
    scheduler.accept(intent, &mut state);
    let mut last_reset = BRIDGE_RESET_COUNTER.get();
    flush_on_receiver_close(&mut state, &mut scheduler, &mut last_reset);
    assert_eq!(VIEW_MODELS.state().read().items.len(), 1);
    assert!(scheduler.pending_deadline.is_none());
}

/// [回归测试] 已发布工具卡的完成更新不改变 committed 长度，仍须发布新内容。
#[test]
#[serial]
fn test_replay_tool_completion_publishes_same_length_update() {
    let _restore = ReplayAtomsGuard::new();
    use crate::kit::atoms::VIEW_MODELS;
    use crate::kit::tui_render_unit::TuiRenderUnit;
    let mut state = scheduler_state();
    let mut scheduler = PublicationScheduler::default();
    let now = tokio::time::Instant::now();
    let intent = acp_events::dispatch_for_bridge(
        &mut state,
        &AcpEventData::ReplayToolStarted {
            tool_id: "tool".into(),
            tool_name: "Read".into(),
            input_summary: "合成文件".into(),
            raw_input: serde_json::json!({}),
        },
    );
    scheduler.accept_at(intent, &mut state, now);
    assert!(scheduler.fire_at(&mut state, now + PUBLICATION_INTERVAL));
    let intent = acp_events::dispatch_for_bridge(
        &mut state,
        &AcpEventData::ReplayToolEnded {
            tool_id: "tool".into(),
            output_summary: "合成失败结果".into(),
            is_error: true,
        },
    );
    scheduler.accept_at(intent, &mut state, now + PUBLICATION_INTERVAL);
    assert!(scheduler.fire_at(&mut state, now + PUBLICATION_INTERVAL * 2));
    let snapshot = VIEW_MODELS.state().read().clone();
    assert_eq!(snapshot.items.len(), 1);
    let TuiRenderUnit::TuiToolCard(card) = &snapshot.items[0] else {
        panic!("应保留工具卡")
    };
    assert!(!card.is_running);
    assert!(card.is_error);
    assert_eq!(card.output_summary, "合成失败结果");
}

#[test]
#[serial]
fn test_production_scheduler_uses_fixed_deadline_and_terminal_invalidates_it() {
    use crate::kit::atoms::VIEW_MODELS;

    *VIEW_MODELS.state().write() = Default::default();
    let mut state = scheduler_state();
    let mut scheduler = PublicationScheduler::default();

    state.current_turn.append_text("first", Some("m1"));
    scheduler.accept(PublicationIntent::Immediate, &mut state);
    assert_eq!(state.generation, 1);

    let now = tokio::time::Instant::now();
    state.current_turn.append_text(" second", Some("m1"));
    scheduler.accept_at(PublicationIntent::Deferred, &mut state, now);
    let fixed_deadline = scheduler.pending_deadline.expect("deadline scheduled");
    state.current_turn.append_text(" third", Some("m1"));
    scheduler.accept_at(
        PublicationIntent::Deferred,
        &mut state,
        now + std::time::Duration::from_millis(25),
    );
    assert_eq!(scheduler.pending_deadline, Some(fixed_deadline));

    assert!(!scheduler.fire_at(&mut state, now + std::time::Duration::from_millis(49)));
    assert_eq!(state.generation, 1);
    assert!(scheduler.fire_at(&mut state, fixed_deadline));
    assert_eq!(state.generation, 2);
    assert_eq!(state.current_turn.text, "first second third");

    state.current_turn.append_text(" final", Some("m1"));
    scheduler.accept(PublicationIntent::Deferred, &mut state);
    scheduler.accept(PublicationIntent::Immediate, &mut state);
    assert!(scheduler.pending_deadline.is_none());
    let terminal_generation = state.generation;
    assert_eq!(state.generation, terminal_generation);
}

#[test]
#[serial]
fn test_production_scheduler_lifecycle_invalidation_matrix_is_stale_noop() {
    use crate::kit::atoms::VIEW_MODELS;

    for lifecycle in ["terminal", "reset", "session", "shutdown"] {
        *VIEW_MODELS.state().write() = Default::default();
        let mut state = scheduler_state();
        state.current_turn.append_text("pending", Some("m1"));
        let mut scheduler = PublicationScheduler::default();
        let now = tokio::time::Instant::now();
        scheduler.accept_at(PublicationIntent::Deferred, &mut state, now);
        let stale_deadline = scheduler.pending_deadline.unwrap();

        match lifecycle {
            "terminal" => scheduler.accept_at(PublicationIntent::Immediate, &mut state, now),
            "reset" | "session" | "shutdown" => scheduler.invalidate(),
            _ => unreachable!(),
        }
        let generation = state.generation;
        assert!(!scheduler.fire_at(&mut state, stale_deadline));
        assert_eq!(state.generation, generation, "lifecycle={lifecycle}");
    }
}

#[test]
#[serial]
fn test_receiver_close_reset_wins_over_dirty_final_publication() {
    use crate::kit::atoms::{ACTIVE_SESSION_ID, BRIDGE_RESET_COUNTER, VIEW_MODELS};

    let old_reset = BRIDGE_RESET_COUNTER.get();
    *ACTIVE_SESSION_ID.state().write() = "s2".into();
    *VIEW_MODELS.state().write() = Default::default();
    let mut state = scheduler_state();
    state.current_turn.append_text("stale", Some("m1"));
    let mut last_reset = old_reset;
    let new_reset = old_reset.wrapping_add(1);
    BRIDGE_RESET_COUNTER.set(new_reset);

    let mut scheduler = PublicationScheduler::default();
    scheduler.accept_at(
        PublicationIntent::Deferred,
        &mut state,
        tokio::time::Instant::now(),
    );
    flush_on_receiver_close(&mut state, &mut scheduler, &mut last_reset);

    assert!(scheduler.pending_deadline.is_none());
    assert_eq!(state.active_session_id, "s2");
    assert!(VIEW_MODELS.state().read().items.is_empty());
    BRIDGE_RESET_COUNTER.set(old_reset);
}

/// [回归测试] reset 被 tick 先观察时，manual compact 的 UI-only 完成提示仍
/// 必须进入同一 session 的 replay 快照。
#[test]
#[serial]
fn test_bridge_reset_rehydrates_pending_compact_note_for_same_session() {
    use crate::kit::atoms::{
        ACP_STATE, ACTIVE_SESSION_ID, BRIDGE_RESET_COUNTER, FOCUSED_ENTRY, FOLD_OVERRIDES,
        INPUT_BUFFER, PENDING_COMPACT_NOTE, VIEW_MODELS,
    };
    use crate::kit::tui_render_unit::TuiRenderUnit;

    let old_active = ACTIVE_SESSION_ID.state().read().clone();
    let old_note = PENDING_COMPACT_NOTE.state().read().clone();
    let old_reset = BRIDGE_RESET_COUNTER.get();
    let old_acp_state = ACP_STATE.state().read().clone();
    let old_input = INPUT_BUFFER.state().read().clone();
    let old_fold_overrides = FOLD_OVERRIDES.state().read().clone();
    let old_focused_entry = FOCUSED_ENTRY.state().read().clone();
    let old_view = VIEW_MODELS.state().read().clone();
    *ACTIVE_SESSION_ID.state().write() = "s1".into();
    PENDING_COMPACT_NOTE.set(Some("compact complete".into()));
    let mut state = scheduler_state();
    let mut last_reset = old_reset;

    apply_bridge_reset(&mut state, &mut last_reset, old_reset.wrapping_add(1));

    assert!(matches!(
        state.current_turn.view_models().iter().next(),
        Some(TuiRenderUnit::TuiSystemNote(note)) if note.text == "compact complete"
    ));
    assert!(PENDING_COMPACT_NOTE.state().read().is_none());
    assert!(!crate::kit::atoms::ACP_STATE.state().read().is_loading);

    *ACTIVE_SESSION_ID.state().write() = old_active;
    *PENDING_COMPACT_NOTE.state().write() = old_note;
    BRIDGE_RESET_COUNTER.set(old_reset);
    *ACP_STATE.state().write() = old_acp_state;
    *INPUT_BUFFER.state().write() = old_input;
    *FOLD_OVERRIDES.state().write() = old_fold_overrides;
    *FOCUSED_ENTRY.state().write() = old_focused_entry;
    *VIEW_MODELS.state().write() = old_view;
}

/// 普通 thread 切换不能把旧 session 的 compact 完成提示带到新 session。
#[test]
#[serial]
fn test_bridge_reset_discards_pending_compact_note_on_session_switch() {
    use crate::kit::atoms::{
        ACP_STATE, ACTIVE_SESSION_ID, BRIDGE_RESET_COUNTER, FOCUSED_ENTRY, FOLD_OVERRIDES,
        INPUT_BUFFER, PENDING_COMPACT_NOTE, VIEW_MODELS,
    };

    let old_active = ACTIVE_SESSION_ID.state().read().clone();
    let old_note = PENDING_COMPACT_NOTE.state().read().clone();
    let old_reset = BRIDGE_RESET_COUNTER.get();
    let old_acp_state = ACP_STATE.state().read().clone();
    let old_input = INPUT_BUFFER.state().read().clone();
    let old_fold_overrides = FOLD_OVERRIDES.state().read().clone();
    let old_focused_entry = FOCUSED_ENTRY.state().read().clone();
    let old_view = VIEW_MODELS.state().read().clone();
    *ACTIVE_SESSION_ID.state().write() = "s2".into();
    PENDING_COMPACT_NOTE.set(Some("old compact complete".into()));
    let mut state = scheduler_state();
    let mut last_reset = old_reset;

    apply_bridge_reset(&mut state, &mut last_reset, old_reset.wrapping_add(1));

    assert!(state.current_turn.view_models().is_empty());
    assert!(PENDING_COMPACT_NOTE.state().read().is_none());

    *ACTIVE_SESSION_ID.state().write() = old_active;
    *PENDING_COMPACT_NOTE.state().write() = old_note;
    BRIDGE_RESET_COUNTER.set(old_reset);
    *ACP_STATE.state().write() = old_acp_state;
    *INPUT_BUFFER.state().write() = old_input;
    *FOLD_OVERRIDES.state().write() = old_fold_overrides;
    *FOCUSED_ENTRY.state().write() = old_focused_entry;
    *VIEW_MODELS.state().write() = old_view;
}

/// replay 的 user/assistant 事件是历史投影，不能重新把 reset 后的 bridge
/// 置回 loading；终态应由 reset 的 Idle 保持。
#[test]
#[serial]
fn test_compact_replay_events_keep_bridge_idle_after_reset() {
    use crate::kit::atoms::{
        ACP_STATE, ACTIVE_SESSION_ID, BRIDGE_RESET_COUNTER, FOCUSED_ENTRY, FOLD_OVERRIDES,
        INPUT_BUFFER, PENDING_COMPACT_NOTE, VIEW_MODELS,
    };

    let old_active = ACTIVE_SESSION_ID.state().read().clone();
    let old_note = PENDING_COMPACT_NOTE.state().read().clone();
    let old_reset = BRIDGE_RESET_COUNTER.get();
    let old_acp_state = ACP_STATE.state().read().clone();
    let old_input = INPUT_BUFFER.state().read().clone();
    let old_fold_overrides = FOLD_OVERRIDES.state().read().clone();
    let old_focused_entry = FOCUSED_ENTRY.state().read().clone();
    let old_view = VIEW_MODELS.state().read().clone();
    *ACTIVE_SESSION_ID.state().write() = "s1".into();
    let mut state = scheduler_state();
    let mut last_reset = old_reset;
    apply_bridge_reset(&mut state, &mut last_reset, old_reset.wrapping_add(1));

    acp_events::dispatch_for_bridge(
        &mut state,
        &AcpEventData::ReplayedUserBubble {
            input_id: "replay-user".into(),
            text: "old prompt".into(),
        },
    );
    acp_events::dispatch_for_bridge(
        &mut state,
        &AcpEventData::CommittedAssistantText {
            text: "old answer".into(),
            reasoning: None,
        },
    );

    assert_eq!(state.phase, SessionPhase::Idle);
    assert!(!crate::kit::atoms::ACP_STATE.state().read().is_loading);

    *ACTIVE_SESSION_ID.state().write() = old_active;
    *PENDING_COMPACT_NOTE.state().write() = old_note;
    BRIDGE_RESET_COUNTER.set(old_reset);
    *ACP_STATE.state().write() = old_acp_state;
    *INPUT_BUFFER.state().write() = old_input;
    *FOLD_OVERRIDES.state().write() = old_fold_overrides;
    *FOCUSED_ENTRY.state().write() = old_focused_entry;
    *VIEW_MODELS.state().write() = old_view;
}

#[test]
fn test_deterministic_clock_advances_without_sleep() {
    let mut clock = DeterministicClock::default();
    clock.advance_ms(50);
    assert_eq!(clock.now_ms(), 50);
}

#[test]
fn test_publication_observer_records_metadata_without_content() {
    reset_perf_counters();
    let observations = [
        PublicationObservation {
            generation: 7,
            source_version: 11,
            reason: PublicationReason::Intermediate,
        },
        PublicationObservation {
            generation: 8,
            source_version: 12,
            reason: PublicationReason::Terminal,
        },
        PublicationObservation {
            generation: 0,
            source_version: 13,
            reason: PublicationReason::Reset,
        },
    ];
    for observation in observations {
        observe_publication(observation);
    }
    let counters = perf_counters();
    assert_eq!(
        (
            observations,
            counters.intermediate_publications,
            counters.terminal_publications,
            counters.reset_publications,
        ),
        (observations, 1, 1, 1)
    );
}

fn hitl() -> AcpEventData {
    AcpEventData::HitlPending(PendingInteraction {
        owner: Default::default(),
        request_id_json: "\"h\"".into(),
        payload: HitlPending {
            tool_name: "Bash".into(),
            tool_input: serde_json::Value::Null,
            batch: None,
        },
    })
}

fn ask_user() -> AcpEventData {
    AcpEventData::AskUser(PendingInteraction {
        owner: Default::default(),
        request_id_json: "\"a\"".into(),
        payload: AskUser { questions: vec![] },
    })
}

/// [回归测试] 入站 interaction 只接受非空且精确匹配的 active session。
#[test]
fn test_interaction_gate_requires_nonempty_exact_active_session() {
    for event in [hitl(), ask_user()] {
        assert!(accepts_event_session(&event, "s1", "s1", false));
        assert!(!accepts_event_session(&event, "", "s1", false));
        assert!(!accepts_event_session(&event, "s1", "", false));
        assert!(!accepts_event_session(&event, "s1", "s2", false));
        assert!(accepts_event_session(&event, "s1", "s1", true));
    }
}

#[test]
fn test_ordinary_gate_preserves_nonreset_wildcards() {
    let event = AcpEventData::InteractionTerminal {
        owner: Default::default(),
        outcome: crate::acp_client::InteractionUiOutcome::Resolved {
            result: "done".into(),
        },
    };
    assert!(accepts_event_session(&event, "", "s1", false));
    assert!(accepts_event_session(&event, "s1", "", false));
    assert!(accepts_event_session(&event, "s1", "s1", false));
    assert!(!accepts_event_session(&event, "s1", "s2", false));
}

#[test]
fn test_ordinary_gate_preserves_just_reset_rules() {
    let event = AcpEventData::PromptStarted;
    assert!(accepts_event_session(&event, "s1", "", true));
    assert!(accepts_event_session(&event, "s1", "s1", true));
    assert!(!accepts_event_session(&event, "", "s1", true));
    assert!(!accepts_event_session(&event, "s1", "s2", true));
}

fn goal_snapshot(objective: &str, continuation_count: u64) -> AcpEventData {
    AcpEventData::GoalSnapshot {
        objective: Some(objective.into()),
        status: Some(peri_acp_types::goal::GoalStatus::Active),
        token_budget: None,
        tokens_used: 0,
        time_used_seconds: 0,
        continuation_count,
        blocked_reason: None,
    }
}

/// Goal 投影必须服从普通事件的 session ownership gate。
#[tokio::test]
#[serial]
async fn test_goal_snapshot_bridge_accepts_current_session_and_drops_stale_session() {
    use crate::kit::atoms::{ACTIVE_SESSION_ID, BRIDGE_RESET_COUNTER, GOAL_SNAPSHOT};

    let old_active = ACTIVE_SESSION_ID.state().read().clone();
    let old_reset = BRIDGE_RESET_COUNTER.get();
    let old_goal = GOAL_SNAPSHOT.state().read().clone();
    *ACTIVE_SESSION_ID.state().write() = "s1".into();
    *GOAL_SNAPSHOT.state().write() = None;
    BRIDGE_RESET_COUNTER.set(old_reset.wrapping_add(1));

    let (tx, rx) = mpsc::unbounded_channel();
    let (observed_tx, mut observed_rx) = mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    let handle = spawn_acp_bridge_observed(rx, shutdown.clone(), observed_tx);

    tx.send(AcpEventWithEpoch {
        event: goal_snapshot("current", 2),
        active_session_id: "s1".into(),
    })
    .unwrap();
    assert_eq!(observed_rx.recv().await, Some(true));
    assert_eq!(
        GOAL_SNAPSHOT
            .state()
            .read()
            .as_ref()
            .and_then(|goal| goal.objective.as_deref()),
        Some("current")
    );

    tx.send(AcpEventWithEpoch {
        event: goal_snapshot("stale", 99),
        active_session_id: "s0".into(),
    })
    .unwrap();
    assert_eq!(observed_rx.recv().await, Some(false));
    let projected = GOAL_SNAPSHOT.state().read().clone().unwrap();
    assert_eq!(projected.objective.as_deref(), Some("current"));
    assert_eq!(projected.continuation_count, 2);

    shutdown.cancel();
    drop(tx);
    handle.await.unwrap();
    *ACTIVE_SESSION_ID.state().write() = old_active;
    BRIDGE_RESET_COUNTER.set(old_reset);
    *GOAL_SNAPSHOT.state().write() = old_goal;
}

/// [回归测试] production bridge 在 session gate 前不发布 HITL UI state。
#[tokio::test]
#[serial]
async fn test_hitl_bridge_drops_unowned_events_before_all_side_effects() {
    use crate::kit::atoms::{
        ACP_STATE, ACTIVE_SESSION_ID, BRIDGE_RESET_COUNTER, FOCUSED_ENTRY, FOLD_OVERRIDES,
        HITL_PENDING, INPUT_BUFFER, PENDING_COMPACT_NOTE, POPUP_KIND, PopupKind, VIEW_MODELS,
    };
    let old_acp_state = ACP_STATE.state().read().clone();
    let old_active = ACTIVE_SESSION_ID.state().read().clone();
    let old_reset = BRIDGE_RESET_COUNTER.get();
    let old_input = INPUT_BUFFER.state().read().clone();
    let old_fold_overrides = FOLD_OVERRIDES.state().read().clone();
    let old_focused_entry = FOCUSED_ENTRY.state().read().clone();
    let old_compact_note = PENDING_COMPACT_NOTE.state().read().clone();
    let old_pending = HITL_PENDING.state().read().clone();
    let old_popup = *POPUP_KIND.state().read();
    let old_view = VIEW_MODELS.state().read().clone();
    *ACTIVE_SESSION_ID.state().write() = String::new();
    BRIDGE_RESET_COUNTER.set(old_reset.wrapping_add(1));
    let (tx, rx) = mpsc::unbounded_channel();
    let (observed_tx, mut observed_rx) = mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    let handle = spawn_acp_bridge_observed(rx, shutdown.clone(), observed_tx);
    tx.send(AcpEventWithEpoch {
        event: AcpEventData::PromptStarted,
        active_session_id: String::new(),
    })
    .unwrap();
    assert_eq!(observed_rx.recv().await, Some(true));
    *HITL_PENDING.state().write() = Some(PendingInteraction {
        owner: Default::default(),
        request_id_json: "\"sentinel\"".into(),
        payload: HitlPending {
            tool_name: "sentinel".into(),
            tool_input: serde_json::Value::Null,
            batch: None,
        },
    });
    *POPUP_KIND.state().write() = Some(PopupKind::OAuth);
    let sentinel_view = VIEW_MODELS.state().read().clone();
    tx.send(AcpEventWithEpoch {
        event: hitl(),
        active_session_id: "s1".into(),
    })
    .unwrap();
    assert_eq!(observed_rx.recv().await, Some(false));
    assert_eq!(
        HITL_PENDING
            .state()
            .read()
            .as_ref()
            .unwrap()
            .request_id_json,
        "\"sentinel\""
    );
    assert_eq!(*POPUP_KIND.state().read(), Some(PopupKind::OAuth));
    assert_eq!(
        VIEW_MODELS.state().read().items.len(),
        sentinel_view.items.len()
    );
    *ACTIVE_SESSION_ID.state().write() = "s1".into();
    BRIDGE_RESET_COUNTER.set(old_reset.wrapping_add(2));
    tx.send(AcpEventWithEpoch {
        event: AcpEventData::PromptStarted,
        active_session_id: "s1".into(),
    })
    .unwrap();
    assert_eq!(observed_rx.recv().await, Some(true));
    *HITL_PENDING.state().write() = Some(PendingInteraction {
        owner: Default::default(),
        request_id_json: "\"sentinel\"".into(),
        payload: HitlPending {
            tool_name: "sentinel".into(),
            tool_input: serde_json::Value::Null,
            batch: None,
        },
    });
    *POPUP_KIND.state().write() = Some(PopupKind::OAuth);
    tx.send(AcpEventWithEpoch {
        event: hitl(),
        active_session_id: "stale".into(),
    })
    .unwrap();
    assert_eq!(observed_rx.recv().await, Some(false));
    assert_eq!(
        HITL_PENDING
            .state()
            .read()
            .as_ref()
            .unwrap()
            .request_id_json,
        "\"sentinel\""
    );
    assert_eq!(*POPUP_KIND.state().read(), Some(PopupKind::OAuth));
    tx.send(AcpEventWithEpoch {
        event: hitl(),
        active_session_id: "s1".into(),
    })
    .unwrap();
    assert_eq!(observed_rx.recv().await, Some(true));
    assert_eq!(
        HITL_PENDING
            .state()
            .read()
            .as_ref()
            .unwrap()
            .request_id_json,
        "\"h\""
    );
    assert_eq!(*POPUP_KIND.state().read(), Some(PopupKind::Hitl));
    assert!(VIEW_MODELS.state().read().items.iter().any(|vm| matches!(vm, crate::kit::tui_render_unit::TuiRenderUnit::TuiAskUserBlock(block) if block.request_id.as_deref() == Some("\"h\""))));
    shutdown.cancel();
    drop(tx);
    handle.await.unwrap();
    *ACP_STATE.state().write() = old_acp_state;
    *ACTIVE_SESSION_ID.state().write() = old_active;
    BRIDGE_RESET_COUNTER.set(old_reset);
    *INPUT_BUFFER.state().write() = old_input;
    *FOLD_OVERRIDES.state().write() = old_fold_overrides;
    *FOCUSED_ENTRY.state().write() = old_focused_entry;
    *PENDING_COMPACT_NOTE.state().write() = old_compact_note;
    *HITL_PENDING.state().write() = old_pending;
    *POPUP_KIND.state().write() = old_popup;
    *VIEW_MODELS.state().write() = old_view;
}

/// [回归测试] production bridge 在 session gate 前不发布 AskUser UI state。
#[tokio::test]
#[serial]
async fn test_ask_user_bridge_drops_unowned_events_before_all_side_effects() {
    use crate::app::panel_types::PanelKind;
    use crate::kit::atoms::{
        ACP_STATE, ACTIVE_PANEL, ACTIVE_SESSION_ID, ASK_USER_PENDING, BRIDGE_RESET_COUNTER,
        FOCUSED_ENTRY, FOLD_OVERRIDES, INPUT_BUFFER, OPEN_PANELS, PENDING_COMPACT_NOTE,
        VIEW_MODELS,
    };
    let old_acp_state = ACP_STATE.state().read().clone();
    let old_active_session = ACTIVE_SESSION_ID.state().read().clone();
    let old_reset = BRIDGE_RESET_COUNTER.get();
    let old_input = INPUT_BUFFER.state().read().clone();
    let old_fold_overrides = FOLD_OVERRIDES.state().read().clone();
    let old_focused_entry = FOCUSED_ENTRY.state().read().clone();
    let old_compact_note = PENDING_COMPACT_NOTE.state().read().clone();
    let old_pending = ASK_USER_PENDING.state().read().clone();
    let old_active_panel = *ACTIVE_PANEL.state().read();
    let old_open = OPEN_PANELS.state().read().clone();
    let old_view = VIEW_MODELS.state().read().clone();
    *ACTIVE_SESSION_ID.state().write() = String::new();
    BRIDGE_RESET_COUNTER.set(old_reset.wrapping_add(1));
    let (tx, rx) = mpsc::unbounded_channel();
    let (observed_tx, mut observed_rx) = mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    let handle = spawn_acp_bridge_observed(rx, shutdown.clone(), observed_tx);
    tx.send(AcpEventWithEpoch {
        event: AcpEventData::PromptStarted,
        active_session_id: String::new(),
    })
    .unwrap();
    assert_eq!(observed_rx.recv().await, Some(true));
    *ASK_USER_PENDING.state().write() = Some(PendingInteraction {
        owner: Default::default(),
        request_id_json: "\"sentinel\"".into(),
        payload: AskUser { questions: vec![] },
    });
    *OPEN_PANELS.state().write() = vec![PanelKind::Tasks];
    *ACTIVE_PANEL.state().write() = Some(PanelKind::Tasks);
    let sentinel_view = VIEW_MODELS.state().read().clone();
    tx.send(AcpEventWithEpoch {
        event: ask_user(),
        active_session_id: "s1".into(),
    })
    .unwrap();
    assert_eq!(observed_rx.recv().await, Some(false));
    assert_eq!(
        ASK_USER_PENDING
            .state()
            .read()
            .as_ref()
            .unwrap()
            .request_id_json,
        "\"sentinel\""
    );
    assert_eq!(*ACTIVE_PANEL.state().read(), Some(PanelKind::Tasks));
    assert_eq!(
        VIEW_MODELS.state().read().items.len(),
        sentinel_view.items.len()
    );
    *ACTIVE_SESSION_ID.state().write() = "s1".into();
    BRIDGE_RESET_COUNTER.set(old_reset.wrapping_add(2));
    tx.send(AcpEventWithEpoch {
        event: AcpEventData::PromptStarted,
        active_session_id: "s1".into(),
    })
    .unwrap();
    assert_eq!(observed_rx.recv().await, Some(true));
    *ASK_USER_PENDING.state().write() = Some(PendingInteraction {
        owner: Default::default(),
        request_id_json: "\"sentinel\"".into(),
        payload: AskUser { questions: vec![] },
    });
    *OPEN_PANELS.state().write() = vec![PanelKind::Tasks];
    *ACTIVE_PANEL.state().write() = Some(PanelKind::Tasks);
    tx.send(AcpEventWithEpoch {
        event: ask_user(),
        active_session_id: "stale".into(),
    })
    .unwrap();
    assert_eq!(observed_rx.recv().await, Some(false));
    assert_eq!(
        ASK_USER_PENDING
            .state()
            .read()
            .as_ref()
            .unwrap()
            .request_id_json,
        "\"sentinel\""
    );
    assert_eq!(*ACTIVE_PANEL.state().read(), Some(PanelKind::Tasks));
    tx.send(AcpEventWithEpoch {
        event: ask_user(),
        active_session_id: "s1".into(),
    })
    .unwrap();
    assert_eq!(observed_rx.recv().await, Some(true));
    assert_eq!(
        ASK_USER_PENDING
            .state()
            .read()
            .as_ref()
            .unwrap()
            .request_id_json,
        "\"a\""
    );
    assert_eq!(*ACTIVE_PANEL.state().read(), Some(PanelKind::AskUser));
    shutdown.cancel();
    drop(tx);
    handle.await.unwrap();
    *ACP_STATE.state().write() = old_acp_state;
    *ACTIVE_SESSION_ID.state().write() = old_active_session;
    BRIDGE_RESET_COUNTER.set(old_reset);
    *INPUT_BUFFER.state().write() = old_input;
    *FOLD_OVERRIDES.state().write() = old_fold_overrides;
    *FOCUSED_ENTRY.state().write() = old_focused_entry;
    *PENDING_COMPACT_NOTE.state().write() = old_compact_note;
    *ASK_USER_PENDING.state().write() = old_pending;
    *ACTIVE_PANEL.state().write() = old_active_panel;
    *OPEN_PANELS.state().write() = old_open;
    *VIEW_MODELS.state().write() = old_view;
}
