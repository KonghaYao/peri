use super::*;

/// session/new 首发无 mcp 条目；MCP 发现完成后经「发现管线直接写入命令
/// 注册表（A3 `mark_source_completed`）→ **注册表 on_change（投影重建唯一
/// 触发源）**」重发，第二次通知含 mcp 条目（条目级 `_meta.periKind`；
/// update 级 `mcpSkillNames` 镜像键已退役，Phase 6 D1）；注册表
/// 内容变化（unregister）亦触发重发且投影收缩（Phase 6 A4：McpSkillRegistry
/// 挂点已删，命令面变更统一经注册表）。

#[tokio::test]
async fn test_available_commands_update_mcp_callback_resend() {
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
    let transport: Arc<MockTransport> = Arc::new(MockTransport::default());
    let transport_dyn: Arc<dyn crate::transport::AcpTransport> = transport.clone();

    let result = handle_request(
        "session/new",
        &json!({ "cwd": tmp.path().to_str().unwrap() }),
        &cfg,
        &mut sessions,
        &transport_dyn,
    )
    .await
    .unwrap();
    let sid = result["sessionId"].as_str().unwrap().to_string();
    super::session_lifecycle::after_new_response(&cfg, &transport_dyn, &sid).await;

    // 命令面投影的观测口径：**只数** `session/update` 里的 available_commands_update。
    // 同一会话另有 `peri/unstable_event` 通道（如会话任务投影的 bg-task-snapshot），
    // 与本用例的「投影重发恰一次」断言无关。
    let command_updates = || -> Vec<Value> {
        transport
            .notifications()
            .into_iter()
            .filter_map(|(method, payload)| {
                (method == "session/update"
                    && payload["update"]["sessionUpdate"] == "available_commands_update")
                    .then_some(payload)
            })
            .collect()
    };

    // 首发：registry 尚未发现 → availableCommands 无 mcp 条目
    let notifications = command_updates();
    assert_eq!(notifications.len(), 1, "首发仅一条命令面通知");
    let update0 = &notifications[0]["update"];
    assert_eq!(update0["sessionUpdate"], "available_commands_update");
    let commands0 = update0["availableCommands"].as_array().unwrap();
    assert!(
        commands0
            .iter()
            .all(|c| c["name"] != "mcp__demo__hello" && c["name"] != "demo:hello"),
        "首发不得含 mcp 条目"
    );

    // 变更命令注册表（A4 后 MCP 条目由发现管线直接写入，不再经
    // McpSkillRegistry on_change 对账——挂点已删）→ 注册表 on_change
    // 触发投影重发（A3 `mark_source_completed` 同语义）。
    let command_registry = cfg
        .session_manager
        .command_registry_for(&sid)
        .expect("session 应持有命令注册表");
    let token: peri_acp_types::mcp_skills::HandleToken = Arc::new(42u32);
    command_registry.mark_source_started("demo", token.clone());
    command_registry.mark_source_completed(
        "demo",
        token,
        vec![peri_acp_types::command::command_route::RouteEntry {
            fullname: "demo:hello".into(),
            aliases: Vec::new(),
            description: "MCP skill hello".into(),
            kind: peri_acp_types::command::command_route::CommandEntryKind::McpSkill,
            category: None,
            args_schema: None,
            handler: Arc::new(crate::session::command::AgentPassthrough),
            provenance: peri_acp_types::command::command_route::CommandProvenance {
                source: peri_acp_types::command::command_route::CommandSource::Mcp {
                    server: "demo".into(),
                },
                // 对齐生产语义（skill_discovery.rs `mcp_route_entries` 产出
                // Discovered；handler 为跨 crate 占位等价——peri-acp 无法
                // 引用 peri-middlewares 的 McpSkillReleaser，用
                // AgentPassthrough 占位，本用例只断言触发源 = 注册表
                // on_change，与 handler/lifecycle 无关）。
                lifecycle: peri_acp_types::command::command_route::CommandLifecycle::Discovered,
            },
        }],
    );

    // 回调经 tokio::spawn 异步发送 → 轮询短等待
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while command_updates().len() < 2 {
        assert!(std::time::Instant::now() < deadline, "等待重发通知超时");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    // 稳定窗口：异步重发落袋后再等一拍，断言重发**恰一次**（注册表
    // on_change 只触发一次，不得重复重发——A5「重发恰一次断言不变」）
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(
        command_updates().len(),
        2,
        "mark_source_completed 应触发重发恰一次（首发 + 重发），实际: {:?}",
        transport
            .notifications()
            .iter()
            .map(|(m, p)| (m.as_str(), p["update"]["sessionUpdate"].clone()))
            .collect::<Vec<_>>()
    );

    // 静默断言（验收 13 ACP 半边）：命令面除 available_commands_update 外无其它
    // `session/update` 类型（`peri/unstable_event` 走独立通道，不属本断言面）。
    let notifications = transport.notifications();
    assert!(
        notifications
            .iter()
            .filter(|(m, _)| m == "session/update")
            .all(|(_, p)| p["update"]["sessionUpdate"] == "available_commands_update"),
        "命令面不得出现其它 session/update 类型，实际: {:?}",
        notifications
            .iter()
            .map(|(m, p)| (m.as_str(), p["update"]["sessionUpdate"].clone()))
            .collect::<Vec<_>>()
    );

    let updates = command_updates();
    let update1 = &updates[1]["update"];
    let commands1 = update1["availableCommands"].as_array().unwrap();
    let hello = commands1
        .iter()
        .find(|c| c["name"] == "demo:hello")
        .expect("第二次通知应含 mcp 条目（demo:hello 全名）");
    assert_eq!(
        hello["_meta"]["periKind"], "mcp_skill",
        "mcp 条目 kind 入条目级 _meta（mcpSkillNames 镜像键已退役）"
    );

    // 触发源 = 注册表：unregister 内置条目 → 重发且投影收缩（不再依赖
    // McpSkillRegistry 直接重发）
    let before = transport.notifications().len();
    assert!(
        command_registry.unregister("core:loop"),
        "unregister 应命中"
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let shrunk = transport.notifications().iter().any(|(_, p)| {
            p["update"]["availableCommands"]
                .as_array()
                .map(|a| a.iter().all(|c| c["name"] != "loop"))
                .unwrap_or(false)
        });
        if shrunk {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "等待注册表 on_change 重发超时（before={before}）"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// session/load 与 session/new 同构（决策 B 扩展）：同样预热 MCP skill
/// 发现。pool 存在但无已连接 server（pending）时 prewarm 空跑不 panic、
/// 广播正常发出；已连接 server 的发现行为由 middleware 层单测覆盖
/// （`prewarm_discovery_triggers_idempotent_discovery`）。
#[tokio::test]
async fn test_session_load_prewarms_mcp_discovery_smoke() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let mut cfg = make_server_config(peri_config, provider, &tmp).await;
    cfg.session_manager.set_pending_caps(PeriCaps::default());
    let pool = Arc::new(peri_middlewares::mcp::McpClientPool::new_pending());
    // 夹具补生产步骤：生产装配给每个池 spawn `run_initialize`
    // （`peri-acp/src/host/assemble.rs`），初始化收口即发布「零 server」的目录事实；
    // 手工造的 pending 池没有这一代，session/load 的 workspace task scope 对账
    // （`wait_for_task_owner_catalog`）会一直等到超时。`mark_initialized` 正是生产
    // 「空配置」终态（`peri-middlewares/src/mcp/initialize.rs` 的空集合分支），
    // 池本身仍无任何已连接 server（本用例的 prewarm 空跑面不变）。
    pool.mark_initialized();
    cfg.mcp_pool = Some(pool);
    create_bound_fixture(&cfg, tmp.path().to_str().unwrap(), Some("s1")).await;
    let mut sessions = HashMap::new();
    let transport: Arc<MockTransport> = Arc::new(MockTransport::default());
    let transport_dyn: Arc<dyn crate::transport::AcpTransport> = transport.clone();

    let result = handle_request(
        "session/load",
        &json!({ "sessionId": "s1", "cwd": tmp.path().to_str().unwrap() }),
        &cfg,
        &mut sessions,
        &transport_dyn,
    )
    .await
    .unwrap();
    assert!(
        result.get("modes").is_some(),
        "session/load 应返回 modes/configOptions"
    );
    // prewarm 空跑路径（pending pool 无已连接 server）不 panic，广播正常发出
    assert!(
        transport.notifications().iter().any(|(m, p)| {
            m == "session/update" && p["update"]["sessionUpdate"] == "available_commands_update"
        }),
        "session/load 应广播 available_commands_update"
    );
}

/// [回归测试] 冷启动加载同一 session 必须恢复创建时的 frozen prompt。
///
/// 历史问题：`session/load` 只恢复消息，却重新扫描当前 CLAUDE.md/skills/date；
/// OpenAI/Cursor 因而在恢复后的首请求看到“新 system + 旧 history”，首轮重建
/// cache prefix、后续轮才恢复命中。
/// 核对点 8 覆盖缺口（P2-3）：`set_pending_caps` 带 `ui_commands` 明细 →
/// session/new → 断言 ui 面板条目随 caps 明细出现（name = Level1 裸名、
/// `periKind=panel`、`periCategory=ui`、alias 注入），未协商的默认明细不出现。

#[tokio::test]
async fn test_available_commands_update_ui_entries_from_caps_details() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp).await;
    // 协商 caps：仅上送两条自定义 ui 明细（大写 name 验证小写归一 + alias 透传）
    cfg.session_manager.set_pending_caps(PeriCaps {
        ui_commands: vec![
            peri_acp_types::command::command_route::UiCommandSpec {
                name: "gallery".into(),
                description: "Open the gallery panel".into(),
                aliases: vec!["gal".into()],
                args: None,
            },
            peri_acp_types::command::command_route::UiCommandSpec {
                name: "Zoom".into(),
                description: "Zoom panel".into(),
                ..Default::default()
            },
        ],
        ..Default::default()
    });
    let mut sessions = HashMap::new();
    let transport: Arc<MockTransport> = Arc::new(MockTransport::default());
    let transport_dyn: Arc<dyn crate::transport::AcpTransport> = transport.clone();

    let result = handle_request(
        "session/new",
        &json!({ "cwd": tmp.path().to_str().unwrap() }),
        &cfg,
        &mut sessions,
        &transport_dyn,
    )
    .await
    .unwrap();
    let sid = result["sessionId"].as_str().unwrap();
    super::session_lifecycle::after_new_response(&cfg, &transport_dyn, sid).await;

    let notifications = transport.notifications();
    assert_eq!(notifications.len(), 1, "首发仅一条 session/update 通知");
    let update = &notifications[0].1["update"];
    assert_eq!(update["sessionUpdate"], "available_commands_update");
    let commands = update["availableCommands"].as_array().unwrap();
    let by_name = |n: &str| {
        commands
            .iter()
            .find(|c| c["name"] == n)
            .unwrap_or_else(|| panic!("条目 {n} 应存在: {:?}", commands))
    };
    // ui 条目随 caps 明细出现：name = 裸名 / periKind=panel / periLevel=1 /
    // periCategory=ui（Level1 域归属只经条目级 kind 下发，name 不带域前缀）
    let gallery = by_name("gallery");
    assert_eq!(
        gallery["_meta"]["periKind"], "panel",
        "ui 条目 kind = panel"
    );
    assert_eq!(gallery["_meta"]["periLevel"], 1, "core/ui 域 level = 1");
    assert_eq!(gallery["_meta"]["periCategory"], "ui");
    assert_eq!(
        gallery["_meta"]["periAliases"],
        json!(["gal"]),
        "caps 明细 alias 应透传注入"
    );
    assert_eq!(
        by_name("zoom")["_meta"]["periKind"],
        "panel",
        "name 应小写归一（Zoom → zoom）"
    );
    // 未协商的默认明细不得出现（门控反转：只广播客户端声明的明细）——
    // panel 条目恰为协商的 gallery/zoom 两条（help 未协商不注册；core:clear
    // 的内置裸名条目 kind=command，不构成 panel）
    let panels: Vec<&str> = commands
        .iter()
        .filter(|c| c["_meta"]["periKind"] == "panel")
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        panels,
        ["gallery", "zoom"],
        "仅协商的 ui 明细注册为 panel 条目: {panels:?}"
    );
    // 基座内置仍在（注册表投影，Level1 裸名）
    assert!(
        commands.iter().any(|c| c["name"] == "compact"),
        "基座内置条目应保留"
    );
}

