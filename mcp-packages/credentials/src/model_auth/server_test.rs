use crate::model_auth::types::AuthError;
use crate::model_auth::types::ExpectedIdentity;
use crate::model_auth::types::StoredAccount;

#[test]
fn identity_requires_subject_client_and_active_generation() {
    let mut account = StoredAccount::new("host".into());
    account.subject = Some("subject".into());
    account.client_id = Some("client".into());
    account.binding = Some("session-generation".into());
    let expected = ExpectedIdentity {
        subject: "subject".into(),
        client_id: "client".into(),
    };
    assert_eq!(
        crate::model_auth::server::validate_identity(
            &account,
            &expected,
            Some("session-generation")
        ),
        Ok(())
    );
    account.subject = Some("different-subject".into());
    assert_eq!(
        crate::model_auth::server::validate_identity(
            &account,
            &expected,
            Some("session-generation")
        ),
        Err(AuthError::AccountChanged)
    );
    account.subject = Some("subject".into());
    account.binding = None;
    assert_eq!(
        crate::model_auth::server::validate_identity(
            &account,
            &expected,
            Some("session-generation")
        ),
        Err(AuthError::AccountChanged)
    );
}

#[test]
fn only_definitive_refresh_failures_invalidate_rotating_token() {
    for code in [
        "invalid_grant",
        "refresh_token_reused",
        "refresh_token_invalidated",
        "invalid_refresh_token",
    ] {
        assert!(crate::model_auth::server::refresh_is_terminal(
            &AuthError::OAuth {
                code: code.into(),
                description: String::new()
            }
        ));
    }
    for code in ["temporarily_unavailable", "server_error", "invalid_client"] {
        assert!(!crate::model_auth::server::refresh_is_terminal(
            &AuthError::OAuth {
                code: code.into(),
                description: String::new()
            }
        ));
    }
}
