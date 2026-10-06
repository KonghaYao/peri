use super::*;
use crate::kit::atoms::{BG_TASK_IDENTITY, BgTaskIdentity};

struct Fixture;

impl Fixture {
    fn new() -> Self {
        BG_TASK_IDENTITY.state().write().insert(
            "stream-test".into(),
            BgTaskIdentity {
                agent_id: Some("stream-agent".into()),
                ..Default::default()
            },
        );
        init_agent_live_detail("stream-test", "stream-agent", "coder", Some("occurrence"));
        Self
    }

    fn text(&self, text: &str) {
        append_bg_text_chunk(
            "stream-agent",
            &TuiTextChunk {
                text: text.into(),
                message_id: Some("message".into()),
                agent_id: Some("stream-agent".into()),
            },
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        BG_TASK_IDENTITY.state().write().remove("stream-test");
        BG_LIVE_DETAIL.state().write().remove("stream-test");
    }
}

#[test]
#[serial_test::serial]
fn chunks_accumulate_without_projecting_until_publication() {
    let fixture = Fixture::new();
    crate::kit::acp_bridge::reset_perf_counters();
    for _ in 0..200 {
        fixture.text("中文🙂");
    }
    assert!(
        BG_LIVE_DETAIL.state().read()["stream-test"]
            .nested_units
            .is_empty()
    );
    assert_eq!(
        crate::kit::acp_bridge::perf_counters().assistant_clone_calls,
        0
    );
    publish_pending_streams();
    let live = BG_LIVE_DETAIL.state();
    let map = live.read();
    let detail = &map["stream-test"];
    let TuiRenderUnit::TuiAssistantBubble(bubble) = &detail.nested_units[0] else {
        panic!("expected bubble");
    };
    assert_eq!(bubble.text, "中文🙂".repeat(200));
    assert_eq!(
        crate::kit::acp_bridge::perf_counters().assistant_clone_calls,
        1
    );
    assert_eq!(
        crate::kit::acp_bridge::perf_counters().assistant_clone_bytes,
        bubble.text.len() as u64
    );
    assert_eq!(
        bubble.content_hash,
        TuiAssistantBubble::compute_hash(&bubble.text, None, bubble.duration_secs(), false)
    );
    assert!(!detail.stream.as_ref().unwrap().dirty);
}

#[test]
#[serial_test::serial]
fn later_chunks_do_not_mutate_published_snapshot_and_terminal_flushes() {
    let fixture = Fixture::new();
    fixture.text("first");
    publish_pending_streams();
    let snapshot = BG_LIVE_DETAIL.state().read()["stream-test"]
        .nested_units
        .clone();
    fixture.text(" second");
    append_bg_reasoning_chunk(
        "stream-agent",
        &TuiReasoningChunk {
            text: "reasoning 中文".into(),
            message_id: Some("message".into()),
            agent_id: None,
        },
    );
    handle_bg_subagent_stopped("stream-agent", "done", false);
    let live = BG_LIVE_DETAIL.state();
    let map = live.read();
    let detail = &map["stream-test"];
    let TuiRenderUnit::TuiAssistantBubble(previous) = &snapshot[0] else {
        panic!("expected previous bubble");
    };
    let TuiRenderUnit::TuiAssistantBubble(bubble) = &detail.nested_units[0] else {
        panic!("expected bubble");
    };
    assert_eq!(previous.text, "first");
    assert_eq!(bubble.text, "first second");
    let reasoning = bubble.reasoning.as_ref().unwrap();
    assert_eq!(reasoning.text, "reasoning 中文");
    assert!(!reasoning.is_running);
    assert_eq!(detail.status, BgLiveStatus::Succeeded);
    assert!(detail.stream.is_none());
}

#[test]
#[serial_test::serial]
fn tool_boundary_flushes_text_before_tool_and_starts_new_bubble() {
    let fixture = Fixture::new();
    fixture.text("before");
    handle_bg_tool_started(
        "stream-agent",
        &TuiToolStarted {
            tool_id: "tool".into(),
            tool_name: "Bash".into(),
            input_summary: "echo".into(),
            raw_input: serde_json::json!({}),
            agent_id: None,
        },
        None,
    );
    fixture.text("after");
    publish_pending_streams();
    let live = BG_LIVE_DETAIL.state();
    let map = live.read();
    let units = &map["stream-test"].nested_units;
    assert_eq!(units.len(), 3);
    assert!(
        matches!(&units[0], TuiRenderUnit::TuiAssistantBubble(bubble) if bubble.text == "before")
    );
    assert!(matches!(&units[1], TuiRenderUnit::TuiToolCard(_)));
    assert!(
        matches!(&units[2], TuiRenderUnit::TuiAssistantBubble(bubble) if bubble.text == "after")
    );
}
