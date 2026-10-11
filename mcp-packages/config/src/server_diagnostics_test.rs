use super::failure;
use peri_acp_types::configuration::ConfigurationErrorKind;
use std::io;

#[test]
fn configuration_failure_preserves_classification_and_content() {
    let diagnostic = "denied /tmp/fixture https://fixture.invalid?token=synthetic";
    let failure = failure(io::Error::new(io::ErrorKind::PermissionDenied, diagnostic));
    assert!(matches!(
        failure.kind,
        ConfigurationErrorKind::PermissionDenied
    ));
    assert_eq!(failure.message, diagnostic);
}
