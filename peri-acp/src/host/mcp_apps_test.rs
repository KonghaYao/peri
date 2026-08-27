use super::*;

#[test]
fn version_validation_is_fail_closed() {
    assert!(validate_versions(MCP_APPS_ENVELOPE_VERSION, MCP_APPS_PROTOCOL_VERSION).is_ok());
    let error = validate_versions("2", MCP_APPS_PROTOCOL_VERSION).unwrap_err();
    assert_eq!(error.data.unwrap()["kind"], "unsupported_envelope_version");
}

#[test]
fn initial_binding_uses_stable_apps_version() {
    let binding = initial_binding(
        "connection".into(),
        "session".into(),
        "server".into(),
        7,
        "ui://app".into(),
        "open".into(),
    );
    assert_eq!(binding.server_generation, 7);
    assert_eq!(binding.apps_protocol_version, MCP_APPS_PROTOCOL_VERSION);
}
