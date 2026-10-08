use super::*;

#[tokio::test]
async fn session_io_preparation_rejects_mutations_during_close_but_allows_snapshot() {
    let temporary = tempfile::TempDir::new().unwrap();
    let (_cfg, mut states, session_id) = make_user_input_session(&temporary).await;
    states.get_mut(&session_id).unwrap().closing = true;
    let params = json!({"sessionId": session_id});
    let error =
        crate::host::requests::session_io::prepare("session/input/enqueue", &params, &states)
            .err()
            .unwrap();
    assert_eq!(error.code, -32010);
    assert!(
        crate::host::requests::session_io::prepare("session/input/snapshot", &params, &states)
            .is_ok()
    );
    assert!(crate::host::requests::session_io::prepare(
        "session/input/enqueue",
        &json!({"sessionId": "missing"}),
        &states
    )
    .is_err());
}

#[tokio::test]
async fn removed_execution_protocols_are_unsupported() {
    let temporary = tempfile::TempDir::new().unwrap();
    let (cfg, mut states, session_id) = make_user_input_session(&temporary).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    for method in [
        "session/control",
        "session/control/state",
        "session/control/resolve",
        "session/work/query",
        "session/work/resolve",
        "session/execute",
        "session/execute/resolve",
    ] {
        let error = handle_request(
            method,
            &json!({"sessionId":session_id}),
            &cfg,
            &mut states,
            &transport,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, -32601, "{method}");
    }
}
