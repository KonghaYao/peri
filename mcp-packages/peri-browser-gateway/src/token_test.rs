use super::*;

const SECRET: &[u8] = b"gateway-secret";

#[test]
fn token_is_derived_and_stable() {
    let first = derive_token(SECRET, "s1");
    let second = derive_token(SECRET, "s1");
    assert_eq!(first, second);
    assert_eq!(first.len(), TOKEN_LEN);
}

#[test]
fn token_is_url_safe() {
    for session in ["s1", "peri-abc", "会话", ""] {
        let token = derive_token(SECRET, session);
        assert!(
            token
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "token {token:?} must be base64url"
        );
    }
}

#[test]
fn token_depends_on_secret_and_session() {
    assert_ne!(derive_token(SECRET, "s1"), derive_token(SECRET, "s2"));
    assert_ne!(derive_token(SECRET, "s1"), derive_token(b"other", "s1"));
}

#[test]
fn validate_accepts_only_the_derived_token() {
    let token = derive_token(SECRET, "s1");
    assert!(validate(SECRET, "s1", &token));
    assert!(!validate(SECRET, "s2", &token));
    assert!(!validate(b"other", "s1", &token));
    assert!(!validate(SECRET, "s1", ""));
    assert!(!validate(SECRET, "s1", &token[..TOKEN_LEN - 1]));
}

#[test]
fn constant_time_eq_matches_slice_equality() {
    assert!(constant_time_eq(b"abc", b"abc"));
    assert!(!constant_time_eq(b"abc", b"abd"));
    assert!(!constant_time_eq(b"abc", b"ab"));
    assert!(!constant_time_eq(b"", b"a"));
    assert!(constant_time_eq(b"", b""));
}
