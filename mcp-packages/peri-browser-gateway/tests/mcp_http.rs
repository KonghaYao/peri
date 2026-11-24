#![cfg(unix)]

#[path = "contracts/daemon.rs"]
mod daemon;
#[path = "contracts/monitor_handoff.rs"]
mod monitor_handoff;
#[path = "contracts/onboarding_wire.rs"]
mod onboarding_wire;
#[path = "contracts/process.rs"]
mod process;
#[path = "contracts/session_isolation.rs"]
mod session_isolation;
#[path = "contracts/shutdown.rs"]
mod shutdown;
mod support;

use anyhow::Context;
use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, ClientConfig, GetSkillRequestParams, Implementation,
    ProtocolVersion, ReadResourceRequestParams, TASKS_EXTENSION_ID,
};
use rmcp::service::{serve_client_with_lifecycle, ClientLifecycleMode};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::StreamableHttpClientTransport;
use serde_json::{json, Value};
use support::gateway::{initialize, request, rpc, Gateway, LEGACY};
use support::wire::Wire;

fn peri_config() -> ClientConfig {
    let mut capabilities = ClientCapabilities::default();
    capabilities.extensions = Some(
        ["io.mcpp/server-cache-version", TASKS_EXTENSION_ID]
            .into_iter()
            .map(|key| (key.to_owned(), serde_json::Map::new()))
            .collect(),
    );
    ClientConfig::new(
        capabilities,
        Implementation::new("peri-middlewares", "0.2.0"),
    )
    .with_protocol_version(ProtocolVersion::LATEST_WITH_INITIALIZE)
}

fn peri_mode() -> ClientLifecycleMode {
    ClientLifecycleMode::Auto {
        preferred_versions: vec![ProtocolVersion::V_2026_07_28],
        legacy_version: None,
    }
}

