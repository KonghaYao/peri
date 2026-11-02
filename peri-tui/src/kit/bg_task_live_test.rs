use super::*;
use crate::kit::atoms::{BG_TASK_IDENTITY, BgTaskIdentity};

struct Fixture;

struct RestoreLive(std::collections::HashMap<String, BgLiveDetail>);

impl RestoreLive {
    fn empty() -> Self {
        Self(std::mem::take(&mut *BG_LIVE_DETAIL.state().write()))
    }
}

impl Drop for RestoreLive {
    fn drop(&mut self) {
        *BG_LIVE_DETAIL.state().write() = std::mem::take(&mut self.0);
    }
}

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
        origin_message_id: None,
        origin_readable: None,
    };
    handle_bg_tool_started("stream-agent", &started, None, false);
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
        false,
    );
    handle_bg_tool_started(
        "stream-agent",
        &TuiToolStarted {
            tool_id: "late".into(),
            ..started
        },
        None,
        false,
    );
    let live = BG_LIVE_DETAIL.state();
    let map = live.read();
    let detail = &map["stream-test"];
    assert_eq!(detail.status, BgLiveStatus::Succeeded);
    assert!(
        detail.tool_cards.is_empty(),
        "终态必须释放工具累积器及 raw_input"
    );
    let TuiRenderUnit::TuiToolCard(card) = &detail.nested_units[0] else {
        panic!("expected terminal tool card");
    };
    assert_eq!(card.output_summary, "original");
    assert_eq!(card.completed_duration_ms, duration);
    assert_eq!(
        card.input_summary, "null",
        "终态后的开始事件不能重建工具正文"
    );
    assert!(!card.is_error);
    assert_eq!(detail.nested_units.len(), 1);
    assert!(
        detail
            .nested_units
            .iter()
            .all(|unit| matches!(unit, TuiRenderUnit::TuiToolCard(card) if !card.is_running))
    );
}

/// [回归测试] 正常 Stopped 后的 Completed/快照只更新元数据，不 COW 重建工具正文。
#[test]
#[serial_test::serial]
fn terminal_metadata_updates_do_not_rematerialize_published_tool_bodies() {
    let _fixture = Fixture::new();
    handle_bg_tool_started(
        "stream-agent",
        &TuiToolStarted {
            tool_id: "stable-tool".into(),
            tool_name: "Read".into(),
            input_summary: "input".into(),
            raw_input: serde_json::Value::Null,
            agent_id: None,
            origin_message_id: None,
            origin_readable: None,
        },
        None,
        false,
    );
    handle_bg_tool_ended(
        "stream-agent",
        &TuiToolEnded {
            tool_id: "stable-tool".into(),
            output_summary: "retained tool result".into(),
            is_error: false,
            agent_id: None,
        },
    );
    handle_bg_subagent_stopped("stream-agent", "done", false);
    let snapshot = BG_LIVE_DETAIL.state().read()["stream-test"]
        .nested_units
        .clone();
    let TuiRenderUnit::TuiToolCard(previous) = &snapshot[0] else {
        unreachable!()
    };
    let body = previous.output_summary.as_ptr();
    mark_task_completed("stream-test", true, 1, Some("done".into()));
    seed_live_from_terminal_snapshot("stream-test", "agent", "summary", None, "completed");
    let live = BG_LIVE_DETAIL.state();
    let map = live.read();
    let TuiRenderUnit::TuiToolCard(current) = &map["stream-test"].nested_units[0] else {
        unreachable!()
    };
    assert_eq!(current.output_summary.as_ptr(), body);
    assert_eq!(previous.output_summary, "retained tool result");
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
            origin_message_id: None,
            origin_readable: None,
        },
        None,
        false,
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

/// [M07 回归] BgLiveDetail 分层计量：stream 与 nested 是两份真实分配，
/// tool_cards 计入累积器实际持有的字段。
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
    assert!(
        projected.stream_bytes > 0,
        "projected 后 stream 仍持有自己的 clone，必须计入"
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
            origin_message_id: None,
            origin_readable: None,
        },
        None,
        false,
    );
    let with_tool = BG_LIVE_DETAIL.state().read()["stream-test"].memory_bytes();
    assert!(with_tool.tool_cards_bytes < preview::FIELD_BYTES);
    let live = BG_LIVE_DETAIL.state();
    let map = live.read();
    let detail = &map["stream-test"];
    assert!(
        detail.tool_cards[0].raw_input.is_null(),
        "超限结构化输入不创建部分 JSON"
    );
    assert!(detail.preview_omitted);
}

