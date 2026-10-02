use super::*;

#[tokio::test]
async fn storage_v2_machine_and_archive_requests_round_trip() {
    let tmp = tempfile::TempDir::new().unwrap();
    let provider_config = make_provider_config("test", "openai", "key", "model");
    let peri_config = make_peri_config_with_provider(provider_config);
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    let created = handle_request(
        "session/new",
        &json!({"cwd": tmp.path()}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let session_id = created["sessionId"].as_str().unwrap();
    append_human_message(&cfg, session_id, "archivable").await;
    let machines = handle_request(
        "peri/machines/list",
        &json!({}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let current = machines["machines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|machine| machine["is_current"] == true)
        .unwrap();
    let machine_id = current["id"].as_str().unwrap();
    handle_request(
        "peri/machines/rename",
        &json!({"machineId": machine_id, "name":"本机测试"}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let workspaces = handle_request(
        "peri/workspaces/list",
        &json!({"machineId": machine_id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let workspace_id = workspaces["workspaces"][0]["id"].as_str().unwrap();
    let query = json!({"_meta":{"peri.sessionWorkspaceV1":{"scope":{"kind":"workspace","value":workspace_id}}}});
    let before = handle_request("session/list", &query, &cfg, &mut sessions, &transport)
        .await
        .unwrap();
    assert_eq!(before["sessions"].as_array().unwrap().len(), 1);
    handle_request(
        "peri/session/archive",
        &json!({"sessionId":session_id,"archived":true}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let regular = handle_request("session/list", &query, &cfg, &mut sessions, &transport)
        .await
        .unwrap();
    assert!(regular["sessions"].as_array().unwrap().is_empty());
    let mut archive_query = query;
    archive_query["_meta"]["peri.sessionWorkspaceV1"]["archived"] = json!(true);
    let archived = handle_request(
        "session/list",
        &archive_query,
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(archived["sessions"].as_array().unwrap().len(), 1);
    handle_request(
        "peri/session/archive",
        &json!({"sessionId":session_id,"archived":false}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
}
