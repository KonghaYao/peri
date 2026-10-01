use super::*;

#[tokio::test]
async fn checked_projection_ready_shadow_unload_aba_and_close() {
    let (mut owner, spawner) = McpTaskOwner::new();
    let registry = DynamicMcpRegistry::new(spawner, FakeConnector::new());
    let static_handle = handle("example", "static_lookup");
    let static_token: peri_acp_types::mcp_skills::HandleToken = static_handle.clone();
    let skills = Arc::new(McpSkillRegistry::new());
    let commands = Arc::new(CommandRegistry::new());
    let lease = registry.capability("session-a").bind_projection(
        vec![("example".to_string(), static_token)],
        Arc::clone(&skills),
        Arc::clone(&commands),
    );
    let projection = lease
        .as_any()
        .downcast_ref::<CheckedSessionMcpProjection>()
        .unwrap();
    assert_eq!(
        projection.pool().get_client("example").unwrap().tools[0].name,
        "static_lookup"
    );

    registry
        .execute("session-a", load("example", "one"))
        .await
        .unwrap();
    wait_ready(&registry, "session-a", "example").await;
    assert!(lease.refresh());
    let first = registry.capability("session-a").snapshot().servers["example"]
        .instance_key
        .clone();
    assert_eq!(
        projection.pool().get_client("example").unwrap().tools[0].name,
        "lookup"
    );

    registry
        .execute(
            "session-a",
            CanonicalDynamicMcpAction::Unload(CanonicalDynamicMcpUnloadRequest {
                name: "example".to_string(),
                expected_instance: Some(first.clone()),
            }),
        )
        .await
        .unwrap();
    for _ in 0..100 {
        if registry
            .capability("session-a")
            .snapshot()
            .servers
            .is_empty()
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(
        projection.pool().get_client("example").unwrap().tools[0].name,
        "static_lookup"
    );

    for _ in 0..100 {
        match registry.execute("session-a", load("example", "one")).await {
            Ok(_) => break,
            Err(error) if error.code == DynamicMcpErrorCode::ServerBusy => {
                tokio::task::yield_now().await
            }
            Err(error) => panic!("reload failed: {error:?}"),
        }
    }
    wait_ready(&registry, "session-a", "example").await;
    let second = registry.capability("session-a").snapshot().servers["example"]
        .instance_key
        .clone();
    assert_ne!(
        first, second,
        "L1 logical-name ABA must use a new incarnation"
    );
    let stale = registry
        .execute(
            "session-a",
            CanonicalDynamicMcpAction::Unload(CanonicalDynamicMcpUnloadRequest {
                name: "example".to_string(),
                expected_instance: Some(first),
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(
        stale.code,
        DynamicMcpErrorCode::ServerBusy,
        "L2 stale incarnation CAS must fail"
    );
    assert!(lease.refresh());
    assert_eq!(
        projection.pool().get_client("example").unwrap().tools[0].name,
        "lookup"
    );

    registry.close_session("session-a").await;
    assert!(
        !lease.refresh(),
        "closed session rejects late projection writes"
    );
    assert_eq!(
        projection.pool().get_client("example").unwrap().tools[0].name,
        "static_lookup"
    );
    owner.shutdown().await;
}

#[tokio::test]
async fn session_owned_projection_keeps_existing_discover_instance_live_until_close() {
    use peri_agent::tools::{BaseTool, ToolContext};

    let (mut owner, spawner) = McpTaskOwner::new();
    let registry = DynamicMcpRegistry::new(spawner, FakeConnector::new());
    let skills = Arc::new(McpSkillRegistry::new());
    let commands = Arc::new(CommandRegistry::new());
    let holder = Arc::new(parking_lot::Mutex::new(Some(
        registry.capability("session-a").bind_projection(
            Vec::new(),
            Arc::clone(&skills),
            Arc::clone(&commands),
        ),
    )));
    let pool = holder
        .lock()
        .as_ref()
        .unwrap()
        .as_any()
        .downcast_ref::<CheckedSessionMcpProjection>()
        .unwrap()
        .pool();
    let discover = crate::mcp::discover_tool::DiscoverMCPTool::new(Arc::clone(&pool), Some(skills));

    registry
        .execute("session-a", load("example", "one"))
        .await
        .unwrap();
    wait_ready(&registry, "session-a", "example").await;

    let listed = discover
        .invoke(
            serde_json::json!({"method": "list", "params": {"server": "example"}}),
            ToolContext::new(&[], "/tmp"),
        )
        .await
        .unwrap();
    let listed: serde_json::Value = serde_json::from_str(&listed).unwrap();
    assert_eq!(listed["server"], "example");
    assert_eq!(listed["tools"], serde_json::json!(["lookup"]));
    assert_eq!(
        listed["resources"],
        serde_json::json!(["test://example/resource"])
    );

    let lease = holder.lock().take().unwrap();
    lease.close();
    assert!(!lease.refresh());
    assert!(pool.get_client("example").is_none());
    drop(lease);
    registry.close_session("session-a").await;
    owner.shutdown().await;
}

#[test]
fn repeated_catalog_registration_revalidates_and_replaces_baseline() {
    let (_owner, spawner) = McpTaskOwner::new();
    let registry = DynamicMcpRegistry::new(spawner, FakeConnector::new());
    let initial = vec![DynamicMcpCatalogTool {
        name: "Builtin".to_string(),
        aliases: vec!["builtin_alias".to_string()],
        static_mcp_server: None,
    }];
    let updated = vec![DynamicMcpCatalogTool {
        name: "SubagentOnly".to_string(),
        aliases: Vec::new(),
        static_mcp_server: None,
    }];

    registry
        .register_catalog("session-a", initial.clone())
        .unwrap();
    // 同值重复注册保持幂等。
    registry.register_catalog("session-a", initial).unwrap();
    // 晚到的静态目录整体替换旧基线，而不是 first-write-wins。
    registry
        .register_catalog("session-a", updated.clone())
        .unwrap();

    let state = registry.state.lock();
    assert_eq!(state.catalogs["session-a"], updated);
}

#[tokio::test]
async fn load_and_unload_do_not_change_session_collision_baseline() {
    let (mut owner, spawner) = McpTaskOwner::new();
    let registry = DynamicMcpRegistry::new(spawner, FakeConnector::new());
    let baseline = vec![DynamicMcpCatalogTool {
        name: "Builtin".to_string(),
        aliases: vec!["builtin_alias".to_string()],
        static_mcp_server: None,
    }];
    registry
        .register_catalog("session-a", baseline.clone())
        .unwrap();

    registry
        .execute("session-a", load("example", "one"))
        .await
        .unwrap();
    wait_ready(&registry, "session-a", "example").await;
    // 已发布动态工具既不改变基线，也不让与自身无重名的目录重注册失败。
    registry
        .register_catalog("session-a", baseline.clone())
        .unwrap();
    assert_eq!(registry.state.lock().catalogs["session-a"], baseline);

    registry
        .execute(
            "session-a",
            CanonicalDynamicMcpAction::Unload(CanonicalDynamicMcpUnloadRequest {
                name: "example".to_string(),
                expected_instance: None,
            }),
        )
        .await
        .unwrap();

    assert_eq!(registry.state.lock().catalogs["session-a"], baseline);
    owner.shutdown().await;
}

#[tokio::test]
async fn late_static_catalog_with_live_dynamic_collision_is_rejected_without_partial_replace() {
    let (mut owner, spawner) = McpTaskOwner::new();
    let registry = DynamicMcpRegistry::new(spawner, FakeConnector::new());
    let baseline = vec![DynamicMcpCatalogTool {
        name: "Builtin".to_string(),
        aliases: vec!["builtin_alias".to_string()],
        static_mcp_server: None,
    }];
    registry
        .register_catalog("session-a", baseline.clone())
        .unwrap();

    // 发现期基线没有该名字，load 因此被放行；晚到的静态条目必须被重验抓住。
    registry
        .execute("session-a", load("example", "one"))
        .await
        .unwrap();
    wait_ready(&registry, "session-a", "example").await;

    let error = registry
        .register_catalog(
            "session-a",
            vec![
                baseline[0].clone(),
                DynamicMcpCatalogTool {
                    name: "mcp__Example__lookup".to_string(),
                    aliases: Vec::new(),
                    static_mcp_server: Some("Example".to_string()),
                },
            ],
        )
        .unwrap_err();

    assert_eq!(error.code, DynamicMcpErrorCode::ToolNameConflict);
    // 大小写不同视为同一碰撞；上报候选条目名而不是已有的动态工具名。
    assert_eq!(error.safe_summary, "mcp__Example__lookup");
    // 拒绝必须整体生效：旧基线保留，动态实例与已发布工具不受影响。
    assert_eq!(registry.state.lock().catalogs["session-a"], baseline);
    assert!(registry
        .capability("session-a")
        .snapshot()
        .tools
        .contains_key("mcp__example__lookup"));
    owner.shutdown().await;
}

#[tokio::test]
async fn late_static_catalog_alias_conflict_is_rejected() {
    let (mut owner, spawner) = McpTaskOwner::new();
    let registry = DynamicMcpRegistry::new(spawner, FakeConnector::new());
    registry
        .execute("session-a", load("example", "one"))
        .await
        .unwrap();
    wait_ready(&registry, "session-a", "example").await;

    let error = registry
        .register_catalog(
            "session-a",
            vec![DynamicMcpCatalogTool {
                name: "mcp__other__lookup".to_string(),
                aliases: vec!["MCP__EXAMPLE__LOOKUP".to_string()],
                static_mcp_server: None,
            }],
        )
        .unwrap_err();

    assert_eq!(error.code, DynamicMcpErrorCode::ToolNameConflict);
    assert_eq!(error.safe_summary, "mcp__other__lookup");
    assert!(!registry.state.lock().catalogs.contains_key("session-a"));
    owner.shutdown().await;
}

#[tokio::test]
async fn late_static_catalog_shadows_same_named_live_dynamic_server() {
    let (mut owner, spawner) = McpTaskOwner::new();
    let registry = DynamicMcpRegistry::new(spawner, FakeConnector::new());
    registry
        .execute("session-a", load("example", "one"))
        .await
        .unwrap();
    wait_ready(&registry, "session-a", "example").await;

    let updated = vec![DynamicMcpCatalogTool {
        name: "mcp__example__lookup".to_string(),
        aliases: Vec::new(),
        static_mcp_server: Some("example".to_string()),
    }];
    registry
        .register_catalog("session-a", updated.clone())
        .unwrap();

    assert_eq!(registry.state.lock().catalogs["session-a"], updated);
    owner.shutdown().await;
}

#[tokio::test]
async fn catalog_revalidation_is_scoped_to_the_registering_session() {
    let (mut owner, spawner) = McpTaskOwner::new();
    let registry = DynamicMcpRegistry::new(spawner, FakeConnector::new());
    registry
        .execute("session-a", load("example", "one"))
        .await
        .unwrap();
    wait_ready(&registry, "session-a", "example").await;

    let conflicting = vec![DynamicMcpCatalogTool {
        name: "mcp__example__lookup".to_string(),
        aliases: Vec::new(),
        static_mcp_server: None,
    }];
    // 同目录在无动态实例的 session 上合法：碰撞基线不跨 session 共享。
    registry
        .register_catalog("session-b", conflicting.clone())
        .unwrap();
    assert_eq!(registry.state.lock().catalogs["session-b"], conflicting);

    let error = registry
        .register_catalog("session-a", conflicting)
        .unwrap_err();
    assert_eq!(error.code, DynamicMcpErrorCode::ToolNameConflict);
    assert!(!registry.state.lock().catalogs.contains_key("session-a"));
    owner.shutdown().await;
}

#[tokio::test]
async fn catalog_collision_rejects_load_without_publishing_capability() {
    let (mut owner, spawner) = McpTaskOwner::new();
    let registry = DynamicMcpRegistry::new(spawner, FakeConnector::new());
    registry
        .register_catalog(
            "session-a",
            vec![DynamicMcpCatalogTool {
                name: "mcp__example__lookup".to_string(),
                aliases: Vec::new(),
                static_mcp_server: None,
            }],
        )
        .unwrap();
    registry
        .execute("session-a", load("example", "one"))
        .await
        .unwrap();

    for _ in 0..100 {
        let response = registry
            .execute(
                "session-a",
                CanonicalDynamicMcpAction::Status(DynamicMcpStatusRequest {
                    name: Some("example".to_string()),
                    ..Default::default()
                }),
            )
            .await
            .unwrap();
        if matches!(
            response,
            DynamicMcpResponse::Status(ref status)
                if status.operations.iter().any(|operation| {
                    operation.error.as_ref().is_some_and(|failure| {
                        failure.code == DynamicMcpErrorCode::ToolNameConflict
                    })
                })
        ) {
            break;
        }
        tokio::task::yield_now().await;
    }

    assert!(registry.capability("session-a").snapshot().tools.is_empty());
    owner.shutdown().await;
}

#[tokio::test]
async fn same_named_static_server_is_shadowable_not_a_collision() {
    let (mut owner, spawner) = McpTaskOwner::new();
    let registry = DynamicMcpRegistry::new(spawner, FakeConnector::new());
    registry
        .register_catalog(
            "session-a",
            vec![DynamicMcpCatalogTool {
                name: "mcp__example__lookup".to_string(),
                aliases: Vec::new(),
                static_mcp_server: Some("example".to_string()),
            }],
        )
        .unwrap();

    registry
        .execute("session-a", load("example", "one"))
        .await
        .unwrap();
    wait_ready(&registry, "session-a", "example").await;

    assert!(registry
        .capability("session-a")
        .snapshot()
        .tools
        .contains_key("mcp__example__lookup"));
    owner.shutdown().await;
}
