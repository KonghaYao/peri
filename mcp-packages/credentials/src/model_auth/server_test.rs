use super::*;

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
        validate_identity(&account, &expected, Some("session-generation")),
        Ok(())
    );
    account.subject = Some("different-subject".into());
    assert_eq!(
        validate_identity(&account, &expected, Some("session-generation")),
        Err(AuthError::AccountChanged)
    );
    account.subject = Some("subject".into());
    account.binding = None;
    assert_eq!(
        validate_identity(&account, &expected, Some("session-generation")),
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
        assert!(refresh_is_terminal(&AuthError::OAuth {
            code: code.into(),
            description: String::new()
        }));
    }
    for code in ["temporarily_unavailable", "server_error", "invalid_client"] {
        assert!(!refresh_is_terminal(&AuthError::OAuth {
            code: code.into(),
            description: String::new()
        }));
    }
}
