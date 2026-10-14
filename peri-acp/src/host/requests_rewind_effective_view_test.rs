use super::*;

/// 归档目标（只存在于存储）仍出现在候选里、可预览、可执行。
///
/// 收缩后的热态只持有有效集合：目标是否合法不得再以 Host 快照为判据。
#[tokio::test]
async fn test_rewind_reaches_target_archived_by_full_compact() {
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
    let (h1, _a1, _h2) = archive_prefix_in_storage(&mut sessions, &cfg, &sid).await;
    enable_rewind_cap(&cfg, &sid);
    let target = h1.as_uuid().to_string();

    let candidates = handle_request(
        "session/rewind-candidates",
        &json!({ "sessionId": sid }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let ids: Vec<String> = candidates["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|message| message["id"].as_str().unwrap().to_string())
        .collect();
    assert!(
        ids.contains(&target),
        "被 Full 归档的 user 消息必须仍是候选，实际: {ids:?}"
    );

    let preview = handle_request(
        "session/rewind-preview",
        &json!({ "sessionId": sid, "target_message_id": target }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let fingerprint = preview["preview_fingerprint"].as_str().unwrap();

    let result = handle_request(
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
    .await;

    assert_eq!(result.unwrap()["status"], "executed");
    // 目标本身及之后的 own 历史（含归档正文）都在存储里消失，热态同步清空。
    let stored = cfg
        .controller
        .sessions()
        .load_session_history(&peri_acp_types::thread::ThreadId::from(sid.clone()))
        .await
        .unwrap();
    assert!(stored.is_empty(), "存储里不应再有 own 历史");
    let state = sessions.get(&sid).unwrap();
    assert!(state.history.is_empty(), "热态展示投影必须清空");
    assert!(state.history_payloads.is_empty(), "热态有效集合必须清空");
}

/// 回退到仍然有效的尾条目：保留前缀里被排除的原文重新生效（摘要被删，旧排除失去依据），
/// 热态与冷恢复看到同一份有效视图。
#[tokio::test]
async fn test_rewind_publishes_the_cold_recovery_view() {
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
    let (h1, a1, h2) = archive_prefix_in_storage(&mut sessions, &cfg, &sid).await;
    enable_rewind_cap(&cfg, &sid);
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

    handle_request(
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
    .unwrap();

    // 冷恢复（load_active_snapshot 就是恢复路径读的那一份）：h2 已删，h1/a1 重新有效。
    let active = cfg
        .controller
        .sessions()
        .load_active_snapshot(&peri_acp_types::thread::ThreadId::from(sid.clone()))
        .await
        .unwrap();
    let cold_ids: Vec<peri_acp_types::messages::MessageId> =
        active.payloads.iter().map(PersistedPayload::id).collect();
    assert_eq!(
        cold_ids,
        vec![h1, a1],
        "被删摘要的排除失去依据 → 保留前缀的原文必须重新有效"
    );

    let state = sessions.get(&sid).unwrap();
    let hot_ids: Vec<peri_acp_types::messages::MessageId> = state
        .history_payloads
        .iter()
        .map(PersistedPayload::id)
        .collect();
    assert_eq!(hot_ids, cold_ids, "热态有效集合必须等于冷恢复视图");
    assert_eq!(
        state
            .history
            .iter()
            .map(peri_acp_types::messages::BaseMessage::id)
            .collect::<Vec<_>>(),
        vec![h1, a1]
    );
    assert!(!hot_ids.contains(&h2), "被移除的目标不得留在任何容器里");
}

/// 回归（S6c / 验收 A19）：rewind 之后**同一会话继续 prompt** 与**冷恢复**必须把同一份
/// 有效视图交给模型，且都不含已退出有效上下文的原文或被移除的摘要。
///
/// 断言贴在模型边界而不是数据边界：链路是 `SessionState::history_payloads`
/// →（`host/prompt.rs` 的 `TurnInput.history_payloads`）→ 执行体
/// `MessageTranscript::with_own_payloads`（`peri-agent` `v2_execute.rs` 用的同一个构造器）
/// → `visible_model_messages()`。热态若多带一份被删摘要、或漏掉重新有效的原文，
/// 这条断言就会红；它同时覆盖「延续同一会话」与「重启后冷恢复」两条路径。
#[tokio::test]
async fn test_rewind_then_prompt_matches_a_cold_restore_at_the_model_boundary() {
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
    let (h1, a1, summary) = archive_prefix_in_storage(&mut sessions, &cfg, &sid).await;
    enable_rewind_cap(&cfg, &sid);

    // 回退到摘要本身：摘要被移除 → 它覆盖的原文重新有效（REQ-T04 的边界语义）。
    let target = summary.as_uuid().to_string();
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
    handle_request(
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
    .unwrap();

    // 热态：同一会话继续（下一条 prompt 拿到的就是这一份）。
    let hot = sessions.get(&sid).unwrap().history_payloads.clone();
    // 冷恢复：重启后从存储读同一份有效视图。
    let cold = cfg
        .controller
        .sessions()
        .load_active_snapshot(&peri_acp_types::thread::ThreadId::from(sid.clone()))
        .await
        .unwrap()
        .payloads;
    assert_eq!(
        hot.iter().map(PersistedPayload::id).collect::<Vec<_>>(),
        vec![h1, a1],
        "热态 = 重新有效的原文，且不含被移除的摘要"
    );

    let model_view = |payloads: Vec<PersistedPayload>| {
        peri_agent::session::MessageTranscript::new()
            .with_own_payloads(payloads)
            .visible_model_messages()
            .expect("模型视图必须可投影")
    };
    let hot_view = model_view(hot);
    let cold_view = model_view(cold);
    // `BaseMessage` 没有 `PartialEq`：按（身份，正文）逐条比较模型真正看到的东西。
    let projection = |view: &[peri_acp_types::messages::BaseMessage]| {
        view.iter()
            .map(|message| (message.id(), message.content()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        projection(&hot_view),
        projection(&cold_view),
        "热态与冷恢复必须给模型同一份历史（无复活正文、无重复摘要）"
    );
    assert_eq!(
        hot_view
            .iter()
            .map(|message| message.id())
            .collect::<Vec<_>>(),
        vec![h1, a1],
        "模型请求里的历史正是重新生效的那两条"
    );
    assert!(
        !hot_view.iter().any(|message| message.id() == summary),
        "被移除的摘要不得回到模型请求"
    );
}

/// 保留前缀里仍成立的排除必须原样保留：摘要活着时，被它覆盖的正文不因 rewind 复活。

///
/// 与上一个用例互为反面——清零集合为空的分支，热态与 wire 都只能是有效视图
/// （目标之后的原文被移除，但目标之前的归档区不受影响）。
#[tokio::test]
async fn test_rewind_keeps_still_covered_bodies_archived() {
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
    // 追加一轮：目标落在摘要（history[2]）之后，回退不会删掉摘要本身。
    let late = peri_acp_types::messages::BaseMessage::ai("late answer");
    cfg.session_resources
        .append_history(
            &peri_acp_types::thread::ThreadId::from(sid.clone()),
            &[PersistedPayload::Message(late.clone())],
        )
        .await
        .unwrap();
    sessions.get_mut(&sid).unwrap().history.push(late.clone());
    let (h1, a1, h2) = archive_prefix_in_storage(&mut sessions, &cfg, &sid).await;
    enable_rewind_cap(&cfg, &sid);
    let target = late.id().as_uuid().to_string();

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

    let response = handle_request(
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
    .unwrap();

    let stored: Vec<peri_acp_types::messages::MessageId> = cfg
        .controller
        .sessions()
        .load_session_history(&peri_acp_types::thread::ThreadId::from(sid.clone()))
        .await
        .unwrap()
        .iter()
        .map(PersistedPayload::id)
        .collect();
    assert_eq!(stored, vec![h1, a1, h2], "摘要仍在 → 归档区原样保留");

    let active = cfg
        .controller
        .sessions()
        .load_active_snapshot(&peri_acp_types::thread::ThreadId::from(sid.clone()))
        .await
        .unwrap();
    let cold_ids: Vec<peri_acp_types::messages::MessageId> =
        active.payloads.iter().map(PersistedPayload::id).collect();
    assert_eq!(cold_ids, vec![h2], "被覆盖的正文不得复活");

    let state = sessions.get(&sid).unwrap();
    assert_eq!(
        state
            .history_payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>(),
        cold_ids
    );
    assert_eq!(
        response["history"]
            .as_array()
            .unwrap()
            .iter()
            .map(|message| message["id"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        vec![h2.as_uuid().to_string()],
        "wire 里的 history 必须是有效视图，不是含归档正文的完整前缀"
    );
}

/// F4 回归：`RewindCompleted` 的载荷必须是**有效视图**（与热态、wire `history` 同源），
/// 不是含「仍被存活摘要覆盖的正文」的 canonical 前缀。
///
/// TUI 以该事件为事实源重建 transcript 与候选弹窗；载荷退回 canonical 前缀时，用户会看到
/// 模型与存储都不认为有效的条目（这正是 F4 逃逸的路径）。
#[tokio::test]
async fn test_rewind_completed_event_carries_the_effective_view() {
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
    let mock = Arc::new(MockTransport::default());
    let transport: Arc<dyn crate::transport::AcpTransport> = mock.clone();
    let sid =
        register_session_with_history(&mut sessions, tmp.path().to_str().unwrap(), &cfg).await;
    // 目标落在摘要之后：摘要活着 → canonical 前缀含被覆盖的正文，有效视图不含。
    let late = peri_acp_types::messages::BaseMessage::ai("late answer");
    cfg.session_resources
        .append_history(
            &peri_acp_types::thread::ThreadId::from(sid.clone()),
            &[PersistedPayload::Message(late.clone())],
        )
        .await
        .unwrap();
    sessions.get_mut(&sid).unwrap().history.push(late.clone());
    let (h1, a1, h2) = archive_prefix_in_storage(&mut sessions, &cfg, &sid).await;
    enable_rewind_and_event_caps(&cfg, &sid);
    let target = late.id().as_uuid().to_string();

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

    let response = handle_request(
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
    .unwrap();

    let events = agent_events(&mock, "rewind_completed");
    assert_eq!(events.len(), 1, "应恰好发一条重建信号: {events:?}");
    let messages_json = events[0]
        .get("messages_json")
        .and_then(Value::as_str)
        .expect("messages_json 缺失");
    let messages: Vec<peri_acp_types::messages::BaseMessage> =
        serde_json::from_str(messages_json).unwrap();
    let event_ids: Vec<MessageId> = messages
        .iter()
        .map(peri_acp_types::messages::BaseMessage::id)
        .collect();
    assert_eq!(
        event_ids,
        vec![h2],
        "事件载荷必须是有效视图：仍被存活摘要覆盖的正文（{h1:?}/{a1:?}）不得出现"
    );
    // 三处同源：事件载荷 = 热态有效集合 = wire `history`。
    assert_eq!(payload_ids(&sessions, &sid), vec![h2]);
    assert_eq!(
        response["history"]
            .as_array()
            .unwrap()
            .iter()
            .map(|message| message["id"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        vec![h2.as_uuid().to_string()]
    );
}

/// 边界语义未变：ACP 用户 rewind 用 RemoveFrom（目标本身也被移除），transcript 的
/// KeepThrough 保留目标——两者仍由同一份边界规则区分。
#[tokio::test]
async fn test_rewind_remove_from_drops_the_target_itself() {
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
    enable_rewind_cap(&cfg, &sid);
    let (h1, a1, h2) = {
        let s = sessions.get(&sid).unwrap();
        (s.history[0].id(), s.history[1].id(), s.history[2].id())
    };
    let ids = vec![h1, a1, h2];
    assert_eq!(
        peri_acp_types::store::history::rewind_keep_len(
            &ids,
            peri_acp_types::session_resources::RewindBoundary::RemoveFrom(h2),
        )
        .unwrap(),
        2,
        "RemoveFrom 移除目标本身"
    );
    assert_eq!(
        peri_acp_types::store::history::rewind_keep_len(
            &ids,
            peri_acp_types::session_resources::RewindBoundary::KeepThrough(h2),
        )
        .unwrap(),
        3,
        "KeepThrough 保留目标本身"
    );

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
    handle_request(
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
    .unwrap();

    let stored: Vec<peri_acp_types::messages::MessageId> = cfg
        .controller
        .sessions()
        .load_session_history(&peri_acp_types::thread::ThreadId::from(sid.clone()))
        .await
        .unwrap()
        .iter()
        .map(PersistedPayload::id)
        .collect();
    assert_eq!(stored, vec![h1, a1], "目标本身必须被移除");
    assert_eq!(
        history_ids(&sessions, &sid),
        vec![h1.as_uuid().to_string(), a1.as_uuid().to_string()]
    );
}