/// 防引用环断言：注册回调后清零外部强引用（持 Weak 观察）→ upgrade 必须
/// 变 None——注册表 on_change 回调不得捕获注册表 Arc 强引用（重发闭包只
/// 持 Weak；Phase 6 A4 后 McpSkillRegistry 不再经本函数挂回调）。
#[tokio::test]
async fn test_available_commands_update_callbacks_do_not_hold_strong_refs() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let _cfg = make_server_config(peri_config, provider, &tmp).await;
    let transport: Arc<MockTransport> = Arc::new(MockTransport::default());
    let transport_dyn: Arc<dyn crate::transport::AcpTransport> = transport.clone();

    let command_registry = Arc::new(peri_acp_types::command_registry::CommandRegistry::new());
    let weak_cmd = Arc::downgrade(&command_registry);

    crate::host::notify::send_available_commands_update(
        &transport_dyn,
        "anti-cycle-session",
        &PeriCaps::all_enabled(),
        Some(command_registry), // 唯一强引用移入函数，返回后即释放
        false,
    )
    .await;

    // 外部强引用清零后：注册表 on_change 回调只持 Weak(注册表)（重发闭包），
    // 注册表必须能被回收（无引用环）。
    assert!(
        weak_cmd.upgrade().is_none(),
        "注册表应可回收（on_change 回调不得捕获强引用）；upgrade 应为 None"
    );
    assert_eq!(weak_cmd.strong_count(), 0, "注册表 strong_count 应归零");
}

