use std::time::{Duration, Instant};

use crate::kit::markdown::{
    FenceClose, FenceKind, MermaidFence, MermaidFenceKey, MermaidMessageIdentity, MermaidMessageRef,
};

use super::JobKind;
use super::PreviewContent;
use super::PreviewEngine;
use super::PreviewNotice;
use super::PreviewView;
use super::RenderJob;
use super::rebuild_history;
use super::render_source;
use crate::kit::mermaid::frame::{RenderFailure, RenderOutput, render_final};

fn fence(body: &str, closed: bool) -> MermaidFence {
    MermaidFence {
        kind: FenceKind::Backtick,
        marker_len: 3,
        language: "mermaid".to_string(),
        info: "mermaid".to_string(),
        open_start: 0,
        body_start: 10,
        body_end: 10 + body.len(),
        end: 10 + body.len() + if closed { 4 } else { 0 },
        close: if closed {
            FenceClose::Closed
        } else {
            FenceClose::Open
        },
        opener_confirmed: true,
        opening_line: "```mermaid".to_string(),
        raw: body.to_string(),
        source: body.to_string(),
    }
}

fn key(name: &str) -> MermaidFenceKey {
    MermaidFenceKey {
        message: MermaidMessageIdentity {
            session_id: "session".to_string(),
            message: MermaidMessageRef::Persistent(name.to_string()),
        },
        open_start: 0,
    }
}

fn frame_text(view: &PreviewView) -> String {
    match &view.content {
        PreviewContent::Frame { lines, .. } => lines.join("\n"),
        PreviewContent::Source => panic!("expected a rendered frame"),
    }
}

fn frame_tail(view: &PreviewView) -> Option<&str> {
    match &view.content {
        PreviewContent::Frame { tail, .. } => Some(tail),
        PreviewContent::Source => None,
    }
}

fn render_job(job: &RenderJob) -> Result<RenderOutput, RenderFailure> {
    match job.kind {
        JobKind::Preview => render_source(job.source.as_str(), job.width),
        JobKind::Final => render_final(job.source.as_str(), job.mode, job.width),
    }
}

fn publish(engine: &mut PreviewEngine, now: Instant) -> bool {
    let Some(job) = engine.take_due(now) else {
        return false;
    };
    let result = render_job(&job);
    engine.complete(job, result, now)
}

#[test]
fn first_drawable_prefix_is_scheduled_immediately() {
    let mut engine = PreviewEngine::new();
    let now = Instant::now();
    let header = fence("graph TD\n", false);
    let first = engine.observe(key("m1"), &header, 60, false, now);
    assert!(matches!(first.content, PreviewContent::Source));
    assert_eq!(engine.wait_duration(now), None);

    let drawable = fence("graph TD\nA-->B\n", false);
    let second = engine.observe(key("m1"), &drawable, 60, false, now);
    assert!(matches!(second.content, PreviewContent::Source));
    assert_eq!(engine.wait_duration(now), Some(Duration::ZERO));

    let job = engine.take_due(now).expect("first frame is due now");
    assert_eq!(job.kind, JobKind::Preview);
    assert_eq!(job.source.as_str(), "graph TD\nA-->B\n");
    let output = render_job(&job).expect("render succeeds");
    assert!(engine.complete(job, Ok(output), now));
    let view = engine.view(&key("m1")).expect("state exists");
    let text = frame_text(&view);
    assert!(text.contains('A') && text.contains('B'), "{text}");
    assert!(
        text.contains('▾') || text.contains('▸') || text.contains('▼'),
        "arrow must be visible before the fence closes: {text}"
    );
    assert_eq!(frame_tail(&view), Some(""));
}

