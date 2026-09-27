//! 远程模块的确定性测试：解析、身份归一、Debug 脱敏、失败分类与脱敏。
//!
//! 全部离线：不连接任何远程服务，不读 `.env`，不依赖网络。

use turso_serverless::Error as SdkError;

use super::credentials::{CredentialError, CredentialSource, SessionStoreCredential};
use super::endpoint::{EndpointError, RemoteEndpoint, RemoteEngine};
use super::failure;
use super::RemoteFailureClass;

const FAKE_SECRET: &str = "sentinel-credential-000000000000";
const FAKE_HOST: &str = "sentinel-db-sentinel-org.turso.io";

#[test]
fn scheme_aliases_share_one_identity() {
    let turso =
        RemoteEndpoint::parse(&format!("turso://{FAKE_HOST}"), None).expect("turso locator");
    let https = RemoteEndpoint::parse(&format!("https://{FAKE_HOST}/"), Some(RemoteEngine::Turso))
        .expect("https locator with explicit engine");
    assert_eq!(turso.engine(), RemoteEngine::Turso);
    assert_eq!(turso.locator_digest(), https.locator_digest());
}

#[test]
fn engine_comes_from_confirmed_syntax_only() {
    let from_scheme =
        RemoteEndpoint::parse(&format!("libsql://{FAKE_HOST}"), None).expect("libsql locator");
    assert_eq!(from_scheme.engine(), RemoteEngine::LibSql);
    assert_eq!(from_scheme.engine().as_str(), "libsql");
    assert_eq!(RemoteEngine::parse("turso"), Some(RemoteEngine::Turso));
    assert_eq!(RemoteEngine::parse("mysql"), None);

    // https 无法判定引擎：必须显式选择，不轮流试两种 SDK。
    assert_eq!(
        RemoteEndpoint::parse(&format!("https://{FAKE_HOST}"), None).unwrap_err(),
        EndpointError::AmbiguousEngine
    );
    assert_eq!(
        RemoteEndpoint::parse(&format!("turso://{FAKE_HOST}"), Some(RemoteEngine::LibSql))
            .unwrap_err(),
        EndpointError::EngineConflict
    );
}

#[test]
fn locator_rejects_credentials_and_sync_forms() {
    assert_eq!(
        RemoteEndpoint::parse(&format!("turso://user:{FAKE_SECRET}@{FAKE_HOST}"), None)
            .unwrap_err(),
        EndpointError::UserInfoPresent
    );
    assert_eq!(
        RemoteEndpoint::parse(&format!("turso://{FAKE_HOST}?sync_interval=1"), None).unwrap_err(),
        EndpointError::QueryOrFragmentUnsupported
    );
    assert_eq!(
        RemoteEndpoint::parse("/tmp/threads.db", None).unwrap_err(),
        EndpointError::NotARemoteUrl
    );
    assert_eq!(
        RemoteEndpoint::parse("ftp://example.invalid/db", None).unwrap_err(),
        EndpointError::UnsupportedScheme
    );
}

#[test]
fn endpoint_debug_keeps_host_and_path_out() {
    let endpoint = RemoteEndpoint::parse(
        &format!("turso://{FAKE_HOST}/sessions"),
        Some(RemoteEngine::Turso),
    )
    .expect("endpoint");
    let rendered = format!("{endpoint:?}");
    assert!(rendered.contains("official_turso_cloud_domain"));
    assert!(!rendered.contains("sentinel-db"));
    assert!(!rendered.contains("sessions"));
}

#[test]
fn credential_value_never_debug_leaks() {
    let credential = SessionStoreCredential::new(FAKE_SECRET).expect("credential");
    let rendered = format!("{credential:?}");
    assert!(!rendered.contains(FAKE_SECRET));
    assert_eq!(credential.expose(), FAKE_SECRET);
    assert_eq!(
        SessionStoreCredential::new("").unwrap_err(),
        CredentialError::EmptyValue
    );
}

#[test]
fn credential_source_is_explicit_and_not_aliased() {
    assert_eq!(
        CredentialSource::env("not a name").unwrap_err(),
        CredentialError::InvalidName
    );
    assert_eq!(
        CredentialSource::env("").unwrap_err(),
        CredentialError::InvalidName
    );
    let source = CredentialSource::env("PERI_PROBE_ABSENT_VARIABLE_9f3").expect("source");
    assert_eq!(source.name(), "PERI_PROBE_ABSENT_VARIABLE_9f3");
    assert_eq!(
        source.resolve().unwrap_err(),
        CredentialError::Missing {
            name: "PERI_PROBE_ABSENT_VARIABLE_9f3".to_owned()
        }
    );
    assert_eq!(
        format!("{source:?}"),
        "CredentialSource(env:PERI_PROBE_ABSENT_VARIABLE_9f3)"
    );
}

#[test]
fn empty_environment_variable_is_a_typed_error() {
    // 变量名唯一，避免与并行测试互相干扰。
    let name = "PERI_PROBE_EMPTY_VARIABLE_7c1";
    std::env::set_var(name, "");
    let source = CredentialSource::env(name).expect("source");
    assert_eq!(
        source.resolve().unwrap_err(),
        CredentialError::Empty {
            name: name.to_owned()
        }
    );
    std::env::remove_var(name);
}

#[test]
fn sdk_failures_map_to_stable_classes() {
    assert_eq!(
        failure::classify(&SdkError::Constraint("UNIQUE".to_owned())),
        RemoteFailureClass::Constraint
    );
    assert_eq!(
        failure::classify(&SdkError::Readonly("readonly".to_owned())),
        RemoteFailureClass::Readonly
    );
    assert_eq!(
        failure::classify(&SdkError::Busy("locked".to_owned())),
        RemoteFailureClass::Busy
    );
    assert_eq!(
        failure::classify(&SdkError::Http(
            "HTTP status 401 for https://db.turso.io".to_owned()
        )),
        RemoteFailureClass::AuthRejected
    );
    assert_eq!(
        failure::classify(&SdkError::Http("request timed out".to_owned())),
        RemoteFailureClass::Timeout
    );
    assert_eq!(
        failure::classify(&SdkError::Http("connection refused".to_owned())),
        RemoteFailureClass::Transport
    );
}

#[test]
fn domain_failure_keeps_sdk_text_out() {
    // SDK 载荷可能含 URL/凭证/SQL：领域失败只能带稳定分类文本。
    let raw = format!("HTTP status 403 for https://user:{FAKE_SECRET}@{FAKE_HOST}/v2/pipeline");
    let mapped = failure::classify(&SdkError::Http(raw.clone())).into_session_resource_error();
    let rendered = format!("{mapped}");
    assert!(!rendered.contains(FAKE_SECRET));
    assert!(!rendered.contains(FAKE_HOST));
    assert!(!rendered.contains("403"));
    assert!(rendered.contains("rejected the credential"));
}

#[test]
fn timeout_class_is_not_an_unavailable_detail() {
    let mapped = RemoteFailureClass::Timeout.into_session_resource_error();
    assert!(matches!(
        mapped.kind(),
        peri_acp_types::session_resources::SessionResourceErrorKind::Timeout
    ));
    assert_eq!(RemoteFailureClass::Timeout.as_str(), "timeout");
}
