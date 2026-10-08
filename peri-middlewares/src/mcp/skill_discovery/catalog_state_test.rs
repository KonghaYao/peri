use super::*;
use crate::skills::tools::{DiscoverSkillsTool, SkillTool};
use peri_agent::tools::{BaseTool, ToolContext};

async fn list_response_server(io: tokio::io::DuplexStream, response: serde_json::Value) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (reader, mut writer) = tokio::io::split(io);
    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await.unwrap() {
        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
        if let Some(id) = request.get("id") {
            let mut reply = response.clone();
            reply["jsonrpc"] = serde_json::json!("2.0");
            reply["id"] = id.clone();
            writer
                .write_all(serde_json::to_string(&reply).unwrap().as_bytes())
                .await
                .unwrap();
            writer.write_all(b"\n").await.unwrap();
        }
    }
}

async fn discover_response(response: serde_json::Value) -> Arc<McpSkillRegistry> {
    let (client_io, server_io) = tokio::io::duplex(8192);
    let server_task = tokio::spawn(list_response_server(server_io, response));
    let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let registry = Arc::new(McpSkillRegistry::new());
    let token: HandleToken = Arc::new(1u32);
    registry.mark_discovery_started("srv", token.clone());
    run_discovery(
        registry.clone(),
        None,
        make_spec_handle(&running),
        token,
        AgentCancellationToken::new(),
    )
    .await;
    server_task.abort();
    registry
}

async fn assert_failed_catalog(registry: Arc<McpSkillRegistry>) {
    assert!(matches!(
        registry.discovery_state("srv"),
        Some(ServerDiscoveryState::Failed { .. })
    ));
    assert!(!registry.discovery_in_progress());
    let discover = DiscoverSkillsTool::new(Some(registry.clone()));
    let error = discover
        .invoke(serde_json::json!({}), ToolContext::new(&[], "."))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("reconnect the provider"));
    assert!(!error.to_string().contains("initializing"));
    let skill = SkillTool::new(Some(registry));
    let error = skill
        .invoke(
            serde_json::json!({"skill_name": "demo"}),
            ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("reconnect the provider"));
}

/// [回归测试] L2：成功空清单与读取失败必须有不同的模型可见结果。
#[tokio::test]
async fn skills_list_ready_empty_is_a_legal_empty_catalog() {
    let registry = discover_response(serde_json::json!({"result": {"skills": []}})).await;
    assert!(
        matches!(registry.discovery_state("srv"), Some(ServerDiscoveryState::Discovered { entries, .. }) if entries.is_empty())
    );
    let tool = DiscoverSkillsTool::new(Some(registry));
    assert_eq!(
        tool.invoke(serde_json::json!({}), ToolContext::new(&[], "."))
            .await
            .unwrap(),
        "[]"
    );
}

/// [回归测试] 缺少必填 skills 不是显式 skills:[]，不得默认为合法空目录。
#[tokio::test]
async fn skills_list_missing_skills_field_is_failed_catalog() {
    assert_failed_catalog(discover_response(serde_json::json!({"result": {}})).await).await;
}

#[tokio::test]
async fn frozen_skills_snapshot_reuses_catalog_failure_and_pagination_rules() {
    for (response, should_fail) in [
        (serde_json::json!({"result": {"skills": []}}), false),
        (serde_json::json!({"result": {}}), true),
        (
            serde_json::json!({"result": {"skills": [
                {"uri": "skill://bad/SKILL.md", "frontmatter": {"name": "bad"}}
            ]}}),
            true,
        ),
        (
            serde_json::json!({"result": {"skills": [
                {"uri": "skill://bad/SKILL.md", "frontmatter": {"name": "bad", "description": "bad"}, "resources": []}
            ]}}),
            true,
        ),
        (
            serde_json::json!({"result": {"skills": [], "nextCursor": "stuck"}}),
            true,
        ),
    ] {
        let (client_io, server_io) = tokio::io::duplex(8192);
        let server_task = tokio::spawn(list_response_server(server_io, response));
        let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
            (),
            client_io,
            None::<rmcp::model::ServerPeerInfo>,
        );
        let result = crate::mcp::skill_discovery::skills_list::snapshot_via_skills_list(
            running.peer().clone(),
            "srv",
            AgentCancellationToken::new(),
        )
        .await;
        assert_eq!(result.is_err(), should_fail, "{result:?}");
        if !should_fail {
            assert!(result.unwrap().is_empty());
        }
        running.cancel().await.unwrap();
        server_task.abort();
    }
}

/// [回归测试] 非空候选全部 frontmatter 非法不能冒充读取成功的空目录。
#[tokio::test]
async fn skills_list_all_invalid_frontmatter_is_failed_catalog() {
    let response = serde_json::json!({"result": {"skills": [
        {"uri": "skill://missing/SKILL.md", "frontmatter": {"name": "missing"}},
        {"uri": "skill://wrong/SKILL.md", "frontmatter": {"name": "wrong", "description": 7}}
    ]}});
    assert_failed_catalog(discover_response(response).await).await;
}

