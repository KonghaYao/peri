use crate::host::requests::tests::HomeDirGuard;
use crate::host::requests::tests::MockTransport;
use serde_json::json;
use serial_test::serial;
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::test]
#[serial]
async fn cron_endpoint_does_not_fall_back_to_local_scheduler_when_mcp_is_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let cfg = crate::host::requests::tests::cron_tests::deployment(&tmp, false).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let cwd = tmp
        .path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut sessions = HashMap::new();
    let id = crate::host::requests::tests::cron_tests::new_session(
        &cfg,
        &mut sessions,
        &transport,
        &cwd,
    )
    .await;
    let environment = sessions[&id].environment.clone().unwrap();
    crate::host::requests::tests::cron_tests::cron_tool(
        &environment,
        &id,
        &cwd,
        "cron_register",
        json!({
            "expression":"*/5 * * * *", "prompt":"must-not-bypass-mcp"
        }),
    )
    .await;
    let local = crate::host::requests::tests::cron_tests::scheduler(&environment);
    let job_id = local.lock().list_tasks()[0].id.clone();
    let before = crate::host::requests::handle_request(
        "cron/list",
        &json!({"sessionId":id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(before["jobs"][0]["id"], job_id);
    environment.cfg.mcp_pool.as_ref().unwrap().shutdown().await;
    for method in ["cron/list", "cron/toggle", "cron/remove"] {
        assert!(
            crate::host::requests::handle_request(
                method,
                &json!({"sessionId":id,"id":job_id}),
                &cfg,
                &mut sessions,
                &transport
            )
            .await
            .is_err(),
            "{method} must fail when the MCP owner is unavailable"
        );
    }
    assert!(local.lock().list_tasks()[0].enabled);
    assert_eq!(local.lock().list_tasks().len(), 1);
}

#[tokio::test]
#[serial]
async fn cron_endpoint_uses_session_scheduler_and_rejects_cross_session_jobs() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let cfg = crate::host::requests::tests::cron_tests::deployment(&tmp, false).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let cwd = tmp
        .path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut sessions = HashMap::new();
    let first = crate::host::requests::tests::cron_tests::new_session(
        &cfg,
        &mut sessions,
        &transport,
        &cwd,
    )
    .await;
    let second = crate::host::requests::tests::cron_tests::new_session(
        &cfg,
        &mut sessions,
        &transport,
        &cwd,
    )
    .await;
    for id in [&first, &second] {
        crate::host::requests::tests::cron_tests::cron_tool(
            sessions[id].environment.as_ref().unwrap(),
            id,
            &cwd,
            "cron_list",
            json!({}),
        )
        .await;
    }
    let first_scheduler = crate::host::requests::tests::cron_tests::scheduler(
        sessions[&first].environment.as_ref().unwrap(),
    );
    let second_scheduler = crate::host::requests::tests::cron_tests::scheduler(
        sessions[&second].environment.as_ref().unwrap(),
    );
    let job = first_scheduler
        .lock()
        .register("*/5 * * * *", "first session")
        .unwrap();
    let list = crate::host::requests::handle_request(
        "cron/list",
        &json!({"sessionId":first}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(list["jobs"][0]["id"], job);
    assert_eq!(list["jobs"][0]["prompt"], "first session");
    let other = crate::host::requests::handle_request(
        "cron/list",
        &json!({"sessionId":second}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(other["jobs"], json!([]));
    for method in ["cron/toggle", "cron/remove"] {
        let error = crate::host::requests::handle_request(
            method,
            &json!({"sessionId":second,"id":job}),
            &cfg,
            &mut sessions,
            &transport,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, -32603);
        assert!(
            !error.message.is_empty(),
            "MCP tool failure must remain observable"
        );
    }
    assert!(first_scheduler.lock().list_tasks()[0].enabled);
    assert!(second_scheduler.lock().list_tasks().is_empty());
    let toggled = crate::host::requests::handle_request(
        "cron/toggle",
        &json!({"sessionId":first,"id":job}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(toggled, json!({"id":job,"success":true}));
    assert!(!first_scheduler.lock().list_tasks()[0].enabled);
    crate::host::requests::handle_request(
        "cron/remove",
        &json!({"sessionId":first,"id":job}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert!(first_scheduler.lock().list_tasks().is_empty());
    for id in [first, second] {
        crate::host::requests::handle_request(
            "session/close",
            &json!({"sessionId":id}),
            &cfg,
            &mut sessions,
            &transport,
        )
        .await
        .unwrap();
    }
}

#[tokio::test]
#[serial]
async fn cron_endpoint_rejects_absent_closed_and_unsupported_environments() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let capabilities = crate::host::assemble::HostCapabilities {
        cron: false,
        ..Default::default()
    };
    let cfg = crate::host::requests::tests::cron_tests::deployment_with_capabilities(
        &tmp,
        false,
        capabilities,
    )
    .await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let cwd = tmp
        .path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut sessions = HashMap::new();
    for params in [json!({}), json!({"sessionId":"unknown"})] {
        assert_eq!(
            crate::host::requests::handle_request(
                "cron/list",
                &params,
                &cfg,
                &mut sessions,
                &transport
            )
            .await
            .unwrap_err()
            .code,
            -32602
        );
    }
    let id = crate::host::requests::tests::cron_tests::new_session(
        &cfg,
        &mut sessions,
        &transport,
        &cwd,
    )
    .await;
    for method in ["cron/list", "cron/toggle", "cron/remove"] {
        assert_eq!(
            crate::host::requests::handle_request(
                method,
                &json!({"sessionId":id,"id":"job"}),
                &cfg,
                &mut sessions,
                &transport
            )
            .await
            .unwrap_err()
            .code,
            -32601
        );
    }
    sessions.get_mut(&id).unwrap().closing = true;
    assert!(crate::host::requests::handle_request(
        "cron/list",
        &json!({"sessionId":id}),
        &cfg,
        &mut sessions,
        &transport
    )
    .await
    .unwrap_err()
    .message
    .contains("closing"));
    sessions.get_mut(&id).unwrap().closing = false;
    let environment = sessions.get_mut(&id).unwrap().environment.take();
    assert!(crate::host::requests::handle_request(
        "cron/list",
        &json!({"sessionId":id}),
        &cfg,
        &mut sessions,
        &transport
    )
    .await
    .unwrap_err()
    .message
    .contains("environment"));
    sessions.get_mut(&id).unwrap().environment = environment;
    sessions.get_mut(&id).unwrap().cwd = tmp
        .path()
        .join("wrong-scope")
        .to_string_lossy()
        .into_owned();
    assert!(crate::host::requests::handle_request(
        "cron/list",
        &json!({"sessionId":id}),
        &cfg,
        &mut sessions,
        &transport
    )
    .await
    .is_err());
    sessions.get_mut(&id).unwrap().cwd = cwd;
    crate::host::requests::handle_request(
        "session/close",
        &json!({"sessionId":id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
}