/// [回归测试] 任意展示字段都须先裁剪，不能只约束 text/reasoning。
#[test]
#[serial_test::serial]
fn all_background_display_payloads_are_bounded_without_changing_event_data() {
    let fixture = Fixture::new();
    let huge = "正文🙂".repeat(64 * 1024);
    seed_live_from_started("stream-test", "agent", &huge, None);
    fixture.text(&huge);
    append_bg_reasoning_chunk(
        "stream-agent",
        &TuiReasoningChunk {
            text: huge.clone(),
            message_id: Some("message".into()),
            agent_id: None,
        },
    );
    publish_pending_streams();
    let started = TuiToolStarted {
        tool_id: "huge-tool".into(),
        tool_name: "Skill".into(),
        input_summary: huge.clone(),
        raw_input: serde_json::json!({"file_path": huge}),
        agent_id: None,
        origin_message_id: None,
        origin_readable: None,
    };
    handle_bg_tool_started("stream-agent", &started, None, false);
    assert!(
        matches!(
            BG_LIVE_DETAIL.state().read()["stream-test"].tool_cards[0].presentation,
            crate::kit::tui_render_unit::TuiToolPresentation::Generic
        ),
        "超限输入不能生成伪造的专属工具语义"
    );
    handle_bg_tool_ended(
        "stream-agent",
        &TuiToolEnded {
            tool_id: "huge-tool".into(),
            output_summary: huge.clone(),
            is_error: true,
            agent_id: None,
        },
    );
    handle_bg_subagent_stopped("stream-agent", &huge, true);
    mark_task_completed("stream-test", false, 10, Some(huge.clone()));
    mark_task_cancelled("stream-test", &huge);
    let live = BG_LIVE_DETAIL.state();
    let map = live.read();
    let detail = &map["stream-test"];
    assert!(detail.preview_omitted);
    assert!(detail.preview_reserved() <= preview::DETAIL_BYTES);
    assert!(detail.summary.capacity() <= preview::FIELD_BYTES);
    assert!(detail.cancel_reason.as_ref().unwrap().capacity() <= preview::FIELD_BYTES);
    assert!(detail.subagent_result.as_ref().unwrap().capacity() <= preview::FIELD_BYTES);
    assert!(detail.output_preview.as_ref().unwrap().capacity() <= preview::FIELD_BYTES);
    assert!(detail.tool_cards.is_empty());
    assert!(detail.stream.is_none());
    assert_eq!(started.raw_input["file_path"].as_str(), Some(huge.as_str()));
    assert_eq!(
        started.input_summary, huge,
        "显示预算不改写原始事件或业务输入"
    );
}

/// [回归测试] 淘汰无需无限墓碑；未知 Completed 不建正文，终态快照只建有限元数据。
#[test]
#[serial_test::serial]
fn repeated_terminal_tasks_and_late_snapshots_remain_bounded() {
    let _saved = RestoreLive::empty();
    for index in 0..1000 {
        let id = format!("terminal-{index}");
        seed_live_from_started(&id, "agent", "worker", None);
        mark_task_completed(&id, true, 1, Some("finished".into()));
    }
    {
        let live = BG_LIVE_DETAIL.state();
        let map = live.read();
        assert!(map.len() <= preview::TERMINAL_ENTRIES);
        assert!(!map.contains_key("terminal-0"));
        assert!(
            map.iter()
                .map(|(id, detail)| id.capacity() + detail.memory_bytes().reserved)
                .sum::<usize>()
                <= preview::TERMINAL_BYTES
        );
        assert!(map.capacity() <= map.len() * 2 + 64);
    }
    mark_task_completed("terminal-0", true, 2, Some("x".repeat(1024 * 1024)));
    assert!(!BG_LIVE_DETAIL.state().read().contains_key("terminal-0"));
    seed_live_from_terminal_snapshot(
        "terminal-0",
        "agent",
        &"x".repeat(1024 * 1024),
        None,
        "completed",
    );
    {
        let live = BG_LIVE_DETAIL.state();
        let map = live.read();
        let detail = &map["terminal-0"];
        assert!(detail.preview_omitted);
        assert!(detail.nested_units.is_empty());
        assert!(detail.tool_cards.is_empty());
        assert!(detail.summary.capacity() <= preview::FIELD_BYTES);
        assert!(map.len() <= preview::TERMINAL_ENTRIES);
    }
}

#[test]
#[serial_test::serial]
fn stopping_an_agent_preserves_an_independent_running_shell() {
    let _saved = RestoreLive::empty();
    let fixture = Fixture::new();
    seed_live_from_started("independent-shell", "shell", "sleep 100", Some(123));
    fixture.text("agent output");
    handle_bg_subagent_stopped("stream-agent", "done", false);
    let live = BG_LIVE_DETAIL.state();
    let map = live.read();
    let shell = &map["independent-shell"];
    assert_eq!(shell.status, BgLiveStatus::Running);
    assert_eq!(shell.summary, "sleep 100");
    assert_eq!(shell.pid, Some(123));
    assert!(!shell.preview_omitted);
}
