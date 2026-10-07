use super::*;
use crate::acp_client::AcpTuiClient;
use peri_acp::host::execution_admission::ReverseExecutionAdmission;
use peri_acp::transport::{AcpRequestBridge, mpsc::mpsc_transport_pair};
use peri_acp_types::execution_admission::{
    AdmissionOutcome, AdmissionRequest, ExecutionAdmissionError, ExecutionAdmissionPort,
};
use peri_acp_types::session_resources::{
    ControlState,
    work::{WorkCandidate, WorkSnapshot, WorkStage, WorkState},
};

fn snapshot_request() -> AdmissionRequest {
    AdmissionRequest {
        request_id: "bridge-fixture".into(),
        existing_admission: None,
        snapshot: WorkSnapshot {
            pending_commands: Vec::new(),
            session_id: "bridge-session".into(),
            control: ControlState::default(),
            state: WorkState::default(),
            blocked: false,
            candidates: vec![WorkCandidate {
                work_id: "bridge-work".into(),
                work_revision: 0,
                stage: WorkStage::ReasonReady,
                batch_id: None,
                delivery_ids: Vec::new(),
                requires_recovery: false,
            }],
        }
        .into(),
    }
}

#[tokio::test]
async fn real_sdk_headless_and_interactive_reverse_bridge_with_snapshot_fixture() {
    for interactive in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let dispatcher = Arc::new(
            JsonlSdkDispatcher::launch(SdkDispatcherLaunch {
                executable: trusted_bun_path().expect("real deployment gate requires Bun"),
                module: trusted_sdk_module()
                    .expect("real deployment gate requires production SDK sidecar"),
                database: directory.path().join("registry.db"),
                instance_id: "bridge-instance".into(),
                generation_id: "bridge-generation".into(),
            })
            .await
            .unwrap(),
        );
        let (client_transport, server_transport) = mpsc_transport_pair();
        let (client, notification_tx, _notification_rx) = if interactive {
            AcpTuiClient::new_interactive(client_transport)
        } else {
            AcpTuiClient::new(client_transport)
        };
        client.spawn_pump_with_execution_dispatcher(notification_tx, Some(dispatcher));
        let port =
            ReverseExecutionAdmission::new(Arc::new(AcpRequestBridge(Arc::new(server_transport))));
        let admitted = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            port.admit(snapshot_request()),
        )
        .await
        .unwrap()
        .unwrap();
        let AdmissionOutcome::Admitted { admission } = admitted else {
            panic!("SDK reverse bridge must admit fixture work");
        };
        let mut confirm = snapshot_request();
        confirm.request_id = admission.admission_id.clone();
        confirm.existing_admission = Some(admission.clone());
        assert_eq!(
            port.admit(confirm).await.unwrap(),
            AdmissionOutcome::Admitted { admission }
        );
        client.close();
    }
}

#[tokio::test]
async fn missing_execution_dispatcher_returns_error_without_rust_fallback() {
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notification_tx, _notification_rx) = AcpTuiClient::new(client_transport);
    client.spawn_pump(notification_tx);
    let port =
        ReverseExecutionAdmission::new(Arc::new(AcpRequestBridge(Arc::new(server_transport))));
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        port.admit(snapshot_request()),
    )
    .await
    .unwrap();
    assert!(matches!(result, Err(ExecutionAdmissionError::Unavailable)));
    client.close();
}

#[tokio::test]
async fn real_sdk_available_hint_reverses_query_over_actual_tui_transport_with_idle_fixture() {
    use peri_acp::transport::{AcpTransport, types::IncomingMessage};
    let directory = tempfile::tempdir().unwrap();
    let (client_transport, server_transport) = mpsc_transport_pair();
    let (client, notification_tx, _notification_rx) = AcpTuiClient::new(client_transport);
    let dispatcher = Arc::new(
        JsonlSdkDispatcher::launch_with_reverse_transport(
            SdkDispatcherLaunch {
                executable: trusted_bun_path().expect("real deployment gate requires Bun"),
                module: trusted_sdk_module().expect("real deployment gate requires SDK sidecar"),
                database: directory.path().join("registry.db"),
                instance_id: "available-hint-instance".into(),
                generation_id: "available-hint-generation".into(),
            },
            Some(Arc::new(AcpExecutionReverse(client.clone()))),
        )
        .await
        .unwrap(),
    );
    client.spawn_pump_with_execution_dispatcher(notification_tx, Some(dispatcher.clone()));
    server_transport
        .send_notification(
            "session/work/available",
            serde_json::json!({
                "sessionId":"available-hint-fixture", "revision":5, "lifecycle":1,
                "controlGeneration":0, "executionProtocol":1,
            }),
        )
        .await
        .unwrap();
    let incoming = tokio::time::timeout(std::time::Duration::from_secs(5), server_transport.recv())
        .await
        .unwrap()
        .unwrap();
    let IncomingMessage::Request { id, method, params } = incoming else {
        panic!("SDK activation must query through the actual ACP transport");
    };
    assert_eq!(method, "session/work/query");
    assert_eq!(params["sessionId"], "available-hint-fixture");
    server_transport
        .send_response(
            id,
            Ok(serde_json::json!({
                "control":ControlState::default(), "work":serde_json::Value::Null,
            })),
        )
        .await
        .unwrap();
    let record = dispatcher
        .send_request(
            "peri/execution/query",
            serde_json::json!({"sessionId":"available-hint-fixture"}),
        )
        .await
        .unwrap();
    assert!(record["attempt"].is_null());
    assert!(record["budgets"].as_array().unwrap().is_empty());
    client.close();
}
