use std::{collections::BTreeMap, sync::Arc};

use serde_json::json;

use super::*;

fn secret(name: &str) -> SecretRef {
    SecretRef::new(name).unwrap()
}

fn stdio_config() -> DynamicMcpConfig {
    DynamicMcpConfig {
        command: Some("example-mcp".to_string()),
        args: vec!["stdio".to_string()],
        env: BTreeMap::from([("TOKEN".to_string(), secret("example-token"))]),
        cwd: Some("/tmp".to_string()),
        timeout_ms: Some(1_000),
        ..Default::default()
    }
}

#[test]
fn dto_serde_roundtrip_uses_camel_case_and_stable_state() {
    let action = DynamicMcpAction::Load(DynamicMcpLoadRequest {
        name: "example".to_string(),
        config: stdio_config(),
    });
    let value = serde_json::to_value(&action).unwrap();
    assert_eq!(value["method"], "load");
    assert_eq!(value["params"]["config"]["timeoutMs"], 1_000);
    assert_eq!(
        serde_json::from_value::<DynamicMcpAction>(value).unwrap(),
        action
    );
    assert_eq!(
        serde_json::to_value(DynamicMcpOperationState::Starting).unwrap(),
        "starting"
    );
}

#[test]
fn error_codes_have_stable_wire_values() {
    let codes = [
        DynamicMcpErrorCode::InvalidConfig,
        DynamicMcpErrorCode::SecretNotFound,
        DynamicMcpErrorCode::ConfigConflict,
        DynamicMcpErrorCode::StartRejected,
        DynamicMcpErrorCode::ConnectTimeout,
        DynamicMcpErrorCode::AuthRequired,
        DynamicMcpErrorCode::AuthFailed,
        DynamicMcpErrorCode::InitializeFailed,
        DynamicMcpErrorCode::ToolDiscoveryFailed,
        DynamicMcpErrorCode::ToolNameConflict,
        DynamicMcpErrorCode::TaskOwnerClosed,
        DynamicMcpErrorCode::NotFound,
        DynamicMcpErrorCode::ServerBusy,
        DynamicMcpErrorCode::ShutdownIncomplete,
        DynamicMcpErrorCode::Internal,
    ];
    assert!(codes
        .into_iter()
        .all(|code| { serde_json::to_value(code).unwrap().as_str() == Some(code.as_str()) }));
}

#[test]
fn secret_ref_rejects_inline_secret_value() {
    let error = serde_json::from_value::<SecretRef>(json!({
        "secretRef": "example-token",
        "value": "must-not-appear"
    }))
    .unwrap_err();
    assert!(error.to_string().contains("unknown field"));
}

#[test]
fn canonical_identity_is_order_stable_and_uses_reference_identity() {
    let mut first = stdio_config();
    first.env.insert("SECOND".to_string(), secret("second"));
    let mut second = stdio_config();
    second.env = BTreeMap::from([
        ("SECOND".to_string(), secret("second")),
        ("TOKEN".to_string(), secret("example-token")),
    ]);
    let first = first.canonicalize().unwrap();
    let second = second.canonicalize().unwrap();
    assert_eq!(first, second);
    assert_eq!(first.digest(), second.digest());

    let mut changed = stdio_config();
    changed
        .env
        .insert("TOKEN".to_string(), secret("different-ref"));
    assert_ne!(first, changed.canonicalize().unwrap());
}

#[test]
fn strict_transport_and_name_validation_fails_closed() {
    let invalid_name = DynamicMcpAction::Load(DynamicMcpLoadRequest {
        name: "a.b".to_string(),
        config: stdio_config(),
    })
    .canonicalize();
    assert!(invalid_name.is_err());

    let mut both = stdio_config();
    both.url = Some("https://example.invalid/mcp".to_string());
    assert!(both.canonicalize().is_err());
}

#[test]
fn sensitive_http_headers_require_secret_refs() {
    let config = DynamicMcpConfig {
        url: Some("https://example.invalid/mcp".to_string()),
        headers: BTreeMap::from([(
            "Authorization".to_string(),
            DynamicMcpHeaderValue::Literal("inline".to_string()),
        )]),
        ..Default::default()
    };
    assert!(config.canonicalize().is_err());
}

#[test]
fn safe_summary_removes_url_query_and_preserves_only_secret_reference_identity() {
    let config = DynamicMcpConfig {
        url: Some("https://example.invalid/mcp?token=inline-leak".to_string()),
        headers: BTreeMap::from([(
            "Authorization".to_string(),
            DynamicMcpHeaderValue::Secret(secret("example-token")),
        )]),
        ..Default::default()
    }
    .canonicalize()
    .unwrap();

    let serialized = serde_json::to_string(&config.safe_summary()).unwrap();
    assert!(!serialized.contains("inline-leak"));
    assert!(serialized.contains("example-token"));
}

#[test]
fn identity_types_roundtrip_without_being_interchangeable() {
    let operation = DynamicMcpOperationId::from_string("mcpop_test");
    let incarnation = DynamicMcpIncarnationId::from_string("mcpinc_test");
    let logical = DynamicMcpLogicalKey {
        session_id: "session-a".to_string(),
        server_name: "example".to_string(),
    };
    let instance = DynamicMcpInstanceKey {
        logical: logical.clone(),
        incarnation_id: incarnation.clone(),
    };
    assert_eq!(
        serde_json::from_value::<DynamicMcpOperationId>(serde_json::to_value(&operation).unwrap())
            .unwrap(),
        operation
    );
    assert_eq!(
        serde_json::from_value::<DynamicMcpInstanceKey>(serde_json::to_value(&instance).unwrap())
            .unwrap()
            .logical,
        logical
    );
    assert_ne!(operation.as_str(), incarnation.as_str());
}

#[test]
fn session_capability_snapshot_is_immutable_by_arc_replacement() {
    let first = Arc::new(SessionMcpCapabilitySnapshot::default());
    let second = Arc::new(SessionMcpCapabilitySnapshot {
        generation: 1,
        ..Default::default()
    });
    assert_eq!(first.generation, 0);
    assert_eq!(second.generation, 1);
}