#[tokio::test]
async fn test_mcp_list_requires_negotiated_oauth_capability() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-test-placeholder",
        "gpt-test",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let mut cfg = make_server_config(peri_config, provider, &tmp).await;
    cfg.session_manager.set_pending_caps(PeriCaps::default());
    cfg.mcp_pool = Some(Arc::new(peri_middlewares::mcp::McpClientPool::new_pending()));
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let error = handle_request(
        "mcp/list",
        &json!({}),
        &cfg,
        &mut HashMap::new(),
        &transport,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32601);
    assert_eq!(error.message, "peri.oauth capability not negotiated");
}

#[tokio::test]
async fn test_mcp_list_returns_bounded_safe_empty_snapshot_when_negotiated() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-test-placeholder",
        "gpt-test",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let mut cfg = make_server_config(peri_config, provider, &tmp).await;
    cfg.session_manager.set_pending_caps(PeriCaps {
        oauth: true,
        ..PeriCaps::default()
    });
    cfg.mcp_pool = Some(Arc::new(peri_middlewares::mcp::McpClientPool::new_pending()));
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let response = handle_request(
        "mcp/list",
        &json!({}),
        &cfg,
        &mut HashMap::new(),
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(response, json!({ "servers": [] }));
    assert!(response.to_string().find("url").is_none());
    assert!(response.to_string().find("error").is_none());
}