#[tokio::test]
async fn peri_auto_and_explicit_initialize_use_live_gateway_sessions() -> anyhow::Result<()> {
    let mut gateway = Gateway::start("http://127.0.0.1:9", &[]).await?;
    let discovery = gateway
        .post(
            None,
            request(
                "server/discover",
                json!({"_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": {},
                }}),
            ),
        )
        .header("mcp-method", "server/discover")
        .header("mcp-protocol-version", "2026-07-28")
        .send()
        .await?;
    assert_eq!(discovery.status(), 404);
    let discovery = rpc(discovery).await?;
    assert_eq!(discovery["id"], 1);
    assert_eq!(discovery["error"]["code"], -32601);
    assert_eq!(gateway.count().await?, 0);

    let wire = Wire::start(&gateway).await?;
    let mut clients = Vec::new();
    let mut domains = Vec::new();
    let mut transport_ids = Vec::new();
    for (mode, expected_lifecycle) in [
        (peri_mode(), &["server/discover", "initialize"][..]),
        (peri_mode(), &["server/discover", "initialize"][..]),
        (ClientLifecycleMode::Initialize, &["initialize"][..]),
    ] {
        let start = wire.posts().len();
        let transport = StreamableHttpClientTransport::with_client(
            gateway.http.clone(),
            StreamableHttpClientTransportConfig::with_uri(wire.uri.as_str()),
        );
        let client = serve_client_with_lifecycle(peri_config(), transport, mode).await?;
        let info = client.peer_info().expect("协商结果");
        assert_eq!(
            info.protocol_version,
            ProtocolVersion::LATEST_WITH_INITIALIZE
        );
        assert_eq!(
            info.server_info.as_ref().map(|info| info.name.as_str()),
            Some("peri-browser-mcp")
        );
        let tools = client.list_tools(None).await?;
        assert!(tools.tools.iter().any(|tool| tool.name == "browser_status"));
        let status = client
            .call_tool(CallToolRequestParams::new("browser_status"))
            .await?
            .structured_content
            .unwrap();
        assert_eq!(status["connected"], false);
        assert_eq!(status["pageCount"], 0);
        assert!(status["gatewaySock"].is_null());
        assert_eq!(status["monitorIndex"], format!("{}/", gateway.base));
        let path = gateway
            .monitor_path(status["sessionId"].as_str().unwrap())
            .await?;
        let monitor = gateway
            .http
            .get(format!("{}{path}", gateway.base))
            .send()
            .await?;
        assert_eq!(monitor.status(), 200);
        assert_eq!(monitor.headers()["cache-control"], "no-store");
        assert_eq!(monitor.headers()["referrer-policy"], "no-referrer");
        assert_eq!(monitor.headers()["x-frame-options"], "DENY");

        let skills = client.list_skills(None).await?;
        assert_eq!(skills.skills.len(), 2);
        let uri = &skills.skills[0].uri;
        let skill = client
            .get_skill(GetSkillRequestParams::new(uri.clone()))
            .await?;
        assert_eq!(skill.skill.uri, *uri);
        let resources = client.list_resources(None).await?;
        assert_eq!(resources.resources.len(), 2);
        let resource = client
            .read_resource(ReadResourceRequestParams::new(uri.clone()))
            .await?;
        assert_eq!(resource.contents.len(), 1);
        let posts = wire.posts();
        let posts = &posts[start..];
        let methods: Vec<_> = posts
            .iter()
            .map(|post| post.rpc_method.as_deref().unwrap())
            .collect();
        assert!(methods.starts_with(expected_lifecycle));
        let init = &posts[expected_lifecycle.len() - 1];
        assert_eq!(init.status, 200);
        assert!(init.sent_session.is_none());
        let transport_id = init
            .returned_session
            .as_deref()
            .expect("initialize 响应中的 session header");
        if expected_lifecycle[0] == "server/discover" {
            let discovery = &posts[0];
            assert_eq!(discovery.status, 404);
            assert_eq!(discovery.sent_version.as_deref(), Some("2026-07-28"));
            assert!(discovery.sent_session.is_none() && discovery.returned_session.is_none());
        }
        let attached = &posts[expected_lifecycle.len()..];
        assert_eq!(
            methods[expected_lifecycle.len()..],
            [
                "notifications/initialized",
                "tools/list",
                "tools/call",
                "skills/list",
                "skills/get",
                "resources/list",
                "resources/read"
            ]
        );
        assert!(attached
            .iter()
            .all(|post| post.sent_session.as_deref() == Some(transport_id)
                && post.sent_version.as_deref() == Some(LEGACY)));
        transport_ids.push(transport_id.to_owned());
        domains.push(status["sessionId"].clone());
        clients.push(client);
    }
    assert_eq!(
        transport_ids
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        3
    );
    assert_eq!(
        domains
            .iter()
            .filter_map(Value::as_str)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        3
    );
    assert_eq!(gateway.count().await?, 3);
    for client in clients {
        client.cancel().await?;
    }
    gateway.shutdown().await?;
    wire.shutdown().await
}

#[tokio::test]
async fn failed_initialize_and_stateless_schema_lookup_do_not_publish_or_open_cdp(
) -> anyhow::Result<()> {
    let mut engine = support::fake_engine::Engine::ephemeral().await?;
    let mut gateway = Gateway::start(&engine.url(), &[]).await?;
    let mut bad = initialize(LEGACY);
    bad["params"]["clientInfo"] = json!(false);
    let rejected = gateway.post(None, bad).send().await?;
    assert_eq!(rejected.status(), 400);
    assert!(!rejected.headers().contains_key("mcp-session-id"));
    let write = request(
        "tools/call",
        json!({"name":"new_page", "arguments":{}, "_meta":{
            "io.modelcontextprotocol/protocolVersion":"2026-07-28",
            "io.modelcontextprotocol/clientCapabilities":{},
            "io.modelcontextprotocol/clientInfo":{"name":"fixture","version":"1"},
        }}),
    );
    let status = gateway
        .post(None, write)
        .header("mcp-protocol-version", "2026-07-28")
        .header("mcp-method", "tools/call")
        .header("mcp-name", "new_page")
        .send()
        .await?;
    assert!(!status.headers().contains_key("mcp-session-id"));
    let denied = rpc(status).await?;
    assert_eq!(denied["id"], 1);
    assert_eq!(
        denied["error"]["message"],
        "a live browser session is required"
    );
    assert_eq!(gateway.count().await?, 0);
    assert!(engine.events().is_empty());
    assert!(engine.targets().is_empty());
    gateway.shutdown().await?;
    engine.shutdown().await
}

