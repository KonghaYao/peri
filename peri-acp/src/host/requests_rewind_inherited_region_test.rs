use super::*;

// ── 继承区在请求级 rewind 里的口径 ─────────────────────────────────────────────

/// 带继承区的会话夹具：parent 的两条历史作为 child 的继承区，child 自有区按 Full 之后的
/// 形状排布（排除段 + **仍然存活的摘要** + 回退目标）。
///
/// 继承区条目不属于本会话，永不参与排除。夹具刻意让继承区以未排除的非 System 条目开头：
/// 清零依据若误用完整逻辑上下文，规则会在这里认出「位置 0 上的存活摘要」，把自有区里仍被
/// 存活摘要覆盖的原文一并清零——用户会看到模型与存储都不认为有效的重复覆盖。
struct InheritedRewindFixture {
    sid: String,
    inherited: Vec<MessageId>,
    own: Vec<MessageId>,
}

async fn register_child_with_inherited_history(
    sessions: &mut HashMap<String, SessionState>,
    cfg: &AcpServerConfig,
    cwd: &str,
) -> InheritedRewindFixture {
    use peri_acp_types::messages::BaseMessage;

    let workspace = cfg
        .session_resources
        .resolve_workspace(std::path::Path::new(cwd))
        .await
        .unwrap();
    let frozen = cfg
        .session_manager
        .build_frozen_data(workspace.cwd.to_str().unwrap());
    let encoded = crate::session::frozen_snapshot::encode_frozen_snapshot(&frozen).unwrap();

    // parent：child 的 frozen 必须逐字节等于 root 已保存快照，因此复用同一份编码结果。
    let parent = "rewind-inherited-parent".to_string();
    cfg.session_resources
        .create_session(&bound_input(&parent, &workspace, encoded.clone()))
        .await
        .unwrap();
    let inherited_payloads = vec![
        PersistedPayload::Message(BaseMessage::human("继承区问题")),
        PersistedPayload::Message(BaseMessage::ai("继承区回答")),
    ];
    cfg.session_resources
        .append_history(&parent, &inherited_payloads)
        .await
        .unwrap();

    // child：继承 parent 到快照点为止的上下文，自有历史另算。
    let sid = "rewind-inherited-child".to_string();
    let mut target = bound_input(&sid, &workspace, encoded);
    target.meta.parent_thread_id = Some(parent.clone());
    target.meta.snapshot_at_message_id = Some(inherited_payloads[1].id());
    cfg.session_resources
        .save_child(&ChildSnapshot {
            target,
            parent_id: parent.clone(),
            root_id: parent,
            inherited: InheritedContext {
                payloads: inherited_payloads.clone(),
                flags: HashMap::new(),
            },
        })
        .await
        .unwrap();

    let own_payloads = vec![
        PersistedPayload::Message(BaseMessage::human("自有原文一")),
        PersistedPayload::Message(BaseMessage::ai("自有原文二")),
        PersistedPayload::Message(BaseMessage::human("存活摘要")),
        PersistedPayload::Message(BaseMessage::ai("回退目标")),
    ];
    cfg.session_resources
        .append_history(&sid, &own_payloads)
        .await
        .unwrap();
    let excluded = MessageFlags {
        excluded: true,
        ..Default::default()
    };
    cfg.session_resources
        .apply_message_projections(
            &ThreadId::from(sid.clone()),
            &[
                (own_payloads[0].id(), excluded.clone()),
                (own_payloads[1].id(), excluded),
            ],
        )
        .await
        .unwrap();

    // 热态：与冷恢复同形的有效视图（排除段的正文不在其中）。
    let effective: Vec<PersistedPayload> = inherited_payloads
        .iter()
        .cloned()
        .chain([own_payloads[2].clone(), own_payloads[3].clone()])
        .collect();
    sessions.insert(
        sid.clone(),
        SessionState {
            session_id: sid.clone(),
            thread_id: sid.clone(),
            cwd: workspace.cwd.to_str().unwrap().to_owned(),
            environment: None,
            closing: false,
            context_ownership: crate::host::ContextOwnership::Resident,
            history_payloads: effective,
            structure_version: 0,
            session_generation: crate::host::next_session_generation(),
            history_window: Default::default(),
            live_delivery: None,
            event_health: Default::default(),
            cancel_token: None,
            interaction_gate: std::sync::Arc::new(tokio::sync::Mutex::new(())),
            host_request_seq: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            continuation_armed: false,
            continuation_epoch: 0,
            continuation_in_flight: false,
            continuation_mq_steering_pending: false,
            frozen: None,
            recall_items: Vec::new(),
            agent_pool: crate::session::agent_pool::AgentPool::new(),
            title: None,
            tags: Vec::new(),
        },
    );
    InheritedRewindFixture {
        sid,
        inherited: inherited_payloads
            .iter()
            .map(PersistedPayload::id)
            .collect(),
        own: own_payloads.iter().map(PersistedPayload::id).collect(),
    }
}

