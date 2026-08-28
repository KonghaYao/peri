use super::*;
use async_trait::async_trait;
use peri_acp_types::tools::{
    EffectiveToolCall, EffectiveToolDefinition, EffectiveToolDispatcher, EffectiveToolError,
};

struct FakeDispatcher;

#[async_trait]
impl EffectiveToolDispatcher for FakeDispatcher {
    async fn dispatch(
        &self,
        _call: EffectiveToolCall,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<String, EffectiveToolError> {
        Ok("ok".into())
    }

    fn tools(&self) -> Vec<EffectiveToolDefinition> {
        Vec::new()
    }
}

#[test]
fn binding_lease_is_single_consume_and_cancel_aware() {
    let registry = McpAppBindingLeaseRegistry::default();
    let cancel = tokio_util::sync::CancellationToken::new();
    registry.issue(McpAppBindingLease::new(
        "session".into(),
        "turn".into(),
        "server".into(),
        7,
        "ui://app".into(),
        "tool".into(),
        "token-1".into(),
        HashMap::from([("tool".into(), "mcp__server__tool".into())]),
        Arc::new(FakeDispatcher),
        cancel.clone(),
    ));
    assert!(registry
        .consume(
            "server",
            "tool",
            8,
            "ui://app",
            "session",
            "token-1",
            "connection"
        )
        .is_none());
    assert!(registry
        .consume(
            "server",
            "tool",
            7,
            "ui://other",
            "session",
            "token-1",
            "connection"
        )
        .is_none());
    assert!(registry
        .consume(
            "server",
            "tool",
            7,
            "ui://app",
            "session",
            "wrong-token",
            "connection"
        )
        .is_none());
    let consumed = registry
        .consume(
            "server",
            "tool",
            7,
            "ui://app",
            "session",
            "token-1",
            "connection",
        )
        .expect("matching lease should be consumed once");
    assert!(registry.is_current_turn(&consumed));
    assert!(registry
        .consume(
            "server",
            "tool",
            7,
            "ui://app",
            "session",
            "token-1",
            "connection"
        )
        .is_none());

    registry.issue(McpAppBindingLease::new(
        "session".into(),
        "turn-2".into(),
        "server".into(),
        7,
        "ui://app".into(),
        "tool".into(),
        "token-1".into(),
        HashMap::from([("tool".into(), "mcp__server__tool".into())]),
        Arc::new(FakeDispatcher),
        cancel.clone(),
    ));
    assert!(!registry.is_current_turn(&consumed));
    cancel.cancel();
    assert!(registry
        .consume(
            "server",
            "tool",
            7,
            "ui://app",
            "session",
            "token-1",
            "connection"
        )
        .is_none());
}

#[test]
fn connection_cleanup_purges_only_owned_raw_results() {
    let registry = McpAppBindingLeaseRegistry::default();
    registry.record_raw_result(
        "mcp-app:connection-a:one",
        serde_json::json!({"content": []}),
    );
    registry.record_raw_result(
        "mcp-app:connection-b:two",
        serde_json::json!({"content": []}),
    );

    registry.purge_raw_results_for_connection("connection-a");

    assert!(registry
        .take_raw_result("mcp-app:connection-a:one")
        .is_none());
    assert!(registry
        .take_raw_result("mcp-app:connection-b:two")
        .is_some());
}

#[test]
fn deployment_presence_enables_apps_regardless_of_value() {
    assert!(deployment_profile(true).apps_enabled());
    assert!(!deployment_profile(false).apps_enabled());
}

#[test]
fn profile_only_negotiates_supported_mime() {
    let profile =
        McpCapabilityProfile::negotiated(["text/plain", MCP_APP_MIME_TYPE, MCP_APP_MIME_TYPE]);
    assert_eq!(
        profile.apps_mime_types().collect::<Vec<_>>(),
        [MCP_APP_MIME_TYPE]
    );
}

#[test]
fn negotiated_profile_builds_ui_extension() {
    let profile = McpCapabilityProfile::negotiated([MCP_APP_MIME_TYPE]);
    assert_eq!(
        profile.ui_extension(),
        Some(Map::from_iter([(
            "mimeTypes".to_string(),
            serde_json::json!([MCP_APP_MIME_TYPE]),
        )]))
    );
}

#[test]
fn disabled_profile_has_no_ui_extension() {
    assert!(McpCapabilityProfile::disabled().ui_extension().is_none());
}

#[test]
fn visibility_missing_defaults_to_model_and_app() {
    assert_eq!(
        ToolVisibility::from_tool_meta(None),
        ToolVisibility {
            model: true,
            app: true
        }
    );
}

#[test]
fn app_only_visibility_excludes_model() {
    let meta = serde_json::json!({"ui": {"visibility": ["app"]}});
    assert_eq!(
        ToolVisibility::from_tool_meta(Some(&meta)),
        ToolVisibility {
            model: false,
            app: true
        }
    );
}

#[test]
fn malformed_ui_metadata_fails_closed() {
    let meta = serde_json::json!({"ui": "invalid"});
    assert_eq!(
        ToolVisibility::from_tool_meta(Some(&meta)),
        ToolVisibility {
            model: false,
            app: false
        }
    );
}

#[test]
fn unknown_visibility_fails_closed() {
    let meta = serde_json::json!({"ui": {"visibility": ["app", "future"]}});
    assert_eq!(
        ToolVisibility::from_tool_meta(Some(&meta)),
        ToolVisibility {
            model: false,
            app: false
        }
    );
}

#[test]
fn conflicting_resource_uri_fails_closed() {
    let meta = serde_json::json!({
        "ui": {"resourceUri": "ui://canonical"},
        "ui/resourceUri": "ui://legacy"
    });
    assert_eq!(canonical_resource_uri(Some(&meta)), None);
}

#[test]
fn legacy_resource_uri_is_canonicalized() {
    let meta = serde_json::json!({"ui/resourceUri": "ui://app/index.html"});
    assert_eq!(
        canonical_resource_uri(Some(&meta)).as_deref(),
        Some("ui://app/index.html")
    );
}

#[tokio::test]
async fn invocation_without_canonical_seam_is_unavailable() {
    let error = McpAppsInvoker::unavailable()
        .call_tool("mcp__server__tool", serde_json::json!({}))
        .await
        .unwrap_err();
    assert_eq!(error, McpAppsInvocationError::Unavailable);
}

#[test]
fn raw_result_round_trip_preserves_standard_and_unknown_fields() {
    let raw = serde_json::json!({
        "content": [{"type": "resource", "resource": {"uri": "ui://app", "mimeType": MCP_APP_MIME_TYPE, "text": "<html/>"}}],
        "structuredContent": {"answer": 42},
        "isError": false,
        "_meta": {"ui": {"domain": "example"}, "unknown": {"nested": true}}
    });
    let encoded = serde_json::to_string(&raw).unwrap();
    let decoded: RawCallToolResult = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, raw);
}