#[tokio::test]
async fn session_id_and_http_authority_are_checked_on_the_real_routes() -> anyhow::Result<()> {
    let mut gateway = Gateway::start("http://127.0.0.1:9", &[]).await?;
    let tools = request(
        "tools/list",
        json!({"_meta": {"BoundBrowserSession": true}}),
    );
    assert_eq!(
        gateway.post(None, tools.clone()).send().await?.status(),
        400
    );
    assert_eq!(
        gateway
            .post(Some("no-such-id"), tools.clone())
            .send()
            .await?
            .status(),
        404
    );
    assert_eq!(
        gateway
            .post(None, initialize(LEGACY))
            .header("origin", &gateway.base)
            .send()
            .await?
            .status(),
        403
    );
    assert_eq!(
        gateway
            .post(None, initialize(LEGACY))
            .header("host", "remote.invalid")
            .send()
            .await?
            .status(),
        403
    );
    assert_eq!(
        gateway
            .http
            .get(format!("{}/mcp/", gateway.base))
            .send()
            .await?
            .status(),
        404
    );
    assert_eq!(gateway.count().await?, 0);

    let id = gateway.init().await?;
    let duplicate = gateway.post(Some(&id), initialize(LEGACY)).send().await?;
    assert!(!duplicate.headers().contains_key("mcp-session-id"));
    assert!(rpc(duplicate).await.context("重复 initialize 回执")?["error"].is_object());
    let modern = request(
        "tools/list",
        json!({"_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {},
            "io.modelcontextprotocol/clientInfo": {"name": "fixture", "version": "1"},
            "BoundBrowserSession": true,
        }}),
    );
    let denied = gateway
        .post(None, modern)
        .header("mcp-session-id", &id)
        .header("mcp-method", "tools/list")
        .header("mcp-protocol-version", "2026-07-28")
        .send()
        .await?;
    let denial = rpc(denied).await.context("现代无绑定请求回执")?;
    assert!(denial.get("result").is_none());
    assert!(denial["error"].is_object());
    let response = gateway.call(&id, "browser_status", json!({})).await?;
    let path = gateway
        .monitor_path(response["structuredContent"]["sessionId"].as_str().unwrap())
        .await?;
    let url = format!("{}{path}", gateway.base);
    assert_eq!(
        gateway
            .http
            .get(&url)
            .header("origin", &gateway.base)
            .send()
            .await?
            .status(),
        200
    );
    assert_eq!(
        gateway
            .http
            .get(&url)
            .header("origin", "null")
            .send()
            .await?
            .status(),
        403
    );
    assert_eq!(
        gateway
            .http
            .get(format!("{}/s/missing/", gateway.base))
            .send()
            .await?
            .status(),
        404
    );

    let retained = gateway.events(&id).send().await?;
    assert_eq!(retained.status(), 200);
    let (a, b) = tokio::join!(gateway.delete(&id).send(), gateway.delete(&id).send());
    assert_eq!(a?.status(), 202);
    assert_eq!(b?.status(), 202);
    let mut retained = retained;
    while retained.chunk().await?.is_some() {}
    assert_eq!(gateway.post(Some(&id), tools).send().await?.status(), 404);
    assert_eq!(gateway.events(&id).send().await?.status(), 404);
    assert_eq!(gateway.http.get(&url).send().await?.status(), 404);
    assert_eq!(gateway.count().await?, 0);
    let health: Value = gateway
        .http
        .get(format!("{}/healthz", gateway.base))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(health["ok"], true);
    gateway.shutdown().await
}
