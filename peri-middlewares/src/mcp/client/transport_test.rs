use super::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::test]
async fn auto_falls_back_to_legacy_for_both_handlers() {
    for channel in [false, true] {
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
        let handler = channel.then(|| {
            Arc::new(ChannelHandler::new(
                peri_agent::interaction::ChannelState::new(),
            ))
        });
        let service = serve_client_auto(
            client,
            handler.as_ref(),
            None,
            &crate::mcp::apps::McpCapabilityProfile::disabled(),
            std::time::Duration::from_secs(2),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(matches!(service, McpServiceWrapper::Channel(_)), channel);
        server.await.unwrap();
    }
}

#[tokio::test]
async fn explicit_version_does_not_fall_back() {
    let (client, server) = tokio::io::duplex(8192);
    let server = tokio::spawn(async move {
        let (read, mut write) = tokio::io::split(server);
        let mut lines = BufReader::new(read).lines();
        let request: serde_json::Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(request["method"], "server/discover");
        let error = serde_json::json!({
            "jsonrpc": "2.0", "id": request["id"],
            "error": {"code": -32601, "message": "Method not found"}
        });
        write
            .write_all(format!("{error}\n").as_bytes())
            .await
            .unwrap();
        assert!(lines.next_line().await.unwrap().is_none());
    });
    let result = serve_client_auto(
        client,
        None,
        Some(&McpProtocolVersion::V2026_07_28),
        &crate::mcp::apps::McpCapabilityProfile::disabled(),
        std::time::Duration::from_secs(2),
    )
    .await
    .unwrap();
    assert!(
        matches!(result, Err(ClientInitializeError::JsonRpcError(error))
        if error.code == rmcp::model::ErrorCode::METHOD_NOT_FOUND)
    );
    server.await.unwrap();
}

async fn observe_first_request(
    protocol_version: Option<&McpProtocolVersion>,
    channel: bool,
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

    let channel_handler = channel.then(|| {
        Arc::new(ChannelHandler::new(
            peri_agent::interaction::ChannelState::new(),
        ))
    });
    let _service = serve_client_auto(
        client_io,
        channel_handler.as_ref(),
        protocol_version,
        capability_profile,
        std::time::Duration::from_secs(2),
    )
    .await
    .expect("握手不应超时")
    .expect("握手应成功");
    server.await.unwrap()
}

#[tokio::test]
async fn none_starts_with_discover() {
    let request = observe_first_request(
        None,
        false,
        &crate::mcp::apps::McpCapabilityProfile::disabled(),
    )
    .await;
    assert_eq!(request["method"], "server/discover");
}

#[tokio::test]
async fn explicit_2026_07_28_transport_starts_with_discover() {
    let request = observe_first_request(
        Some(&McpProtocolVersion::V2026_07_28),
        false,
        &crate::mcp::apps::McpCapabilityProfile::disabled(),
    )
    .await;
    assert_eq!(request["method"], "server/discover");
}

#[tokio::test]
async fn enabled_profile_is_advertised_in_both_channel_modes() {
    let profile =
        crate::mcp::apps::McpCapabilityProfile::negotiated([crate::mcp::MCP_APP_MIME_TYPE]);
    for protocol_version in [None, Some(&McpProtocolVersion::V2026_07_28)] {
        let request = observe_first_request(protocol_version, true, &profile).await;
        let extensions = if request["method"] == "server/discover" {
            &request["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"]["extensions"]
        } else {
            &request["params"]["capabilities"]["extensions"]
        };
        assert!(
            extensions[crate::mcp::MCP_UI_EXTENSION].is_object(),
            "request: {request}"
        );
    }
}