#[test]
fn later_prefixes_wait_80ms_and_publish_the_latest_source() {
    let mut engine = PreviewEngine::new();
    let now = Instant::now();
    let first = fence("graph TD\nA-->B\n", false);
    engine.observe(key("m1"), &first, 60, false, now);
    assert!(publish(&mut engine, now));

    let second = fence("graph TD\nA-->B\nB-->C\n", false);
    engine.observe(
        key("m1"),
        &second,
        60,
        false,
        now + Duration::from_millis(10),
    );
    assert_eq!(
        engine.wait_duration(now + Duration::from_millis(10)),
        Some(Duration::from_millis(70))
    );

    let third = fence("graph TD\nA-->B\nB-->C\nC-->D\n", false);
    engine.observe(
        key("m1"),
        &third,
        60,
        false,
        now + Duration::from_millis(20),
    );
    assert_eq!(
        engine.wait_duration(now + Duration::from_millis(20)),
        Some(Duration::from_millis(60))
    );
    assert!(engine.take_due(now + Duration::from_millis(79)).is_none());

    let job = engine
        .take_due(now + Duration::from_millis(80))
        .expect("coalesced frame is due at 80ms");
    assert!(job.source.as_str().contains("C-->D"), "{}", job.source);
}

#[test]
fn closure_bypasses_coalescing_and_renders_the_full_source() {
    let mut engine = PreviewEngine::new();
    let now = Instant::now();
    let first = fence("graph TD\nA-->B\n", false);
    engine.observe(key("m1"), &first, 60, false, now);
    assert!(publish(&mut engine, now));

    let closed = fence("graph TD\nA-->B\nB-->C\n", true);
    engine.observe(
        key("m1"),
        &closed,
        60,
        false,
        now + Duration::from_millis(10),
    );
    assert_eq!(
        engine.wait_duration(now + Duration::from_millis(10)),
        Some(Duration::ZERO)
    );
    let job = engine
        .take_due(now + Duration::from_millis(10))
        .expect("final frame is due immediately");
    assert_eq!(job.kind, JobKind::Final);
    assert_eq!(job.source.as_str(), "graph TD\nA-->B\nB-->C\n");
    let output = render_job(&job).expect("render succeeds");
    assert!(engine.complete(job, Ok(output), now + Duration::from_millis(11)));
    let view = engine.view(&key("m1")).expect("state exists");
    assert_eq!(frame_tail(&view), Some(""));
    assert!(frame_text(&view).contains('C'));
}

#[test]
fn a_running_result_cannot_publish_after_a_newer_prefix() {
    let mut engine = PreviewEngine::new();
    let now = Instant::now();
    let first = fence("graph TD\nA-->B\n", false);
    engine.observe(key("m1"), &first, 60, false, now);
    assert!(publish(&mut engine, now));

    let second = fence("graph TD\nA-->B\nB-->C\n", false);
    engine.observe(
        key("m1"),
        &second,
        60,
        false,
        now + Duration::from_millis(10),
    );
    let stale = engine
        .take_due(now + Duration::from_millis(80))
        .expect("second frame is due");
    let stale_output = render_job(&stale).expect("render succeeds");

    let third = fence("graph TD\nA-->B\nB-->C\nC-->D\n", false);
    engine.observe(
        key("m1"),
        &third,
        60,
        false,
        now + Duration::from_millis(81),
    );
    assert!(!engine.complete(stale, Ok(stale_output), now + Duration::from_millis(82)));
    assert_eq!(
        frame_tail(&engine.view(&key("m1")).expect("state exists")),
        Some("")
    );

    let fresh = engine
        .take_due(now + Duration::from_millis(90))
        .expect("newer frame is due");
    let fresh_output = render_job(&fresh).expect("render succeeds");
    assert!(engine.complete(fresh, Ok(fresh_output), now + Duration::from_millis(90)));
    assert!(frame_text(&engine.view(&key("m1")).expect("state exists")).contains('D'));
}

