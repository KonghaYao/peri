use super::*;
use crate::kit::atoms::{BG_LIVE_DETAIL, BG_TASK_IDENTITY, BgTaskIdentity};
use crate::kit::bg_task_live::{append_bg_text_chunk, init_agent_live_detail};
use crate::kit::stream_data::TuiTextChunk;
use crate::kit::tui_render_unit::TuiRenderUnit;

struct Fixture;

impl Fixture {
    fn new() -> Self {
        BG_TASK_IDENTITY.state().write().insert(
            "scheduler-bg-test".into(),
            BgTaskIdentity {
                agent_id: Some("scheduler-bg-agent".into()),
                ..Default::default()
            },
        );
        init_agent_live_detail(
            "scheduler-bg-test",
            "scheduler-bg-agent",
            "coder",
            Some("occurrence"),
        );
        Self
    }

    fn append(&self, text: &str) {
        append_bg_text_chunk(
            "scheduler-bg-agent",
            &TuiTextChunk {
                text: text.into(),
                message_id: None,
                agent_id: Some("scheduler-bg-agent".into()),
            },
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        BG_TASK_IDENTITY.state().write().remove("scheduler-bg-test");
        BG_LIVE_DETAIL.state().write().remove("scheduler-bg-test");
    }
}

#[test]
#[serial_test::serial]
fn background_deadline_does_not_slide_or_publish_hidden_main_turn() {
    let fixture = Fixture::new();
    let mut state = super::tests::scheduler_state();
    state.phase = SessionPhase::Idle;
    state.current_turn.append_text("hidden main text", None);
    let mut scheduler = PublicationScheduler::default();
    let now = peri_time::monotonic_now();
    fixture.append("first");
    scheduler.accept_at(PublicationIntent::Hidden, &mut state, now);
    let deadline = scheduler.bg_pending_deadline.unwrap();
    fixture.append(" second");
    scheduler.accept_at(
        PublicationIntent::None,
        &mut state,
        now + std::time::Duration::from_millis(25),
    );
    assert_eq!(scheduler.bg_pending_deadline, Some(deadline));
    assert!(scheduler.pending_deadline.is_none());
    assert!(!scheduler.fire_at(&mut state, deadline - std::time::Duration::from_millis(1)));
    assert!(scheduler.fire_at(&mut state, deadline));
    assert_eq!(state.generation, 0);
    assert!(scheduler.unpublished);
    assert!(
        matches!(&BG_LIVE_DETAIL.state().read()["scheduler-bg-test"].nested_units[0],
        TuiRenderUnit::TuiAssistantBubble(bubble) if bubble.text == "first second")
    );
}

#[test]
#[serial_test::serial]
fn invalidated_background_timer_cannot_republish_removed_session_detail() {
    let fixture = Fixture::new();
    let mut state = super::tests::scheduler_state();
    let mut scheduler = PublicationScheduler::default();
    let now = peri_time::monotonic_now();
    fixture.append("old session output");
    scheduler.accept_at(PublicationIntent::None, &mut state, now);
    let deadline = scheduler.bg_pending_deadline.unwrap();
    BG_LIVE_DETAIL.state().write().remove("scheduler-bg-test");
    scheduler.invalidate();
    assert!(!scheduler.fire_at(&mut state, deadline));
    assert!(
        !BG_LIVE_DETAIL
            .state()
            .read()
            .contains_key("scheduler-bg-test")
    );
    assert_eq!(state.generation, 0);
}

#[test]
#[serial_test::serial]
fn same_session_reset_rearms_preserved_stream_without_another_event() {
    let fixture = Fixture::new();
    let mut state = super::tests::scheduler_state();
    let mut scheduler = PublicationScheduler::default();
    let now = peri_time::monotonic_now();
    fixture.append("preserved output");
    scheduler.accept_at(PublicationIntent::None, &mut state, now);
    scheduler.invalidate();
    assert!(scheduler.next_deadline().is_none());
    scheduler.schedule_background_at(now);
    let deadline = scheduler.bg_pending_deadline.unwrap();
    assert!(scheduler.fire_at(&mut state, deadline));
    assert!(
        matches!(&BG_LIVE_DETAIL.state().read()["scheduler-bg-test"].nested_units[0],
        TuiRenderUnit::TuiAssistantBubble(bubble) if bubble.text == "preserved output")
    );
    assert_eq!(state.generation, 0);
}

#[test]
#[serial_test::serial]
fn receiver_close_flushes_preserved_background_stream_even_when_reset_is_pending() {
    let fixture = Fixture::new();
    let saved_view = atoms::VIEW_MODELS.state().read().clone();
    let saved_acp = atoms::ACP_STATE.state().read().clone();
    let mut state = super::tests::scheduler_state();
    let mut scheduler = PublicationScheduler::default();
    fixture.append("final background output");
    scheduler.accept(PublicationIntent::None, &mut state);
    let mut last_reset = atoms::BRIDGE_RESET_COUNTER.get().wrapping_sub(1);
    flush_on_receiver_close(&mut state, &mut scheduler, &mut last_reset);
    assert!(
        matches!(&BG_LIVE_DETAIL.state().read()["scheduler-bg-test"].nested_units[0],
        TuiRenderUnit::TuiAssistantBubble(bubble) if bubble.text == "final background output")
    );
    *atoms::VIEW_MODELS.state().write() = saved_view;
    *atoms::ACP_STATE.state().write() = saved_acp;
}
