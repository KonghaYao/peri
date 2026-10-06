use super::*;

#[tokio::test]
#[serial]
async fn cron_endpoint_uses_session_scheduler_and_rejects_cross_session_jobs() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let cfg = deployment(&tmp, false).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let cwd = tmp
        .path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut sessions = HashMap::new();
    let first = new_session(&cfg, &mut sessions, &transport, &cwd).await;
    let second = new_session(&cfg, &mut sessions, &transport, &cwd).await;
    let first_scheduler = scheduler(sessions[&first].environment.as_ref().unwrap());
    let second_scheduler = scheduler(sessions[&second].environment.as_ref().unwrap());
    let job = first_scheduler
        .lock()
        .register("*/5 * * * *", "first session")
        .unwrap();
    let list = handle_request(
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
    let other = handle_request(
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
        let error = handle_request(
            method,
            &json!({"sessionId":second,"id":job}),
            &cfg,
            &mut sessions,
            &transport,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, -32602);
    }
    assert!(first_scheduler.lock().list_tasks()[0].enabled);
    assert!(second_scheduler.lock().list_tasks().is_empty());
    let toggled = handle_request(
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
    handle_request(
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
        handle_request(
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
    let cfg = deployment_with_capabilities(&tmp, false, capabilities).await;
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
            handle_request("cron/list", &params, &cfg, &mut sessions, &transport)
                .await
                .unwrap_err()
                .code,
            -32602
        );
    }
    let id = new_session(&cfg, &mut sessions, &transport, &cwd).await;
    for method in ["cron/list", "cron/toggle", "cron/remove"] {
        assert_eq!(
            handle_request(
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
    assert!(handle_request(
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
    assert!(handle_request(
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
    assert!(handle_request(
        "cron/list",
        &json!({"sessionId":id}),
        &cfg,
        &mut sessions,
        &transport
    )
    .await
    .is_err());
    sessions.get_mut(&id).unwrap().cwd = cwd;
    handle_request(
        "session/close",
        &json!({"sessionId":id}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
}