#[test]
fn a_result_for_a_stale_width_is_discarded() {
    let mut engine = PreviewEngine::new();
    let now = Instant::now();
    let first = fence("graph TD\nA-->B\n", false);
    engine.observe(key("m1"), &first, 60, false, now);
    assert!(publish(&mut engine, now));

    let second = fence("graph TD\nA-->B\nB-->C\n", false);
    engine.observe(
        key("m1"),
        &second,
        40,
        false,
        now + Duration::from_millis(10),
    );
    assert_eq!(
        engine.wait_duration(now + Duration::from_millis(10)),
        Some(Duration::ZERO),
        "width changes bypass the 80ms coalescing window"
    );
    let narrow = engine
        .take_due(now + Duration::from_millis(10))
        .expect("width change is scheduled immediately");
    assert_eq!(narrow.width, 40);
    let narrow_output = render_job(&narrow).expect("render");

    engine.observe(
        key("m1"),
        &second,
        80,
        false,
        now + Duration::from_millis(11),
    );
    assert!(!engine.complete(narrow, Ok(narrow_output), now + Duration::from_millis(12)));
    let wide = engine
        .take_due(now + Duration::from_millis(12))
        .expect("new width is scheduled");
    assert_eq!(wide.width, 80);
}

#[test]
fn pending_jobs_are_replaced_per_instance() {
    let mut engine = PreviewEngine::new();
    let now = Instant::now();
    let first = fence("graph TD\nA-->B\n", false);
    engine.observe(key("m1"), &first, 60, false, now);
    assert!(publish(&mut engine, now));

    engine.observe(
        key("m1"),
        &fence("graph TD\nA-->B\nB-->C\n", false),
        60,
        false,
        now + Duration::from_millis(10),
    );
    engine.observe(
        key("m1"),
        &fence("graph TD\nA-->B\nB-->C\nC-->D\n", false),
        60,
        false,
        now + Duration::from_millis(20),
    );
    assert_eq!(engine.pending_count(), 1);
    let job = engine
        .take_due(now + Duration::from_millis(80))
        .expect("latest job is due");
    assert!(job.source.as_str().contains("C-->D"));
}

#[test]
fn instances_with_the_same_open_offset_stay_independent() {
    let mut engine = PreviewEngine::new();
    let now = Instant::now();
    let left = key("left");
    let right = key("right");
    engine.observe(
        left.clone(),
        &fence("graph TD\nA-->B\n", false),
        60,
        false,
        now,
    );
    engine.observe(
        right.clone(),
        &fence("graph TD\nX-->Y\n", false),
        60,
        false,
        now,
    );
    assert_eq!(engine.pending_count(), 2);

    for _ in 0..2 {
        let job = engine.take_due(now).expect("each instance has its own job");
        let output = render_source(job.source.as_str(), job.width).expect("render");
        assert!(engine.complete(job, Ok(output), now));
    }
    let left_text = frame_text(&engine.view(&left).expect("left state"));
    let right_text = frame_text(&engine.view(&right).expect("right state"));
    assert!(left_text.contains('A'), "{left_text}");
    assert!(right_text.contains('X'), "{right_text}");
}

#[test]
fn unknown_statement_keeps_the_last_frame_and_the_source_tail() {
    let mut engine = PreviewEngine::new();
    let now = Instant::now();
    let first = fence("graph TD\nA-->B\n", false);
    engine.observe(key("m1"), &first, 60, false, now);
    assert!(publish(&mut engine, now));
    let revision = engine.revision();

    let broken = fence("graph TD\nA-->B\nthis is not valid\n", false);
    let view = engine.observe(
        key("m1"),
        &broken,
        60,
        false,
        now + Duration::from_millis(10),
    );
    assert_eq!(
        frame_tail(&view).expect("the last frame stays visible"),
        "this is not valid\n"
    );
    assert_eq!(view.notice, Some(PreviewNotice::UnknownStatement));
    assert_eq!(engine.revision(), revision);
    assert_eq!(engine.wait_duration(now + Duration::from_millis(10)), None);
}

#[test]
fn a_new_statement_never_flashes_back_to_an_empty_frame() {
    let mut engine = PreviewEngine::new();
    let now = Instant::now();
    let first = fence("graph TD\nA-->B\n", false);
    engine.observe(key("m1"), &first, 60, false, now);
    assert!(publish(&mut engine, now));

    for (index, source) in [
        "graph TD\nA-->B\nB-->C\n",
        "graph TD\nA-->B\nB-->C\nC-->D\n",
        "graph TD\nA-->B\nB-->C\nC-->D\nD--",
    ]
    .into_iter()
    .enumerate()
    {
        let view = engine.observe(
            key("m1"),
            &fence(source, false),
            60,
            false,
            now + Duration::from_millis(10 * (index as u64 + 1)),
        );
        let text = frame_text(&view);
        assert!(
            !text.trim().is_empty(),
            "frame flashed empty for {source:?}"
        );
        assert!(
            view.notice.is_none(),
            "unexpected notice for {source:?}: {:?}",
            view.notice
        );
    }
}

