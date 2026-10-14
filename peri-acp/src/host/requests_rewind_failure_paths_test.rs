use super::*;

/// F1 回归：提交时边界行已不在存储里（并发删除）⇒ 存储一行未动，热态与结构版本都不得前进。
///
/// 旧行为是「存储返回 Ok(什么都没做)，ACP 照样发布已截断 + 已清零的热态」——冷恢复与
/// 运行中的上下文就此分叉。现在必须报可区分错误（`-32013` + `data.reason`）、不发
/// `RewindCompleted`（TUI 会以它为事实源重建 transcript）。
#[tokio::test]
async fn test_rewind_missing_boundary_changes_nothing_and_reports_it() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let mut cfg = make_server_config(peri_config, provider, &tmp).await;
    let mut sessions = HashMap::new();
    let mock = Arc::new(MockTransport::default());
    let transport: Arc<dyn crate::transport::AcpTransport> = mock.clone();
    let sid =
        register_session_with_history(&mut sessions, tmp.path().to_str().unwrap(), &cfg).await;
    let (h1, a1, h2) = archive_prefix_in_storage(&mut sessions, &cfg, &sid).await;
    enable_rewind_and_event_caps(&cfg, &sid);
    let before_history = history_ids(&sessions, &sid);
    let before_payloads = payload_ids(&sessions, &sid);
    let target = h2.as_uuid().to_string();

    // 注入：提交那一刻边界行已被并发删除（真实门面的未知边界语义 = 无变更 + BoundaryMissing）。
    cfg.controller = Arc::new(peri_controller::Controller::new(Arc::new(
        RewindFaultStore {
            inner: Arc::clone(&cfg.session_resources),
            fault: RewindFault::BoundaryVanishesAtCommit(h2),
        },
    )));

    let preview = handle_request(
        "session/rewind-preview",
        &json!({ "sessionId": sid, "target_message_id": target }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let fingerprint = preview["preview_fingerprint"].as_str().unwrap().to_string();

    let error = handle_request(
        "session/rewind",
        &json!({
            "sessionId": sid,
            "target_message_id": target,
            "preview_fingerprint": fingerprint,
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();

    assert_eq!(
        error.code, -32013,
        "未生效的提交必须与普通失败可区分: {error:?}"
    );
    assert_eq!(
        error
            .data
            .as_ref()
            .and_then(|data| data.get("reason"))
            .and_then(Value::as_str),
        Some("rewind_boundary_missing"),
        "分类必须稳定: {error:?}"
    );
    assert!(
        error.message.contains("存储未改动"),
        "报文要说清副作用: {error:?}"
    );
    assert_eq!(
        history_ids(&sessions, &sid),
        before_history,
        "热态展示投影不得改变"
    );
    assert_eq!(
        payload_ids(&sessions, &sid),
        before_payloads,
        "热态有效集合不得改变"
    );
    assert_eq!(
        sessions.get(&sid).unwrap().structure_version,
        0,
        "未生效的提交不得推进结构版本（内存不得领先于已提交事实）"
    );
    assert!(
        agent_events(&mock, "rewind_completed").is_empty(),
        "提交没生效就不得发重建信号: {:?}",
        mock.notifications()
    );
    // 存储：除并发删掉的那一行，一行未动——尤其是保留前缀的 `excluded` 没有被清零
    //（清零与删除同批，批不生效就一条都不能落）。
    let snapshot = cfg
        .controller
        .sessions()
        .load_session_snapshot(&ThreadId::from(sid.clone()))
        .await
        .unwrap();
    assert_eq!(
        snapshot
            .payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>(),
        vec![h1, a1]
    );
    for id in [h1, a1] {
        assert!(
            snapshot.flags.get(&id).is_some_and(|flags| flags.excluded),
            "保留前缀的排除不得被清零: {id:?}"
        );
    }
}

/// durable 提交失败：内存保持提交前状态，错误按原样上抛（-32603）。
#[tokio::test]
async fn test_rewind_commit_failure_leaves_memory_untouched() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let mut cfg = make_server_config(peri_config, provider, &tmp).await;
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let sid =
        register_session_with_history(&mut sessions, tmp.path().to_str().unwrap(), &cfg).await;
    let (h1, a1, h2) = archive_prefix_in_storage(&mut sessions, &cfg, &sid).await;
    enable_rewind_cap(&cfg, &sid);
    let before = history_ids(&sessions, &sid);
    let before_payloads: Vec<peri_acp_types::messages::MessageId> = sessions
        .get(&sid)
        .unwrap()
        .history_payloads
        .iter()
        .map(PersistedPayload::id)
        .collect();

    // 注入：提交失败（读取照常成功）。
    cfg.controller = Arc::new(peri_controller::Controller::new(Arc::new(
        RewindFaultStore {
            inner: Arc::clone(&cfg.session_resources),
            fault: RewindFault::CommitFails,
        },
    )));
    let target = h2.as_uuid().to_string();
    let preview = handle_request(
        "session/rewind-preview",
        &json!({ "sessionId": sid, "target_message_id": target }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let fingerprint = preview["preview_fingerprint"].as_str().unwrap().to_string();

    let error = handle_request(
        "session/rewind",
        &json!({
            "sessionId": sid,
            "target_message_id": target,
            "preview_fingerprint": fingerprint,
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, -32603);
    assert!(
        error.message.contains("durable rewind commit failed"),
        "原始错误原因必须保留: {error:?}"
    );
    assert_eq!(history_ids(&sessions, &sid), before, "热态展示投影不得改变");
    let after_payloads: Vec<peri_acp_types::messages::MessageId> = sessions
        .get(&sid)
        .unwrap()
        .history_payloads
        .iter()
        .map(PersistedPayload::id)
        .collect();
    assert_eq!(after_payloads, before_payloads, "热态有效集合不得改变");
    assert_eq!(
        sessions.get(&sid).unwrap().structure_version,
        0,
        "失败的提交不得推进结构版本（内存不得领先于已提交事实）"
    );
    // 存储同样是提交前状态：完整历史仍是三条，排除标记仍在。
    let stored: Vec<peri_acp_types::messages::MessageId> = cfg
        .controller
        .sessions()
        .load_session_history(&peri_acp_types::thread::ThreadId::from(sid.clone()))
        .await
        .unwrap()
        .iter()
        .map(PersistedPayload::id)
        .collect();
    assert_eq!(stored, vec![h1, a1, h2]);
}

#[tokio::test]
async fn test_rewind_methods_require_explicit_capability() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp).await;
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let sid =
        register_session_with_history(&mut sessions, tmp.path().to_str().unwrap(), &cfg).await;
    cfg.session_manager
        .caps_registry()
        .insert(sid.clone(), PeriCaps::default());
    let target = sessions.get(&sid).unwrap().history[0]
        .id()
        .as_uuid()
        .to_string();
    let original: Vec<_> = sessions
        .get(&sid)
        .unwrap()
        .history
        .iter()
        .map(|message| (message.id().as_uuid().to_string(), message.content()))
        .collect();

    for (method, params) in [
        ("session/rewind-candidates", json!({"sessionId": sid})),
        (
            "session/rewind-preview",
            json!({"sessionId": sid, "target_message_id": target}),
        ),
        (
            "session/rewind",
            json!({"sessionId": sid, "target_message_id": target}),
        ),
    ] {
        let error = handle_request(method, &params, &cfg, &mut sessions, &transport)
            .await
            .unwrap_err();
        assert_eq!(error.code, -32601);
        assert_eq!(error.message, "peri.rewind capability not negotiated");
    }
    let current: Vec<_> = sessions
        .get(&sid)
        .unwrap()
        .history
        .iter()
        .map(|message| (message.id().as_uuid().to_string(), message.content()))
        .collect();
    assert_eq!(current, original);
}

#[tokio::test]
async fn test_rewind_rejects_stale_preview_without_mutating_history() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp).await;
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let sid =
        register_session_with_history(&mut sessions, tmp.path().to_str().unwrap(), &cfg).await;
    cfg.session_manager.caps_registry().insert(
        sid.clone(),
        PeriCaps {
            rewind: true,
            ..PeriCaps::default()
        },
    );
    let target_id = sessions.get(&sid).unwrap().history[0]
        .id()
        .as_uuid()
        .to_string();
    let preview = handle_request(
        "session/rewind-preview",
        &json!({ "sessionId": sid, "target_message_id": target_id }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let preview_fingerprint = preview["preview_fingerprint"].as_str().unwrap().to_string();

    // 事实源是存储：新消息落进存储后，旧预览的 fingerprint 不再成立。
    cfg.session_resources
        .append_history(
            &peri_acp_types::thread::ThreadId::from(sid.clone()),
            &[PersistedPayload::Message(
                peri_acp_types::messages::BaseMessage::ai("late answer"),
            )],
        )
        .await
        .unwrap();
    let before = sessions.get(&sid).unwrap().history.len();
    let stored_before = cfg
        .controller
        .sessions()
        .load_session_history(&peri_acp_types::thread::ThreadId::from(sid.clone()))
        .await
        .unwrap()
        .len();
    let error = handle_request(
        "session/rewind",
        &json!({
            "sessionId": sid,
            "target_message_id": target_id,
            "preview_fingerprint": preview_fingerprint,
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();

    assert!(error.message.contains("preview is stale"));
    assert_eq!(sessions.get(&sid).unwrap().history.len(), before);
    assert_eq!(
        cfg.controller
            .sessions()
            .load_session_history(&peri_acp_types::thread::ThreadId::from(sid.clone()))
            .await
            .unwrap()
            .len(),
        stored_before,
        "被拒的过期执行不得改动存储"
    );
}
