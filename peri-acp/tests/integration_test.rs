//! Integration tests for peri-acp.
//!
//! Tests key components end-to-end: transport, broker, event mapping.

use agent_client_protocol::schema::v1::SessionId;
use peri_acp_types::PeriCaps;
use serde_json::json;

#[tokio::test]
async fn test_transport_full_roundtrip() {
    let (client, server) = peri_acp::transport::mpsc::mpsc_transport_pair();

    // Server: echo back
    let server_handle = tokio::spawn(async move {
        use peri_acp::transport::{types::IncomingMessage, AcpTransport};
        if let Some(IncomingMessage::Request { id, params, .. }) = server.recv().await {
            let _ = server.send_response(id, Ok(params)).await;
        }
    });

    // Client: send request
    use peri_acp::transport::AcpTransport;
    let result = client
        .send_request("test/ping", json!({"msg": "hello"}))
        .await
        .unwrap();
    assert_eq!(result, json!({"msg": "hello"}));

    server_handle.await.unwrap();
}

#[tokio::test]
async fn test_broker_approval_flow() {
    use std::sync::Arc;

    use peri_acp::{
        broker::AcpTransportBroker,
        transport::{mpsc::mpsc_transport_pair, AcpTransport},
    };
    use peri_agent::interaction::{
        ApprovalDecision, ApprovalItem, InteractionContext, InteractionResponse,
        UserInteractionBroker,
    };

    let (client, server) = mpsc_transport_pair();
    let broker = AcpTransportBroker::new(Arc::new(server), SessionId::new("test-session"));

    // Server side: respond to RequestPermission with approve
    let server_handle = tokio::spawn(async move {
        use peri_acp::transport::types::IncomingMessage;
        if let Some(IncomingMessage::Request { id, .. }) = client.recv().await {
            // ACP schema format for SelectedPermissionOutcome:
            // {"outcome": {"outcome": "selected", "optionId": "allow_once"}}
            let response = json!({"outcome": {"outcome": "selected", "optionId": "allow_once"}});
            let _ = client.send_response(id, Ok(response)).await;
        }
    });

    // Send approval request
    let ctx = InteractionContext::Approval {
        items: vec![ApprovalItem {
            tool_call_id: "tc_1".into(),
            tool_name: "bash".into(),
            tool_input: json!("ls -la"),
        }],
    };
    let response = broker.request(ctx).await;
    assert!(matches!(response, InteractionResponse::Decisions(decisions)
            if decisions.len() == 1 && matches!(decisions[0], ApprovalDecision::Approve { .. })));

    server_handle.await.unwrap();
}

#[tokio::test]
async fn test_event_mapper_tool_start() {
    use peri_acp::event::map_event;
    use peri_agent::{agent::events::ExecutorEvent, messages::MessageId};

    let event = ExecutorEvent::ToolStart {
        message_id: MessageId::new(),
        tool_call_id: "tc_test".into(),
        name: "Read".into(),
        input: json!({"path": "/tmp/test.txt"}),
        source_agent_id: None,
    };

    let mapped = map_event(&event, 200000, &PeriCaps::default());
    assert_eq!(mapped.len(), 1, "ToolStart must produce one MappedEvent");
    assert!(
        !mapped[0].updates.is_empty(),
        "ToolStart must produce SessionUpdate"
    );
    assert!(
        mapped[0].source_agent_id.is_none(),
        "ToolStart has no source_agent_id"
    );
}

#[tokio::test]
async fn test_event_mapper_text_chunk() {
    use peri_acp::event::map_event;
    use peri_agent::{agent::events::ExecutorEvent, messages::MessageId};

    let event = ExecutorEvent::TextChunk {
        message_id: MessageId::new(),
        chunk: "Hello, world!".into(),
        source_agent_id: None,
    };

    let mapped = map_event(&event, 200000, &PeriCaps::default());
    assert_eq!(mapped.len(), 1, "TextChunk must produce one MappedEvent");
    assert!(
        !mapped[0].updates.is_empty(),
        "TextChunk must produce SessionUpdate"
    );
    assert!(
        mapped[0].source_agent_id.is_none(),
        "TextChunk has no source_agent_id"
    );
}

#[test]
fn test_event_mapper_todo_update_maps_to_plan() {
    use agent_client_protocol::schema::v1::{PlanEntryPriority, PlanEntryStatus, SessionUpdate};
    use peri_acp::event::map_event;
    use peri_agent::agent::events::{ExecutorEvent, TodoEntry, TodoStatus};

    let event = ExecutorEvent::TodoUpdate(vec![
        TodoEntry {
            content: "Fix the bug".into(),
            active_form: Some("Fixing the bug".into()),
            status: TodoStatus::InProgress,
        },
        TodoEntry {
            content: "Write tests".into(),
            active_form: None,
            status: TodoStatus::Pending,
        },
        TodoEntry {
            content: "Done task".into(),
            active_form: None,
            status: TodoStatus::Completed,
        },
    ]);

    let mapped = map_event(&event, 200000, &PeriCaps::default());
    assert_eq!(
        mapped.len(),
        1,
        "TodoUpdate must produce exactly one MappedEvent"
    );

    match &mapped[0].updates[0] {
        SessionUpdate::Plan(plan) => {
            assert_eq!(plan.entries.len(), 3);
            assert_eq!(plan.entries[0].content, "Fix the bug");
            assert_eq!(plan.entries[0].status, PlanEntryStatus::InProgress);
            assert_eq!(plan.entries[1].content, "Write tests");
            assert_eq!(plan.entries[1].status, PlanEntryStatus::Pending);
            assert_eq!(plan.entries[2].content, "Done task");
            assert_eq!(plan.entries[2].status, PlanEntryStatus::Completed);
            // TodoItem 无 priority，默认 Medium
            assert_eq!(plan.entries[0].priority, PlanEntryPriority::Medium);
        }
        other => panic!("Expected SessionUpdate::Plan, got {:?}", other),
    }
}
