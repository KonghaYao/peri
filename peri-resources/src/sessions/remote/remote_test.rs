//! PostgreSQL locator validation and password handling never require a live server.

use super::credentials::{CredentialError, CredentialSource, SessionStoreCredential};
use super::endpoint::{EndpointError, RemoteEndpoint};
use super::RemoteFailureClass;

const FAKE_SECRET: &str = "sentinel-credential-000000000000";
const FAKE_HOST: &str = "db.example.invalid";

#[test]
fn postgres_dsn_reaches_transport_without_disclosing_parameters() {
    for scheme in ["postgres", "postgresql"] {
        let dsn =
            format!("{scheme}://alice:{FAKE_SECRET}@{FAKE_HOST}:5444/sessions?sslmode=verify-full");
        let endpoint = RemoteEndpoint::parse(&dsn).unwrap();
        assert_eq!(endpoint.sdk_url(), dsn);
        assert_eq!(format!("{endpoint:?}"), "RemoteEndpoint(PostgreSQL)");
    }
}

#[test]
fn old_remote_adapters_are_rejected() {
    for scheme in ["libsql", "turso", "http", "https"] {
        assert_eq!(
            RemoteEndpoint::parse(&format!("{scheme}://{FAKE_HOST}/db")).unwrap_err(),
            EndpointError::UnsupportedScheme
        );
    }
    assert_eq!(
        RemoteEndpoint::parse("postgres://user:password@host/db#part").unwrap_err(),
        EndpointError::UnparsableUrl
    );
}

#[test]
fn unknown_driver_options_are_rejected_without_logging_their_values() {
    let error = RemoteEndpoint::parse("postgres://db.invalid/peri?apikey=secret").unwrap_err();
    assert_eq!(error, EndpointError::UnsupportedConnectParameter);
    assert!(!format!("{error}").contains("secret"));
}

#[test]
fn password_override_is_optional_and_redacted() {
    let credential = SessionStoreCredential::try_new(FAKE_SECRET).expect("credential");
    assert!(!format!("{credential:?}").contains(FAKE_SECRET));
    assert_eq!(credential.expose(), Some(FAKE_SECRET));
    assert_eq!(SessionStoreCredential::default().expose(), None);
    assert_eq!(
        SessionStoreCredential::try_new("").unwrap_err(),
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
fn timeout_class_is_not_an_unavailable_detail() {
    let mapped = RemoteFailureClass::Timeout.into_session_resource_error();
    assert!(matches!(
        mapped.kind(),
        peri_acp_types::session_resources::SessionResourceErrorKind::Timeout
    ));
    assert_eq!(RemoteFailureClass::Timeout.as_str(), "timeout");
}