/// [回归测试] frontmatter 合法但 metadata 完整性全部失败同样发布 Failed。
#[tokio::test]
async fn skills_list_all_rejected_metadata_is_failed_catalog() {
    let response = serde_json::json!({"result": {"skills": [
        {"uri": "skill://incomplete/SKILL.md", "frontmatter": {"name": "incomplete", "description": "demo"}, "resources": []}
    ]}});
    assert_failed_catalog(discover_response(response).await).await;
}

/// [回归测试] 混合坏 frontmatter、坏 metadata、好条目时只隔离坏项。
#[tokio::test]
async fn skills_list_mixed_invalid_entries_keeps_good_catalog() {
    let response = serde_json::json!({"result": {"skills": [
        {"uri": "skill://missing/SKILL.md", "frontmatter": {"name": "missing"}},
        {"uri": "skill://incomplete/SKILL.md", "frontmatter": {"name": "incomplete", "description": "bad"}, "resources": []},
        {"uri": "skill://good/SKILL.md", "frontmatter": {"name": "good", "description": "usable"}}
    ]}});
    let registry = discover_response(response).await;
    assert!(!registry.discovery_failed());
    assert!(
        matches!(registry.discovery_state("srv"), Some(ServerDiscoveryState::Discovered { entries, .. }) if entries.len() == 1 && entries[0].name == "mcp__srv__good")
    );
    let tool = DiscoverSkillsTool::new(Some(registry));
    let result = tool
        .invoke(serde_json::json!({}), ToolContext::new(&[], "."))
        .await
        .unwrap();
    assert!(result.contains("mcp__srv__good"));
    assert!(!result.contains("mcp__srv__missing"));
    assert!(!result.contains("mcp__srv__incomplete"));
}

/// [回归测试] peer 缺失在规范及 legacy 空候选路径均不能发布成功空目录。
#[tokio::test]
async fn missing_peer_is_failed_for_spec_and_legacy_empty_candidates() {
    for skills_capable in [true, false] {
        let (client_io, _server_io) = tokio::io::duplex(8192);
        let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
            (),
            client_io,
            None::<rmcp::model::ServerPeerInfo>,
        );
        let mut handle = make_spec_handle(&running);
        let fields = Arc::get_mut(&mut handle).unwrap();
        fields.peer = None;
        fields.skills_capable = skills_capable;
        let registry = Arc::new(McpSkillRegistry::new());
        let token: HandleToken = Arc::new(1u32);
        registry.mark_discovery_started("srv", token.clone());
        run_discovery(
            registry.clone(),
            None,
            handle,
            token,
            AgentCancellationToken::new(),
        )
        .await;
        assert_failed_catalog(registry).await;
    }
}

/// [回归测试] legacy 空候选的模板探测 await 期间取消，不能发布成功空目录。
#[tokio::test]
async fn legacy_empty_candidates_cancelled_during_probe_do_not_publish_empty() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (client_io, server_io) = tokio::io::duplex(8192);
    let cancel = AgentCancellationToken::new();
    let server_cancel = cancel.clone();
    let server_task = tokio::spawn(async move {
        let (reader, mut writer) = tokio::io::split(server_io);
        let mut lines = BufReader::new(reader).lines();
        let request: serde_json::Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(request["method"], "resources/templates/list");
        server_cancel.cancel();
        let reply = serde_json::json!({"jsonrpc": "2.0", "id": request["id"], "result": {"resourceTemplates": []}});
        writer
            .write_all(serde_json::to_string(&reply).unwrap().as_bytes())
            .await
            .unwrap();
        writer.write_all(b"\n").await.unwrap();
    });
    let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let registry = Arc::new(McpSkillRegistry::new());
    let token: HandleToken = Arc::new(1u32);
    registry.mark_discovery_started("srv", token.clone());
    run_discovery(
        registry.clone(),
        None,
        make_discovery_handle(&running, vec![]),
        token,
        cancel,
    )
    .await;
    server_task.await.unwrap();
    assert!(registry.discovery_state("srv").is_none());
    assert!(!registry.discovery_failed());
}

