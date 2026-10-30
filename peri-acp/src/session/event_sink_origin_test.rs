use super::*;
use peri_acp_types::{
    event::ChildEventStream,
    identity::AttemptId,
    session::{ExecutionBinding, TurnId},
};

#[tokio::test]
async fn child_origin_crosses_standard_and_legacy_wire_without_relabeling() {
    let (transport, sink) = compact_test_sink();
    let execution = ExecutionBinding {
        turn_id: TurnId::new(),
        attempt_id: AttemptId::new(),
    };
    let stream = ChildEventStream::new("child".into(), execution.clone());
    let events = [
        stream.wrap(ExecutorEvent::SubagentStarted {
            agent_name: "worker".into(),
            instance_id: "child".into(),
            is_background: true,
            parent_tool_call_id: None,
        }),
        stream.wrap(ExecutorEvent::TextChunk {
            message_id: MessageId::new(),
            chunk: "child text".into(),
            source_agent_id: Some("child".into()),
        }),
        stream.wrap(ExecutorEvent::SubagentStopped {
            agent_name: "worker".into(),
            instance_id: "child".into(),
            result: "done".into(),
            is_error: false,
            subagent_failure: None,
        }),
    ];
    for event in &events {
        sink.push_event("s1", event, 0).await.unwrap();
    }
    let notifications = transport.notifications.lock().unwrap();
    assert_eq!(notifications.len(), 3);
    for (index, (_, payload)) in notifications.iter().enumerate() {
        let wire: peri_acp_types::event::ChildEventOrigin =
            serde_json::from_value(payload["_meta"]["peri"]["childRun"].clone()).unwrap();
        assert_eq!(&wire, events[index].child_origin().unwrap());
        assert_eq!(wire.execution, execution);
    }
    assert_eq!(notifications[0].0, "peri/agent_event");
    assert_eq!(notifications[1].0, "session/update");
    assert_eq!(
        notifications[1].1["_meta"]["peri"]["sourceAgentId"],
        "child"
    );
    assert_eq!(notifications[2].0, "peri/agent_event");
    assert!(
        serde_json::from_str::<AcpEvent>(notifications[0].1["event_json"].as_str().unwrap())
            .is_ok()
    );
}
