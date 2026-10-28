#![cfg(unix)]

use peri_model::{Model, ModelRequest, ProtocolErrorKind, ResponsesConfig, ResponsesModel};

use super::request_auth;

#[test]
fn chatgpt_capability_rejects_custom_endpoint_even_under_platform_profile() {
    let directory = tempfile::tempdir().unwrap();
    let store = directory.path().join("unused-credentials");
    let auth = request_auth(
        store.clone(),
        "default".into(),
        "verified-subject".into(),
        "issued-client".into(),
    );
    let model = ResponsesModel::new(
        ResponsesConfig::new(
            "https://untrusted.example/v1".parse().unwrap(),
            "unused-api-key",
            "fixture-model",
        )
        .with_auth(auth),
    );
    let error = model
        .prepare_stream(ModelRequest::default())
        .err()
        .expect("plan auth must reject non-OpenAI endpoint before token resolution");
    assert_eq!(
        error.protocol_error().unwrap().kind(),
        ProtocolErrorKind::InvalidEndpoint
    );
    assert!(
        !store.exists(),
        "request preparation must never open the credential store"
    );
}

#[test]
fn same_verified_identity_has_stable_ref_across_store_alias_and_restart() {
    let directory = tempfile::tempdir().unwrap();
    let first = request_auth(
        directory.path().join("first"),
        "work".into(),
        "subject-one".into(),
        "client-one".into(),
    );
    let restarted = request_auth(
        directory.path().join("second"),
        "renamed".into(),
        "subject-one".into(),
        "client-one".into(),
    );
    assert_eq!(first.credential_ref(), restarted.credential_ref());
    assert!(first.credential_ref().starts_with("chatgpt:"));
    assert!(!first.credential_ref().contains("subject-one"));
    assert!(!first.credential_ref().contains("client-one"));
}

#[test]
fn reusing_local_alias_with_another_account_or_registration_changes_ref() {
    let directory = tempfile::tempdir().unwrap();
    let store = directory.path().join("credentials");
    let first = request_auth(
        store.clone(),
        "default".into(),
        "subject-one".into(),
        "client-one".into(),
    );
    let other_account = request_auth(
        store.clone(),
        "default".into(),
        "subject-two".into(),
        "client-one".into(),
    );
    let other_registration = request_auth(
        store,
        "default".into(),
        "subject-one".into(),
        "client-two".into(),
    );
    assert_ne!(first.credential_ref(), other_account.credential_ref());
    assert_ne!(first.credential_ref(), other_registration.credential_ref());
}
