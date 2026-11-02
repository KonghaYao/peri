use super::*;
use crate::kit::stream_data::TuiTextChunk;
use peri_acp_types::{
    identity::AttemptId,
    session::{ExecutionBinding, TurnId},
};

fn origin(id: &str, run_order: u64) -> ChildEventOrigin {
    ChildEventOrigin {
        child_thread_id: id.into(),
        execution: ExecutionBinding {
            turn_id: TurnId::new(),
            attempt_id: AttemptId::new(),
        },
        run_order,
        sequence: 1,
    }
}
fn start(id: &str) -> AcpEventData {
    AcpEventData::SubagentStarted {
        agent_id: id.into(),
        agent_name: "worker".into(),
        is_background: false,
        parent_tool_call_id: None,
    }
}
fn stop(id: &str) -> AcpEventData {
    AcpEventData::SubagentStopped {
        agent_id: id.into(),
        result: "done".into(),
        is_error: false,
    }
}
fn chunk(id: &str) -> AcpEventData {
    AcpEventData::TextChunk(TuiTextChunk {
        text: "preview".into(),
        message_id: None,
        agent_id: Some(id.into()),
    })
}

#[test]
fn resumed_child_rejects_old_start_body_and_terminal() {
    let mut routing = ChildRunRouting::default();
    let mut old = origin("child", 1);
    assert!(routing.accept(&start("child"), &old, true));
    old.sequence += 1;
    assert!(routing.accept(&stop("child"), &old, true));
    let mut resumed = origin("child", 2);
    assert!(routing.accept(&start("child"), &resumed, true));
    old.sequence += 1;
    assert!(!routing.accept(&chunk("child"), &old, true));
    assert!(!routing.accept(&stop("child"), &old, true));
    assert!(!routing.accept(&start("child"), &old, true));
    resumed.sequence += 1;
    assert!(routing.accept(&chunk("child"), &resumed, true));
}

#[test]
fn same_session_reset_rebinds_live_run_but_rejects_old_view_body() {
    let mut routing = ChildRunRouting::default();
    let mut run = origin("child", 1);
    assert!(routing.accept(&start("child"), &run, true));
    let mut turn = CurrentTurn::default();
    routing.restore(&mut turn);
    assert!(turn.accepts_subagent_execution("child", &run.execution));
    assert!(turn.subagents[0].preview_omitted);
    run.sequence += 1;
    assert!(!routing.accept(&chunk("child"), &run, false));
    run.sequence += 1;
    assert!(routing.accept(&chunk("child"), &run, true));
    run.sequence += 1;
    assert!(routing.accept(&stop("child"), &run, false));
    assert!(routing.active.is_empty());
}

#[test]
fn retired_run_directory_is_bounded_and_evicted_run_cannot_resurrect() {
    let mut routing = ChildRunRouting::default();
    let first = origin("child-1", 1);
    for order in 1..=10_000 {
        let id = format!("child-{order}");
        let mut run = if order == 1 {
            first.clone()
        } else {
            origin(&id, order)
        };
        assert!(routing.accept(&start(&id), &run, true));
        run.sequence += 1;
        assert!(routing.accept(&stop(&id), &run, true));
    }
    assert!(routing.active.is_empty());
    assert_eq!(routing.retired.len(), RETIRED_RUNS);
    assert!(routing.retired.capacity() <= RETIRED_RUNS * 2);
    assert!(!routing.accept(&start("child-1"), &first, true));
    routing.clear();
    assert!(routing.accept(&start("child-1"), &first, true));
}

#[test]
fn origin_cannot_relabel_another_child_or_root_preview() {
    let mut routing = ChildRunRouting::default();
    let mut run = origin("child", 1);
    assert!(routing.accept(&start("child"), &run, true));
    run.sequence += 1;
    assert!(!routing.accept(&chunk("other-child"), &run, true));
    let root_chunk = AcpEventData::TextChunk(TuiTextChunk {
        text: "must not reach root".into(),
        message_id: None,
        agent_id: None,
    });
    assert!(!routing.accept(&root_chunk, &run, true));
    assert!(routing.accept(&chunk("child"), &run, true));
}

#[test]
fn higher_unseen_sequence_of_same_live_run_may_continue_after_view_reset() {
    let mut routing = ChildRunRouting::default();
    let mut run = origin("child", 1);
    assert!(routing.accept(&start("child"), &run, true));
    run.sequence = 10;
    assert!(routing.accept(&chunk("child"), &run, true));
    let mut reset_turn = CurrentTurn::default();
    routing.restore(&mut reset_turn);
    // A local reset is not an execution restart or a remote producer barrier.
    assert!(!routing.accept(&chunk("child"), &run, true));
    run.sequence = 11;
    assert!(routing.accept(&chunk("child"), &run, true));
    assert!(reset_turn.subagents[0].preview_omitted);
}

#[test]
fn known_live_run_survives_retirement_of_many_newer_runs() {
    let mut routing = ChildRunRouting::default();
    let mut live = origin("long-running", 1);
    assert!(routing.accept(&start("long-running"), &live, true));
    for order in 2..=2_000 {
        let id = format!("short-{order}");
        let mut short = origin(&id, order);
        assert!(routing.accept(&start(&id), &short, true));
        short.sequence += 1;
        assert!(routing.accept(&stop(&id), &short, true));
    }
    assert!(routing.retired_floor > live.run_order);
    assert_eq!(routing.active.len(), 1);
    let mut turn = CurrentTurn::new();
    routing.restore(&mut turn);
    assert!(turn.accepts_subagent_execution("long-running", &live.execution));
    live.sequence += 1;
    assert!(routing.accept(&chunk("long-running"), &live, true));
    live.sequence += 1;
    assert!(routing.accept(&stop("long-running"), &live, true));
    assert!(routing.active.is_empty());
}

#[test]
fn extremely_late_first_start_expires_only_its_preview_beyond_retirement_window() {
    let mut routing = ChildRunRouting::default();
    let unseen = origin("unseen-delayed-child", 1);
    for order in 2..=514 {
        let id = format!("finished-{order}");
        let mut finished = origin(&id, order);
        assert!(routing.accept(&start(&id), &finished, true));
        finished.sequence += 1;
        assert!(routing.accept(&stop(&id), &finished, true));
    }
    assert!(!routing.accept(&start("unseen-delayed-child"), &unseen, true));
    assert!(routing.active.is_empty());
    // Preview expiry neither invents completion nor owns business task updates.
    assert!(!is_child_preview_event(&AcpEventData::BgTaskCompleted {
        task_id: "real-task".into(),
        kind: Some("agent".into()),
        success: true,
        duration_ms: 1,
        output_preview: Some("real result".into()),
        revision: None,
    }));
    assert!(!is_child_preview_event(&AcpEventData::BgTaskCancelled {
        task_id: "real-task".into(),
        reason: "real cancellation".into(),
        revision: None,
    }));
}
