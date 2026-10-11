use crate::session::event_sink::EventSink;
use peri_acp_types::event::ExecutorEvent;
use peri_acp_types::PeriCaps;
use serde_json::Value;
use std::sync::{Arc, Mutex};

#[derive(Debug, Default)]
struct MapperWireTransport {
    notifications: Mutex<Vec<(String, Value)>>,
}

#[async_trait::async_trait]
impl crate::transport::AcpTransport for MapperWireTransport {
    async fn send_request(
        &self,
        _method: &str,
        _params: Value,
    ) -> Result<Value, crate::transport::types::AcpError> {
        Ok(Value::Null)
    }

    async fn send_notification(
        &self,
        method: &str,
        params: Value,
    ) -> Result<(), crate::transport::types::AcpError> {
        self.notifications
            .lock()
            .unwrap()
            .push((method.to_string(), params));
        Ok(())
    }

    async fn recv(&self) -> Option<crate::transport::types::IncomingMessage> {
        None
    }

    async fn send_response(
        &self,
        _id: crate::transport::types::RequestId,
        _result: Result<Value, crate::transport::types::AcpError>,
    ) -> Result<(), crate::transport::types::AcpError> {
        Ok(())
    }
}

#[tokio::test]
async fn test_subagent_stopped_safe_failure_survives_acp_wire_consumption() {
    let transport = Arc::new(MapperWireTransport::default());
    let caps = Arc::new(dashmap::DashMap::new());
    caps.insert(
        "session-1".to_string(),
        PeriCaps {
            agent_event: true,
            agent_activity: true,
            ..PeriCaps::default()
        },
    );
    let sink = crate::session::event_sink::TransportEventSink::new(transport.clone(), caps);
    let failure = peri_acp_types::error::SafeSubagentFailure::new(
        "CHILD_THREAD_SENTINEL",
        peri_acp_types::error::SafeModelErrorDiagnostic::from_model(
            peri_model::ModelError::http_status(500, "provider.example", Some("request-500"))
                .diagnostic(),
        ),
    )
    .expect("safe failure fixture");

    sink.push_event(
        "session-1",
        &ExecutorEvent::SubagentStopped {
            agent_name: "reviewer".into(),
            result: "RESULT_SENTINEL".into(),
            is_error: true,
            instance_id: "INSTANCE_SENTINEL".into(),
            subagent_failure: Some(failure),
        },
        0,
    )
    .await;

    let notifications = transport.notifications.lock().unwrap();
    assert_eq!(
        notifications.len(),
        2,
        "activity 与 legacy ACP wire 都应送达"
    );
    assert_eq!(notifications[0].0, "peri/agent_activity");
    assert_eq!(notifications[0].1["activity"]["status"], "failed");
    let activity_wire = notifications[0].1.to_string();
    assert!(activity_wire.contains("RESULT_SENTINEL"));
    assert!(activity_wire.contains("INSTANCE_SENTINEL"));
    assert!(activity_wire.contains("CHILD_THREAD_SENTINEL"));
    assert!(
        !notifications
            .iter()
            .any(|(method, _)| method == "session/update"),
        "SubagentStopped 不应伪造标准 SessionUpdate"
    );

    assert_eq!(notifications[1].0, "peri/agent_event");
    let event_json = notifications[1].1["event_json"]
        .as_str()
        .expect("legacy ACP event_json");
    let event: crate::event::AcpEvent =
        serde_json::from_str(event_json).expect("decode actual ACP event wire");
    let crate::event::AcpEvent::SubagentStopped {
        agent_name,
        result,
        is_error,
        instance_id,
        subagent_failure: Some(failure),
    } = event
    else {
        panic!("expected typed SubagentStopped ACP event");
    };
    assert_eq!(agent_name, "reviewer");
    assert_eq!(result, "RESULT_SENTINEL");
    assert!(is_error);
    assert_eq!(instance_id, "INSTANCE_SENTINEL");
    let safe_wire = serde_json::to_value(failure).expect("safe failure wire value");
    assert_eq!(safe_wire["child_thread_id"], "CHILD_THREAD_SENTINEL");
    assert_eq!(safe_wire["diagnostic"]["category"], "http_status");
    assert_eq!(safe_wire["diagnostic"]["status"], 500);
    assert_eq!(safe_wire["diagnostic"]["provider"], "provider.example");
    assert_eq!(safe_wire["diagnostic"]["request_id"], "request-500");
}
