use super::*;
use crate::{identity::AttemptId, session::TurnId};

fn binding() -> ExecutionBinding {
    ExecutionBinding {
        turn_id: TurnId::new(),
        attempt_id: AttemptId::new(),
    }
}

fn event() -> ExecutorEvent {
    ExecutorEvent::SubagentStarted {
        agent_name: "worker".into(),
        instance_id: "child".into(),
        is_background: false,
        parent_tool_call_id: None,
    }
}

#[test]
fn child_origin_retains_real_execution_and_clones_share_sequence() {
    let execution = binding();
    let stream = ChildEventStream::new("child".into(), execution.clone());
    let first = stream.wrap(event());
    let second = stream.clone().wrap(event());
    assert_eq!(first.child_origin().unwrap().execution, execution);
    assert_eq!(first.child_origin().unwrap().sequence, 1);
    assert_eq!(second.child_origin().unwrap().sequence, 2);
    assert_eq!(
        first.child_origin().unwrap().run_order,
        second.child_origin().unwrap().run_order
    );
    assert!(first.child_origin().unwrap().valid());
    assert!(matches!(
        first.payload(),
        ExecutorEvent::SubagentStarted { .. }
    ));
    let resumed = ChildEventStream::new("child".into(), binding()).wrap(event());
    assert_ne!(
        first.child_origin().unwrap().execution,
        resumed.child_origin().unwrap().execution
    );
    assert!(resumed.child_origin().unwrap().run_order > first.child_origin().unwrap().run_order);
}

#[test]
fn forwarding_a_nested_child_does_not_relabel_its_origin() {
    let descendant = ChildEventStream::new("descendant".into(), binding()).wrap(event());
    let expected = descendant.child_origin().unwrap().clone();
    let forwarded = ChildEventStream::new("ancestor".into(), binding()).wrap(descendant);
    assert_eq!(forwarded.child_origin(), Some(&expected));
    let encoded = serde_json::to_value(&forwarded).unwrap();
    let decoded: ExecutorEvent = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded.child_origin(), Some(&expected));
}