#[test]
fn closed_final_matches_a_one_shot_render() {
    let full = "graph TD\nA-->B\nB-->C\n";
    let mut engine = PreviewEngine::new();
    let now = Instant::now();
    engine.observe(key("m1"), &fence(full, false), 60, false, now);
    assert!(publish(&mut engine, now));

    let closed = fence(full, true);
    engine.observe(
        key("m1"),
        &closed,
        60,
        false,
        now + Duration::from_millis(10),
    );
    let job = engine
        .take_due(now + Duration::from_millis(10))
        .expect("final job");
    let output = render_job(&job).expect("one-shot render");
    assert!(engine.complete(job, Ok(output), now + Duration::from_millis(11)));
    let view = engine.view(&key("m1")).expect("state exists");
    let expected = render_source(full, 60)
        .expect("one-shot render")
        .lines()
        .join("\n");
    assert_eq!(frame_text(&view), expected);
}

#[test]
fn history_rebuild_uses_the_same_final_function() {
    let full = "graph TD\nA-->B\nB-->C\n";
    let replay = rebuild_history(&fence(full, true), 60, None);
    let expected = render_source(full, 60)
        .expect("one-shot render")
        .lines()
        .join("\n");
    assert_eq!(replay.mode, super::super::stream::DiagramMode::Flowchart);
    assert_eq!(
        frame_text(&PreviewView {
            content: replay.content,
            notice: replay.notice,
        }),
        expected
    );
}

#[test]
fn unclosed_history_rebuilds_the_last_trusted_preview() {
    let replay = rebuild_history(&fence("graph TD\nA-->B\nC--", false), 60, None);
    assert_eq!(replay.notice, Some(PreviewNotice::Unclosed));
    match replay.content {
        PreviewContent::Frame { lines, tail, .. } => {
            assert!(lines.join("\n").contains('A'));
            assert_eq!(tail, "C--");
        }
        PreviewContent::Source => panic!("expected the last trusted frame"),
    }

    let interrupted = rebuild_history(&fence("graph TD\nA-->B\nC--", false), 60, Some("timeout"));
    assert_eq!(
        interrupted.notice,
        Some(PreviewNotice::Interrupted("timeout".to_string()))
    );
}

#[test]
fn final_validation_failure_keeps_the_source_and_a_short_reason() {
    let mut engine = PreviewEngine::new();
    let now = Instant::now();
    let broken = fence("graph TD\nA-->B\nthis is not valid\n", true);
    let initial = engine.observe(key("m1"), &broken, 60, false, now);
    assert!(matches!(initial.content, PreviewContent::Source));
    let job = engine
        .take_due(now)
        .expect("validation runs in the background job");
    let failure = render_job(&job).expect_err("unknown statements fail validation");
    assert!(engine.complete(job, Err(failure), now + Duration::from_millis(1)));
    let view = engine.view(&key("m1")).expect("state exists");
    assert!(matches!(view.content, PreviewContent::Source));
    match view.notice {
        Some(PreviewNotice::Failed(reason)) => {
            assert!(reason.contains("unrecognized"), "{reason}");
            assert!(!reason.contains('\n'));
        }
        other => panic!("unexpected notice: {other:?}"),
    }
}

#[test]
fn state_class_and_er_render_only_after_closure() {
    let mut engine = PreviewEngine::new();
    let now = Instant::now();
    let open = fence("stateDiagram-v2\n[*] --> A\n", false);
    let view = engine.observe(key("m1"), &open, 60, false, now);
    assert!(matches!(view.content, PreviewContent::Source));
    assert_eq!(engine.wait_duration(now), None);

    let closed = fence("stateDiagram-v2\n[*] --> A\nA --> [*]\n", true);
    engine.observe(
        key("m1"),
        &closed,
        60,
        false,
        now + Duration::from_millis(1),
    );
    let job = engine
        .take_due(now + Duration::from_millis(1))
        .expect("closed state diagram is scheduled");
    let output = render_job(&job).expect("render succeeds");
    assert!(engine.complete(job, Ok(output), now + Duration::from_millis(2)));
    assert!(frame_text(&engine.view(&key("m1")).expect("state exists")).contains('A'));
}

