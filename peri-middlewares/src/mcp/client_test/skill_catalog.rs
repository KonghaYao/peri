/// 规范 skills server：responds to `skills/list` over an rmcp duplex wire.
async fn skill_peer(
    skill: Option<&'static str>,
) -> rmcp::service::RunningService<rmcp::service::RoleClient, ()> {
    let (client_io, server_io) = tokio::io::duplex(8192);
    tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let (reader, mut writer) = tokio::io::split(server_io);
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
            let trimmed = line.trim_end_matches(['\r', '\n']);
            let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed) else {
                line.clear();
                continue;
            };
            line.clear();
            let id = parsed.get("id").cloned().unwrap_or(serde_json::Value::Null);
            let method = parsed
                .get("method")
                .and_then(|method| method.as_str())
                .unwrap_or_default();
            let response = match (method, skill) {
                ("skills/list", Some(name)) => serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "skills": [{
                            "uri": format!("skill://{name}/SKILL.md"),
                            "frontmatter": { "name": name, "description": format!("{name} skill") },
                            "resources": [{
                                "uri": format!("skill://{name}/SKILL.md"),
                                "digest": format!("sha256:{}", "0".repeat(64)),
                            }],
                        }]
                    }
                }),
                ("skills/list", None) => serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32602, "message": "no skills" }
                }),
                _ => serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32601, "message": "method not found" }
                }),
            };
            writer
                .write_all(serde_json::to_string(&response).unwrap().as_bytes())
                .await
                .unwrap();
            writer.write_all(b"\n").await.unwrap();
        }
    });
    rmcp::service::serve_directly::<rmcp::service::RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    )
}

fn skill_handle(
    name: &str,
    running: &rmcp::service::RunningService<rmcp::service::RoleClient, ()>,
    source: peri_acp_types::plugin::ConfigSource,
) -> Arc<McpClientHandle> {
    Arc::new(McpClientHandle {
        call_gate: Default::default(),
        name: name.to_string(),
        version: None,
        connected_at: None,
        protocol_version: None,
        cache_version: None,
        peer: Some(running.peer().clone()),
        tools: vec![],
        resources: vec![],
        status: ClientStatus::Connected,
        oauth_status: OAuthStatus::default(),
        source: Some(source),
        url: None,
        skills_capable: true,
    })
}

#[tokio::test]
async fn skills_catalog_aggregates_by_capability_across_server_names() {
    let workspace_peer = skill_peer(Some("beta")).await;
    let sandbox_peer = skill_peer(Some("alpha")).await;
    let pool = McpClientPool::new_pending();
    pool.clients.write().insert(
        "workspace".into(),
        skill_handle(
            "workspace",
            &workspace_peer,
            crate::mcp::config::ConfigSource::Builtin {
                instance: "workspace".into(),
            },
        ),
    );
    pool.clients.write().insert(
        "sandbox".into(),
        skill_handle(
            "sandbox",
            &sandbox_peer,
            crate::mcp::config::ConfigSource::Project(std::path::PathBuf::from("/tmp")),
        ),
    );
    let catalog = pool
        .read_skills_catalog("session-a")
        .await
        .expect("catalog");
    assert_eq!(
        catalog
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<Vec<_>>(),
        vec!["mcp__sandbox__alpha", "mcp__workspace__beta"],
        "skills are aggregated from every skills-capable server, sorted by full name"
    );
    assert_eq!(
        catalog[0].origin,
        Some(peri_acp_types::skills::SkillOrigin::Mcp {
            server: "sandbox".into(),
            uri: "skill://alpha/SKILL.md".into(),
        })
    );
}

#[tokio::test]
async fn skills_catalog_skips_disconnected_handles() {
    let workspace_peer = skill_peer(Some("beta")).await;
    let pool = McpClientPool::new_pending();
    let mut handle = skill_handle(
        "workspace",
        &workspace_peer,
        crate::mcp::config::ConfigSource::Builtin {
            instance: "workspace".into(),
        },
    );
    Arc::get_mut(&mut handle).unwrap().status = ClientStatus::Disconnected;
    pool.clients.write().insert("workspace".into(), handle);

    let catalog = pool
        .read_skills_catalog("session-a")
        .await
        .expect("disconnected handles must be skipped, not fail the catalog");
    assert!(catalog.is_empty());
}

#[tokio::test]
async fn non_system_skill_failure_is_tolerated_but_system_workspace_fails_closed() {
    let workspace_peer = skill_peer(Some("beta")).await;
    let failing_peer = skill_peer(None).await;
    let pool = McpClientPool::new_pending();
    pool.clients.write().insert(
        "workspace".into(),
        skill_handle(
            "workspace",
            &workspace_peer,
            crate::mcp::config::ConfigSource::Builtin {
                instance: "workspace".into(),
            },
        ),
    );
    pool.clients.write().insert(
        "sandbox".into(),
        skill_handle(
            "sandbox",
            &failing_peer,
            crate::mcp::config::ConfigSource::Project(std::path::PathBuf::from("/tmp")),
        ),
    );
    let catalog = pool
        .read_skills_catalog("session-a")
        .await
        .expect("non-system failure must not abort the catalog");
    assert_eq!(
        catalog
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<Vec<_>>(),
        vec!["mcp__workspace__beta"]
    );

    let failing_workspace_peer = skill_peer(None).await;
    let sandbox_peer = skill_peer(Some("alpha")).await;
    let pool = McpClientPool::new_pending();
    pool.clients.write().insert(
        "workspace".into(),
        skill_handle(
            "workspace",
            &failing_workspace_peer,
            crate::mcp::config::ConfigSource::Builtin {
                instance: "workspace".into(),
            },
        ),
    );
    pool.clients.write().insert(
        "sandbox".into(),
        skill_handle(
            "sandbox",
            &sandbox_peer,
            crate::mcp::config::ConfigSource::Project(std::path::PathBuf::from("/tmp")),
        ),
    );
    let error = pool
        .read_skills_catalog("session-a")
        .await
        .expect_err("system workspace failure must fail closed");
    assert!(error.contains("workspace"), "{error}");
}

#[test]
fn remote_sandbox_is_not_a_local_resource_source() {
    assert!(!crate::mcp::builtin::is_local_resource_source(Some(
        &crate::mcp::config::ConfigSource::Project(std::path::PathBuf::from("/tmp"))
    )));
    assert!(!crate::mcp::builtin::is_local_resource_source(None));
    assert!(crate::mcp::builtin::is_local_resource_source(Some(
        &crate::mcp::config::ConfigSource::Builtin {
            instance: "sandbox".into(),
        }
    )));
}
