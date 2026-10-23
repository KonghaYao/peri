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
fn duplicate_end_and_late_start_preserve_terminal_background_task() {
    let _fixture = Fixture::new();
    let started = TuiToolStarted {
        tool_id: "tool".into(),
        tool_name: "Shell".into(),
        input_summary: "null".into(),
        raw_input: serde_json::Value::Null,
        agent_id: Some("stream-agent".into()),
    };
    handle_bg_tool_started("stream-agent", &started, None);
    handle_bg_tool_ended(
        "stream-agent",
        &TuiToolEnded {
            tool_id: "tool".into(),
            output_summary: "original".into(),
            is_error: false,
            agent_id: Some("stream-agent".into()),
        },
    );
    let duration = BG_LIVE_DETAIL.state().read()["stream-test"].tool_cards[0].completed_duration_ms;
    handle_bg_subagent_stopped("stream-agent", "done", false);
    handle_bg_tool_ended(
        "stream-agent",
        &TuiToolEnded {
            tool_id: "tool".into(),
            output_summary: "duplicate".into(),
            is_error: true,
            agent_id: Some("stream-agent".into()),
        },
    );
    handle_bg_tool_started(
        "stream-agent",
        &TuiToolStarted {
            input_summary: "pwd".into(),
            raw_input: serde_json::json!({"command": "pwd"}),
            ..started.clone()
        },
        None,
    );
    handle_bg_tool_started(
        "stream-agent",
        &TuiToolStarted {
            tool_id: "late".into(),
            ..started
        },
        None,
    );
    let live = BG_LIVE_DETAIL.state();
    let map = live.read();
    let detail = &map["stream-test"];
    assert_eq!(detail.status, BgLiveStatus::Succeeded);
    assert_eq!(
        detail.tool_cards[0].output_summary.as_deref(),
        Some("original")
    );
    assert_eq!(detail.tool_cards[0].completed_duration_ms, duration);
    assert_eq!(detail.tool_cards[0].input_summary, "pwd");
    assert!(!detail.tool_cards[0].is_error);
    assert!(
        detail
            .nested_units
            .iter()
            .all(|unit| matches!(unit, TuiRenderUnit::TuiToolCard(card) if !card.is_running))
    );
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

/// [M07 回归] BgLiveDetail 分层计量：未投影 stream 与 projected nested 不重复，
/// tool_cards 只补充 nested 未持有的 raw_input。
#[test]
#[serial_test::serial]
fn detail_memory_bytes_separates_stream_nested_and_raw_input() {
    let fixture = Fixture::new();
    for _ in 0..10 {
        fixture.text(&"stream-body-".repeat(64));
    }
    let before = BG_LIVE_DETAIL.state().read()["stream-test"].memory_bytes();
    assert!(before.stream_bytes > 0, "未投影 stream 必须计入");
    assert_eq!(before.nested_units_bytes, 0, "未发布时没有嵌套投影");

    publish_pending_streams();
    let projected = BG_LIVE_DETAIL.state().read()["stream-test"].memory_bytes();
    let projected_nested_units = BG_LIVE_DETAIL.state().read()["stream-test"]
        .nested_units
        .len();
    assert_eq!(
        projected.stream_bytes, 0,
        "projected stream 已由 nested_units 持有，不得重复相加"
    );
    assert!(projected.nested_units_bytes > 0);

    handle_bg_tool_started(
        "stream-agent",
        &TuiToolStarted {
            tool_id: "raw-tool".into(),
            tool_name: "Bash".into(),
            input_summary: "large".into(),
            raw_input: serde_json::json!({"command": "x".repeat(64 * 1024)}),
            agent_id: Some("stream-agent".into()),
        },
        None,
    );
    let with_tool = BG_LIVE_DETAIL.state().read()["stream-test"].memory_bytes();
    assert!(
        with_tool.tool_cards_bytes >= 64 * 1024,
        "大 raw_input 必须可区分"
    );

    mark_task_completed("stream-test", true, 5, Some("preview".into()));
    let compaction = compact_terminal_detail("stream-test", false).expect("terminal compaction");
    assert!(!compaction.over_cap);
    let after = BG_LIVE_DETAIL.state().read()["stream-test"].clone();
    assert!(after.tool_cards.is_empty(), "终态累积器必须去重");
    assert_eq!(
        after.nested_units.len(),
        projected_nested_units + 1,
        "无第二持有者时唯一展示正文必须保留"
    );
    assert_eq!(after.memory_bytes().tool_cards_bytes, 0);
    assert_eq!(after.output_preview.as_deref(), Some("preview"));
}

/// [M05 回归] 有第二持有者时终态收缩清空 nested；清空后 capacity 不再计入
/// nested 正文字节（与 memory 字段可区分）。
#[test]
#[serial_test::serial]
fn detail_terminal_compaction_releases_duplicated_body_only() {
    crate::kit::atoms::BG_TASK_IDENTITY.state().write().insert(
        "dup-detail".into(),
        crate::kit::atoms::BgTaskIdentity {
            agent_id: Some("dup-agent".into()),
            ..Default::default()
        },
    );
    init_agent_live_detail("dup-detail", "dup-agent", "coder", Some("dup-occurrence"));
    append_bg_text_chunk(
        "dup-agent",
        &TuiTextChunk {
            text: "duplicated-body-".repeat(128),
            message_id: Some("m".into()),
            agent_id: Some("dup-agent".into()),
        },
    );
    publish_pending_streams();
    handle_bg_tool_started(
        "dup-agent",
        &TuiToolStarted {
            tool_id: "t".into(),
            tool_name: "Read".into(),
            input_summary: "f".into(),
            raw_input: serde_json::json!({"file_path": "/tmp/f"}),
            agent_id: Some("dup-agent".into()),
        },
        None,
    );
    mark_task_completed("dup-detail", true, 3, None);
    let before = BG_LIVE_DETAIL.state().read()["dup-detail"].memory_bytes();
    assert!(before.nested_units_bytes > 0);

    let compaction = compact_terminal_detail("dup-detail", true).expect("terminal compaction");
    assert!(compaction.released_units > 0);
    assert!(compaction.released_body_bytes > 0);
    let after = BG_LIVE_DETAIL.state().read()["dup-detail"].clone();
    assert!(after.terminal_body_released);
    assert!(after.nested_units.is_empty());
    assert!(after.tool_cards.is_empty());
    assert_eq!(after.memory_bytes().nested_units_bytes, 0);
    assert_eq!(after.status, BgLiveStatus::Succeeded);

    BG_TASK_IDENTITY.state().write().remove("dup-detail");
    BG_LIVE_DETAIL.state().write().remove("dup-detail");
}