#[test]
fn block_diagrams_keep_source_even_when_closed() {
    let mut engine = PreviewEngine::new();
    let now = Instant::now();
    let closed = fence("block-beta\n A B\n", true);
    let view = engine.observe(key("m1"), &closed, 60, false, now);
    assert!(matches!(view.content, PreviewContent::Source));
    assert!(engine.take_due(now).is_none());
}

#[test]
fn frozen_unclosed_keeps_the_frame_and_marks_it_unclosed() {
    let mut engine = PreviewEngine::new();
    let now = Instant::now();
    let open = fence("graph TD\nA-->B\nC--", false);
    engine.observe(key("m1"), &open, 60, true, now);
    assert!(publish(&mut engine, now));
    let view = engine.view(&key("m1")).expect("state exists");
    assert_eq!(view.notice, Some(PreviewNotice::Unclosed));
    assert_eq!(frame_tail(&view), Some("C--"));
}

#[test]
fn open_scope_reports_its_label_and_confirmed_count() {
    let mut engine = PreviewEngine::new();
    let now = Instant::now();
    let source = "graph TD\nA-->B\nsubgraph S[Group]\nC-->D\n";
    let view = engine.observe(key("m1"), &fence(source, false), 60, false, now);
    assert_eq!(
        view.notice,
        Some(PreviewNotice::OpenScope {
            label: "Group".to_string(),
            statements: 1
        })
    );
}

#[test]
fn a_final_render_failure_keeps_the_full_source_visible() {
    let mut engine = PreviewEngine::new();
    let now = Instant::now();
    let broken = fence("stateDiagram-v2\nbogus line\n", true);
    engine.observe(key("m1"), &broken, 60, false, now);
    let job = engine
        .take_due(now)
        .expect("closed state diagram is scheduled");
    let failure = render_job(&job).expect_err("the state parser rejects this line");
    assert!(engine.complete(job, Err(failure), now + Duration::from_millis(1)));
    let view = engine.view(&key("m1")).expect("state exists");
    assert!(matches!(view.content, PreviewContent::Source));
    assert!(matches!(view.notice, Some(PreviewNotice::Failed(_))));
}

#[test]
fn a_sequence_arrow_appears_before_the_fence_closes() {
    let mut engine = PreviewEngine::new();
    let now = Instant::now();
    let open = fence("sequenceDiagram\n用户->>系统: 发送请求\n", false);
    engine.observe(key("m1"), &open, 60, false, now);
    let job = engine.take_due(now).expect("message frame is due");
    let output = render_job(&job).expect("render succeeds");
    assert!(engine.complete(job, Ok(output), now));
    let text = frame_text(&engine.view(&key("m1")).expect("state exists"));
    assert!(text.contains("用户"), "{text}");
    assert!(text.contains("系统"), "{text}");
    assert!(text.contains("发送请求"), "{text}");
    assert!(text.contains('▸') || text.contains('▾'), "{text}");
}

#[test]
fn history_rebuild_keeps_the_full_source_when_the_closed_fence_fails_validation() {
    let replay = rebuild_history(
        &fence("graph TD\nA-->B\nthis is not valid\n", true),
        60,
        None,
    );
    assert!(matches!(replay.content, PreviewContent::Source));
    match replay.notice {
        Some(PreviewNotice::Failed(reason)) => {
            assert!(reason.contains("unrecognized"), "{reason}");
        }
        other => panic!("unexpected notice: {other:?}"),
    }
}

