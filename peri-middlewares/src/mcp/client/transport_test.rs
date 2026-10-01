use super::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::test]
async fn auto_falls_back_to_legacy() {
    let (client, server) = tokio::io::duplex(8192);
    let server = tokio::spawn(async move {
        let (read, mut write) = tokio::io::split(server);
        let mut lines = BufReader::new(read).lines();
        let discover: serde_json::Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(discover["method"], "server/discover");
        let error = serde_json::json!({
            "jsonrpc": "2.0", "id": discover["id"],
            "error": {"code": -32601, "message": "Method not found"}
        });
        write
            .write_all(format!("{error}\n").as_bytes())
            .await
            .unwrap();
        let init: serde_json::Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(init["method"], "initialize");
        let response = serde_json::json!({
            "jsonrpc": "2.0", "id": init["id"], "result": {
                "protocolVersion": "2025-11-25", "capabilities": {},
                "serverInfo": {"name": "legacy", "version": "1"}
            }
        });
        write
            .write_all(format!("{response}\n").as_bytes())
            .await
            .unwrap();
        let initialized: serde_json::Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(initialized["method"], "notifications/initialized");
    });
    let service = serve_client_auto(
        client,
        &crate::mcp::apps::McpCapabilityProfile::disabled(),
        std::time::Duration::from_secs(2),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        service.peer().peer_info().unwrap().protocol_version,
        rmcp::model::ProtocolVersion::V_2025_11_25
    );
    server.await.unwrap();
}

async fn observe_first_request(
    capability_profile: &crate::mcp::apps::McpCapabilityProfile,
) -> serde_json::Value {
    let (client_io, server_io) = tokio::io::duplex(8192);
    let server = tokio::spawn(async move {
        let (read, mut write) = tokio::io::split(server_io);
        let mut lines = BufReader::new(read).lines();
        let line = lines.next_line().await.unwrap().unwrap();
        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
        let method = request["method"].as_str().unwrap().to_string();
        let id = request["id"].clone();
        let result = if method == "server/discover" {
            serde_json::json!({
                "resultType": "complete",
                "supportedVersions": ["2026-07-28"],
                "capabilities": {},
                "serverInfo": { "name": "test-server", "version": "1.0.0" },
                "ttlMs": 0,
                "cacheScope": "private"
            })
        } else {
            serde_json::json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "serverInfo": { "name": "test-server", "version": "1.0.0" }
            })
        };
        let response = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result});
        write
            .write_all(format!("{response}\n").as_bytes())
            .await
            .unwrap();
        if method == "initialize" {
            let initialized = lines.next_line().await.unwrap().unwrap();
            let notification: serde_json::Value = serde_json::from_str(&initialized).unwrap();
            assert_eq!(notification["method"], "notifications/initialized");
        }
        request
    });

    let _service = serve_client_auto(
        client_io,
        capability_profile,
        std::time::Duration::from_secs(2),
    )
    .await
    .expect("握手不应超时")
    .expect("握手应成功");
    server.await.unwrap()
}

#[tokio::test]
async fn auto_starts_with_discover() {
    let request = observe_first_request(&crate::mcp::apps::McpCapabilityProfile::disabled()).await;
    assert_eq!(request["method"], "server/discover");
}

#[tokio::test]
async fn enabled_profile_is_advertised_for_negotiated_versions() {
    let profile =
        crate::mcp::apps::McpCapabilityProfile::negotiated([crate::mcp::MCP_APP_MIME_TYPE]);
    let request = observe_first_request(&profile).await;
    let extensions =
        &request["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"]["extensions"];
    assert!(
        extensions[crate::mcp::MCP_UI_EXTENSION].is_object(),
        "request: {request}"
    );
}

#[tokio::test]
async fn disabled_profile_does_not_advertise_apps_for_negotiated_versions() {
    let profile = crate::mcp::apps::McpCapabilityProfile::disabled();
    let request = observe_first_request(&profile).await;
    let extensions =
        &request["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"]["extensions"];
    assert!(
        extensions[crate::mcp::MCP_UI_EXTENSION].is_null(),
        "request: {request}"
    );
}