#[tokio::test]
async fn test_oauth_start_rejects_missing_flow_id_before_spawning() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-test-placeholder",
        "gpt-test",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let mut cfg = make_server_config(peri_config, provider, &tmp).await;
    cfg.session_manager.set_pending_caps(PeriCaps {
        oauth: true,
        ..PeriCaps::default()
    });
    cfg.mcp_pool = Some(Arc::new(peri_middlewares::mcp::McpClientPool::new_pending()));
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let error = handle_request(
        "mcp/oauth_start",
        &json!({ "server_name": "docs" }),
        &cfg,
        &mut HashMap::new(),
        &transport,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32602);
    assert_eq!(error.message, "missing 'flow_id'");
}

#[tokio::test]
async fn test_safe_oauth_capability_rejects_callback_secrets_over_acp() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-test-placeholder",
        "gpt-test",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let mut cfg = make_server_config(peri_config, provider, &tmp).await;
    cfg.session_manager.set_pending_caps(PeriCaps {
        oauth: true,
        ..PeriCaps::default()
    });
    cfg.mcp_pool = Some(Arc::new(peri_middlewares::mcp::McpClientPool::new_pending()));
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let error = handle_request(
        "mcp/oauth_callback",
        &json!({
            "server_name": "docs",
            "flow_id": "flow-1",
            "code": "secret-code",
            "state": "secret-state"
        }),
        &cfg,
        &mut HashMap::new(),
        &transport,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32601);
    assert!(!error.message.contains("secret-code"));
    assert!(!error.message.contains("secret-state"));
}
