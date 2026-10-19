use super::*;

/// 回归（S6c / 验收 A19「旧 cursor 失效」）：rewind 提交成功后，已发出的历史分页游标
/// **在进入存储之前**被拒（结构版本），且与「锚点被删」得到同一个可区分错误。
///
/// 本用例刻意让锚点在 rewind 后仍然存在（回退目标是一条更晚的消息），因此失败原因
/// 只能是结构版本；同时用「新游标可用」证明存储侧一切正常。
#[tokio::test]
async fn test_rewind_expires_previously_issued_history_cursors() {
    use crate::host::history_page::HistoryPageRequest;
    use peri_acp_types::history_page::HistoryCursorEnvelope;

    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let shared: crate::host::SharedSessions = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let sid = {
        let mut sessions = shared.lock().await;
        register_session_with_history(&mut sessions, tmp.path().to_str().unwrap(), &cfg).await
    };
    let (_h1, _a1, anchor) = {
        let mut sessions = shared.lock().await;
        archive_prefix_in_storage(&mut sessions, &cfg, &sid).await
    };
    enable_rewind_cap(&cfg, &sid);
    // 更晚的一条自有消息：回退到它，锚点（摘要）在提交后仍然存活。
    let tail = peri_acp_types::messages::BaseMessage::human("第三轮用户问题");
    cfg.session_resources
        .append_history(&sid, &[PersistedPayload::Message(tail.clone())])
        .await
        .unwrap();

    let pagination = PeriCaps {
        history_pagination: true,
        ..PeriCaps::default()
    };
    let old_cursor = HistoryCursorEnvelope::new(
        0,
        "snapshot-before-rewind",
        crate::dispatch::initial_history_cursor(cfg.controller.as_ref(), &sid)
            .await
            .unwrap()
            .next_cursor
            .expect("归档区存在时必须给出更早页"),
    );
    assert_eq!(old_cursor.cursor.anchor, anchor, "首游标锚在有效区最旧一条");
    let request = HistoryPageRequest {
        session_id: sid.clone(),
        cursor: old_cursor.clone(),
        limit: 50,
    };
    let page = crate::host::history_page::handle_page(&cfg, &shared, &pagination, &request)
        .await
        .expect("rewind 之前该游标必须可用");
    assert!(
        !page["items"].as_array().unwrap().is_empty(),
        "归档区可翻页"
    );

    let target = tail.id().as_uuid().to_string();
    let preview = {
        let mut sessions = shared.lock().await;
        handle_request(
            "session/rewind-preview",
            &json!({ "sessionId": sid, "target_message_id": target }),
            &cfg,
            &mut sessions,
            &transport,
        )
        .await
        .unwrap()
    };
    let fingerprint = preview["preview_fingerprint"].as_str().unwrap().to_string();
    {
        let mut sessions = shared.lock().await;
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
    }

    let cold = cfg
        .controller
        .sessions()
        .load_active_snapshot(&peri_acp_types::thread::ThreadId::from(sid.clone()))
        .await
        .unwrap()
        .payloads;
    assert!(
        cold.iter().any(|payload| payload.id() == anchor),
        "锚点仍在冷恢复视图里：失败原因只能是结构版本"
    );

    let error = crate::host::history_page::handle_page(&cfg, &shared, &pagination, &request)
        .await
        .expect_err("rewind 之后旧游标必须失效");
    assert_eq!(error.code, -32012, "与「锚点被删」同一个可区分错误");

    // 新游标（同一份历史、新版本）继续可用：存储侧没有被这次拒绝影响。
    let version = shared.lock().await.get(&sid).unwrap().structure_version;
    assert_eq!(version, 1, "一次成功的 rewind 只推进一次结构版本");
    let fresh = crate::dispatch::initial_history_cursor(cfg.controller.as_ref(), &sid)
        .await
        .unwrap()
        .next_cursor
        .expect("摘要仍活着，更早方向仍有页");
    let fresh = HistoryCursorEnvelope::new(version, "snapshot-after-rewind", fresh);
    crate::host::history_page::handle_page(
        &cfg,
        &shared,
        &pagination,
        &HistoryPageRequest {
            session_id: sid.clone(),
            cursor: fresh,
            limit: 50,
        },
    )
    .await
    .expect("rewind 之后的新游标必须照常可读");
}
