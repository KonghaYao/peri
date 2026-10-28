use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::json;

use super::*;

// Synthetic fixture private key, generated exclusively for offline tests.
fn signed(claims: serde_json::Value) -> String {
    let key = ring::signature::RsaKeyPair::from_pkcs8(include_bytes!("fixtures/test-only-rsa.pk8"))
        .unwrap();
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","kid":"peri-synthetic-test-key"}"#);
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
    let input = format!("{header}.{payload}");
    let mut signature = vec![0; key.public().modulus_len()];
    key.sign(
        &ring::signature::RSA_PKCS1_SHA256,
        &ring::rand::SystemRandom::new(),
        input.as_bytes(),
        &mut signature,
    )
    .unwrap();
    format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature))
}

fn claims() -> serde_json::Value {
    json!({"iss":"https://auth.openai.com","sub":"user-one","aud":"issued-client","exp":2000,"iat":900,"nonce":"expected-nonce","email":"synthetic@example.test"})
}

fn check(value: serde_json::Value) -> AuthResult<Claims> {
    let jwks = serde_json::from_str(include_str!("fixtures/test-only-jwks.json")).unwrap();
    verify(
        &signed(value),
        &jwks,
        "https://auth.openai.com",
        "issued-client",
        Some("expected-nonce"),
        1000,
    )
}

#[test]
fn verified_identity_comes_from_signed_claims() {
    let result = check(claims()).unwrap();
    assert_eq!(result.sub, "user-one");
    assert_eq!(result.email.as_deref(), Some("synthetic@example.test"));
}

#[test]
fn wrong_issuer_audience_nonce_or_expiry_is_rejected() {
    for (field, value) in [
        ("iss", json!("https://evil.test")),
        ("aud", json!("different-client")),
        ("nonce", json!("other-nonce")),
        ("exp", json!(1000)),
        ("nbf", json!(5000)),
        ("iat", json!(5000)),
    ] {
        let mut value_claims = claims();
        value_claims[field] = value;
        assert!(
            matches!(check(value_claims), Err(AuthError::InvalidIdToken)),
            "field {field}"
        );
    }
}

#[test]
fn multiple_audiences_require_matching_authorized_party() {
    let mut value = claims();
    value["aud"] = json!(["issued-client", "other-client"]);
    assert!(matches!(
        check(value.clone()),
        Err(AuthError::InvalidIdToken)
    ));
    value["azp"] = json!("issued-client");
    assert_eq!(check(value).unwrap().sub, "user-one");
}

#[test]
fn altered_signature_unsigned_token_and_duplicate_key_are_rejected() {
    let mut jwks: Jwks =
        serde_json::from_str(include_str!("fixtures/test-only-jwks.json")).unwrap();
    let token = signed(claims());
    let (input, _) = token.rsplit_once('.').unwrap();
    let corrupt = format!("{input}.{}", URL_SAFE_NO_PAD.encode(vec![0; 256]));
    assert!(matches!(
        verify(
            &corrupt,
            &jwks,
            "https://auth.openai.com",
            "issued-client",
            Some("expected-nonce"),
            1000
        ),
        Err(AuthError::InvalidIdToken)
    ));
    let unsigned = format!(
        "{}.{}.",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"none","kid":"peri-synthetic-test-key"}"#),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims()).unwrap())
    );
    assert!(matches!(
        verify(
            &unsigned,
            &jwks,
            "https://auth.openai.com",
            "issued-client",
            None,
            1000
        ),
        Err(AuthError::InvalidIdToken)
    ));
    jwks.keys.push(
        serde_json::from_str::<Jwks>(include_str!("fixtures/test-only-jwks.json"))
            .unwrap()
            .keys
            .remove(0),
    );
    assert!(matches!(
        verify(
            &token,
            &jwks,
            "https://auth.openai.com",
            "issued-client",
            Some("expected-nonce"),
            1000
        ),
        Err(AuthError::InvalidIdToken)
    ));
}