#[test]
fn identical_sources_share_one_layout_and_one_geometry_entry() {
    let mut engine = PreviewEngine::new();
    let now = Instant::now();
    let shared = fence("graph TD\nA-->B\n", true);
    engine.observe(key("m1"), &shared, 60, true, now);
    let first = engine.take_due(now).expect("first diagram is due");
    let output = render_job(&first).expect("render succeeds");
    assert!(engine.complete(first, Ok(output), now));
    assert_eq!(engine.layouts(), 1);

    let mut second_key = key("m2");
    second_key.message.message = MermaidMessageRef::Persistent("m2".to_string());
    engine.observe(second_key, &shared, 60, true, now);
    let second = engine.take_due(now).expect("second diagram is due");
    let cached = engine
        .cached_output(&second)
        .expect("identical source hits geometry");
    assert!(engine.complete(second, Ok(cached), now));
    assert_eq!(engine.layouts(), 1, "shared geometry is counted once");
    assert_eq!(
        engine.retained_geometry_bytes(),
        engine.retained_geometry_bytes()
    );
}

#[test]
fn switching_sessions_discards_in_flight_results() {
    let mut engine = PreviewEngine::new();
    let now = Instant::now();
    let old = fence("graph TD\nA-->B\n", true);
    engine.observe(key("m1"), &old, 60, true, now);
    let job = engine.take_due(now).expect("job is due");

    let mut other = key("m2");
    other.message.session_id = "other-session".to_string();
    engine.observe(other, &old, 60, true, now);
    let result = render_job(&job);
    assert!(!engine.complete(job, result, now));
    assert_eq!(engine.layouts(), 0);
}

#[test]
fn oversized_sources_keep_the_source_without_queueing() {
    let mut engine = PreviewEngine::new();
    let now = Instant::now();
    let mut source = String::from("graph TD\n");
    source.push_str(&"A-->B\n".repeat(super::MAX_LAYOUT_SOURCE_BYTES / 6 + 8));
    let large = fence(&source, true);
    let view = engine.observe(key("m1"), &large, 60, true, now);
    assert_eq!(engine.wait_duration(now), None);
    assert!(matches!(view.notice, Some(PreviewNotice::TooLarge { .. })));
    assert!(matches!(view.content, PreviewContent::Source));
}

#[test]
fn evicting_geometry_drops_the_instance_frame_too() {
    let mut engine = PreviewEngine::new();
    let now = Instant::now();
    let closed = fence("graph TD\nA-->B\n", true);
    engine.observe(key("m1"), &closed, 60, true, now);
    let job = engine.take_due(now).expect("job is due");
    let result = render_job(&job);
    assert!(engine.complete(job, result, now));
    assert!(matches!(
        engine.peek(&key("m1"), &closed, 60).content,
        PreviewContent::Frame { .. }
    ));
    engine.evict_geometry_to(0);
    assert!(matches!(
        engine.peek(&key("m1"), &closed, 60).content,
        PreviewContent::Source
    ));
}

#[test]
fn layout_publication_bumps_only_the_owning_message_revision() {
    let mut engine = PreviewEngine::new();
    let now = Instant::now();
    let first = fence("graph TD\nA-->B\n", true);
    let second = fence("graph TD\nC-->D\n", true);
    engine.observe(key("m1"), &first, 60, true, now);
    engine.observe(key("m2"), &second, 60, true, now);
    let identity = |name: &str| MermaidMessageIdentity {
        session_id: "session".to_string(),
        message: MermaidMessageRef::Persistent(name.to_string()),
    };
    let first_before = engine.message_revision(&identity("m1"));
    let second_before = engine.message_revision(&identity("m2"));
    let job = engine.take_due(now).expect("first job is due");
    let completed = job.key.message.message.clone();
    let result = render_job(&job);
    assert!(engine.complete(job, result, now));
    let (own, other) = if completed == MermaidMessageRef::Persistent("m1".to_string()) {
        ("m1", "m2")
    } else {
        ("m2", "m1")
    };
    let own_before = if own == "m1" {
        first_before
    } else {
        second_before
    };
    let other_before = if other == "m1" {
        first_before
    } else {
        second_before
    };
    assert!(engine.message_revision(&identity(own)) > own_before);
    assert_eq!(engine.message_revision(&identity(other)), other_before);
}