/// 请求级：会话带非空继承区时，回退目标必须在自有区，清零依据只取自有区。
///
/// 存储的边界提交按 id 找目标，继承条目根本不在本会话的行集合里——在那里放行会留下
/// 「内存已截断、存储没动」的分叉，因此必须先拒绝。
#[tokio::test]
async fn test_rewind_with_an_inherited_region_only_clears_own_archived_prefix() {
    use peri_acp_types::messages::BaseMessage;

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
    let fixture =
        register_child_with_inherited_history(&mut sessions, &cfg, tmp.path().to_str().unwrap())
            .await;
    enable_rewind_and_event_caps(&cfg, &fixture.sid);
    let before_history = history_ids(&sessions, &fixture.sid);
    let before_payloads = payload_ids(&sessions, &fixture.sid);

    // 继承条目不是本会话的回退目标：存储按 id 在**本会话的行集合**里找边界，放行会留下
    // 「内存已截断、存储没动」的分叉，因此执行必须先拒绝，且热态一行不动。
    let inherited_target = fixture.inherited[0].as_uuid().to_string();
    let inherited_preview = handle_request(
        "session/rewind-preview",
        &json!({
            "sessionId": fixture.sid,
            "target_message_id": inherited_target,
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let error = handle_request(
        "session/rewind",
        &json!({
            "sessionId": fixture.sid,
            "target_message_id": inherited_target,
            "preview_fingerprint": inherited_preview["preview_fingerprint"],
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32602, "继承条目不得当作回退目标: {error:?}");
    assert_eq!(history_ids(&sessions, &fixture.sid), before_history);
    assert_eq!(payload_ids(&sessions, &fixture.sid), before_payloads);

    let preview = handle_request(
        "session/rewind-preview",
        &json!({
            "sessionId": fixture.sid,
            "target_message_id": fixture.own[3].as_uuid().to_string(),
        }),
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
            "sessionId": fixture.sid,
            "target_message_id": fixture.own[3].as_uuid().to_string(),
            "preview_fingerprint": fingerprint,
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();

    // 有效视图：继承区 + 存活摘要。被摘要覆盖的原文不得复活（它们仍在归档区），目标被移除。
    let expected = vec![fixture.inherited[0], fixture.inherited[1], fixture.own[2]];
    assert_eq!(
        payload_ids(&sessions, &fixture.sid),
        expected,
        "热态必须是「继承区 + 存活摘要」"
    );
    assert_eq!(
        response["history"]
            .as_array()
            .unwrap()
            .iter()
            .map(|message| message["id"].as_str().unwrap().to_string())
            .collect::<Vec<_>>(),
        expected
            .iter()
            .map(|id| id.as_uuid().to_string())
            .collect::<Vec<_>>(),
        "wire 与热态同源"
    );
    let events = agent_events(&mock, "rewind_completed");
    assert_eq!(events.len(), 1);
    let event_messages: Vec<BaseMessage> =
        serde_json::from_str(events[0]["messages_json"].as_str().unwrap()).unwrap();
    assert_eq!(
        event_messages
            .iter()
            .map(BaseMessage::id)
            .collect::<Vec<_>>(),
        expected,
        "事件载荷 = 发布的有效视图（含继承区、不含被覆盖的正文）"
    );

    // 存储：排除段仍在（摘要存活 → 清零依据不成立），目标及其之后的行已删除。
    let snapshot = cfg
        .controller
        .sessions()
        .load_session_snapshot(&ThreadId::from(fixture.sid.clone()))
        .await
        .unwrap();
    assert_eq!(
        snapshot
            .payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>(),
        vec![fixture.own[0], fixture.own[1], fixture.own[2]],
        "自有区只保留到回退目标之前"
    );
    for id in [fixture.own[0], fixture.own[1]] {
        assert!(
            snapshot.flags.get(&id).is_some_and(|flags| flags.excluded),
            "存活摘要覆盖的原文不得被清零: {id:?}"
        );
    }
    assert_eq!(
        snapshot
            .inherited
            .payloads
            .iter()
            .map(PersistedPayload::id)
            .collect::<Vec<_>>(),
        fixture.inherited,
        "继承区不因本会话的 rewind 改变"
    );
}