/// [回归测试] 超时不能以 Discovered(empty) 发布；缓存和无缓存路径口径一致。
#[tokio::test(start_paused = true)]
async fn skills_list_timeout_is_failed_not_ready_empty() {
    for cached in [false, true] {
        let (client_io, _server_io) = tokio::io::duplex(8192);
        let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
            (),
            client_io,
            None::<rmcp::model::ServerPeerInfo>,
        );
        let registry = Arc::new(McpSkillRegistry::new());
        let token: HandleToken = Arc::new(1u32);
        registry.mark_discovery_started("srv", token.clone());
        let handle = make_spec_handle(&running);
        let cache = cached.then(|| {
            let pool = Arc::new(crate::mcp::client::McpClientPool::new_empty());
            pool.advance_handle_generation(&handle);
            pool.clients
                .write()
                .insert("srv".to_string(), handle.clone());
            pool.resource_cache_for_handle(&handle).unwrap()
        });
        run_discovery_with_cache(
            registry.clone(),
            None,
            handle,
            token,
            AgentCancellationToken::new(),
            cache,
            false,
        )
        .await;
        assert_failed_catalog(registry).await;
    }
}

/// [回归测试] RPC 错误与响应解析错误同样不得伪装合法空目录。
#[tokio::test]
async fn skills_list_rpc_and_parse_errors_are_failed_catalogs() {
    for response in [
        serde_json::json!({"error": {"code": -32603, "message": "unavailable"}}),
        serde_json::json!({"result": {"skills": "invalid"}}),
    ] {
        assert_failed_catalog(discover_response(response).await).await;
    }
}

/// [回归测试] 失败代数不无限重扫；重连才能重新发现，旧失败不得污染新代数。
#[test]
fn failed_discovery_reconnects_without_stale_failure_overwrite() {
    let registry = McpSkillRegistry::new();
    let old: HandleToken = Arc::new(1u32);
    let current: HandleToken = Arc::new(2u32);
    registry.mark_discovery_started("srv", old.clone());
    registry.mark_discovery_failed("srv", old.clone());
    assert!(registry
        .project_connected(&[("srv".into(), old.clone())])
        .to_discover
        .is_empty());
    assert_eq!(
        registry
            .project_connected(&[("srv".into(), current.clone())])
            .to_discover
            .len(),
        1
    );
    registry.mark_discovery_started("srv", current.clone());
    registry.mark_discovery_failed("srv", old);
    assert!(registry.discovery_in_progress());
    assert!(!registry.discovery_failed());
    registry.mark_discovery_completed("srv", current, vec![]);
    assert!(!registry.discovery_failed());
    assert!(!registry.discovery_in_progress());
}

/// [回归测试] 取消不是合法空目录，也不应保留 Failed 或永久 Started。
#[tokio::test]
async fn cancelled_skills_list_does_not_publish_ready_empty() {
    let (client_io, _server_io) = tokio::io::duplex(8192);
    let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let registry = Arc::new(McpSkillRegistry::new());
    let token: HandleToken = Arc::new(1u32);
    registry.mark_discovery_started("srv", token.clone());
    let cancel = AgentCancellationToken::new();
    cancel.cancel();
    let result = crate::mcp::skill_discovery::skills_list::collect_via_skills_list(
        running.peer().clone(),
        "srv",
        cancel.clone(),
    )
    .await;
    assert!(result.is_err());
    run_discovery(
        registry.clone(),
        None,
        make_spec_handle(&running),
        token,
        cancel,
    )
    .await;
    assert!(registry.discovery_state("srv").is_none());
}

/// [回归测试] 一个来源失败不能掩盖其他来源；失败只补模型摘要告警。
#[tokio::test]
async fn failed_provider_keeps_healthy_catalog_and_warns_in_summary() {
    let registry =
        discover_response(serde_json::json!({"error": {"code": -32603, "message": "unavailable"}}))
            .await;
    let metadata = SkillMetadata {
        name: "mcp__healthy__demo".into(),
        aliases: vec![],
        description: "demo".into(),
        path: Default::default(),
        source: SkillSource::Mcp,
        plugin_name: None,
        origin: Some(SkillOrigin::Mcp {
            server: "healthy".into(),
            uri: "skill://demo/SKILL.md".into(),
        }),
        content: None,
        resources: vec![],
        frontmatter: None,
    };
    let token: HandleToken = Arc::new(2u32);
    registry.mark_discovery_started("healthy", token.clone());
    registry.mark_discovery_completed("healthy", token, vec![metadata]);
    let tool = DiscoverSkillsTool::new(Some(registry.clone()));
    let result = tool
        .invoke(serde_json::json!({}), ToolContext::new(&[], "."))
        .await
        .unwrap();
    assert!(result.contains("mcp__healthy__demo"));
    let middleware = crate::skills::SkillsMiddleware::new()
        .with_mcp_registry(Some(registry))
        .with_frozen_summary("FROZEN CATALOG".into());
    let summary =
        peri_agent::middleware::r#trait::Middleware::prompt_contribution(&middleware).unwrap();
    assert!(summary.starts_with("FROZEN CATALOG"));
    assert!(summary.contains("catalog may be incomplete"));
    assert!(!summary.contains("unavailable"));
}
