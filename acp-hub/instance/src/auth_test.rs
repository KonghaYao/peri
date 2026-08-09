//! auth 单测（§9.2）：hello 构造（新 nonce）、auth_response 校验成功/失败、
//! 重连语义（新 nonce 使旧报文失效）。

use super::*;
use acp_hub_proto::hmac::{
    compute_mac, derive_mac_key, generate_connection_context, mac_input, CHALLENGE_NONCE_LEN,
};

/// 固定测试 token（32B 全 0 → base64 44 字符）。
fn test_token() -> String {
    base64::engine::general_purpose::STANDARD.encode([0u8; CHALLENGE_NONCE_LEN])
}

fn token_bytes(token: &str) -> [u8; CHALLENGE_NONCE_LEN] {
    base64::engine::general_purpose::STANDARD
        .decode(token)
        .unwrap()
        .try_into()
        .unwrap()
}

/// 以「server 侧」逻辑构造合法 auth_response（复用 proto hmac 原语）。
fn valid_auth_response(
    nonce: &[u8; CHALLENGE_NONCE_LEN],
    token: &str,
    context: &[u8; CHALLENGE_NONCE_LEN],
) -> AuthResponse {
    let key = derive_mac_key(&token_bytes(token), ROLE);
    let input = mac_input(nonce, context, &PROTOCOL_VERSION.to_string(), ROLE);
    let mac = compute_mac(&key, &input);
    AuthResponse {
        connection_context: base64::engine::general_purpose::STANDARD.encode(context),
        hmac: base64::engine::general_purpose::STANDARD.encode(mac),
    }
}

#[test]
fn test_client_new_rejects_bad_token() {
    assert!(matches!(
        AuthClient::new("too-short".to_string()),
        Err(AuthError::BadToken)
    ));
    // 44 字符但非 base64。
    assert!(matches!(
        AuthClient::new("!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!".to_string()),
        Err(AuthError::BadToken)
    ));
    // 合法 token。
    assert!(AuthClient::new(test_token()).is_ok());
}

#[test]
fn test_begin_generates_fresh_nonce() {
    let client = AuthClient::new(test_token()).unwrap();
    let a = client.begin();
    let b = client.begin();
    assert_ne!(a.nonce(), b.nonce(), "每次握手必须新 nonce（§9.2 挑战新鲜性）");
    assert_eq!(a.nonce().len(), CHALLENGE_NONCE_LEN);
}

#[test]
fn test_build_hello_fields() {
    let client = AuthClient::new(test_token()).unwrap();
    let session = client.begin();
    let mut epochs = HashMap::new();
    epochs.insert("s1".to_string(), 2u64);
    let hello = session.build_hello(&HelloCtx {
        hostname: "h1".to_string(),
        buffered: true,
        buffer_lost: false,
        stream_epochs: epochs,
    });
    assert_eq!(hello.token, test_token());
    assert_eq!(hello.hostname, "h1");
    assert_eq!(hello.buffered, Some(true));
    assert_eq!(hello.buffer_lost, Some(false));
    assert_eq!(hello.stream_epochs, Some(HashMap::from([("s1".to_string(), 2u64)])));
    // nonce 为本次连接 nonce 的 base64。
    assert_eq!(
        base64::engine::general_purpose::STANDARD.decode(&hello.nonce).unwrap(),
        session.nonce().as_slice()
    );
}

#[test]
fn test_verify_valid_response() {
    let client = AuthClient::new(test_token()).unwrap();
    let session = client.begin();
    let context = generate_connection_context();
    let resp = valid_auth_response(session.nonce(), &test_token(), &context);
    assert!(session.verify_auth_response(&resp).is_ok());
}

#[test]
fn test_verify_rejects_forged_hmac() {
    let client = AuthClient::new(test_token()).unwrap();
    let session = client.begin();
    let context = generate_connection_context();
    let mut resp = valid_auth_response(session.nonce(), &test_token(), &context);
    // 篡改 hmac（换一字节后重编码）。
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&resp.hmac)
        .unwrap();
    let mut forged = bytes.clone();
    forged[0] ^= 0x01;
    resp.hmac = base64::engine::general_purpose::STANDARD.encode(forged);
    assert_eq!(
        session.verify_auth_response(&resp),
        Err(AuthError::Hmac(HmacError::Mismatch))
    );
}

#[test]
fn test_verify_rejects_bad_hmac_length() {
    let client = AuthClient::new(test_token()).unwrap();
    let session = client.begin();
    let context = generate_connection_context();
    let resp = AuthResponse {
        connection_context: base64::engine::general_purpose::STANDARD.encode(context),
        hmac: base64::engine::general_purpose::STANDARD.encode([0u8; 16]), // 非 32B
    };
    assert_eq!(
        session.verify_auth_response(&resp),
        Err(AuthError::Hmac(HmacError::BadLength))
    );
}

#[test]
fn test_verify_rejects_bad_session_context() {
    let client = AuthClient::new(test_token()).unwrap();
    let session = client.begin();
    let resp = AuthResponse {
        connection_context: "not-base64!!".to_string(),
        hmac: base64::engine::general_purpose::STANDARD.encode([0u8; 32]),
    };
    assert!(matches!(
        session.verify_auth_response(&resp),
        Err(AuthError::Malformed(_))
    ));
}

#[test]
fn test_verify_rejects_replayed_old_response_on_new_session() {
    // 重连语义（§9.2）：新握手用新 nonce，旧连接的 auth_response 校验失败
    // （instance 侧以新 nonce 保证自己不重放）。
    let client = AuthClient::new(test_token()).unwrap();
    let old = client.begin();
    let context = generate_connection_context();
    let old_resp = valid_auth_response(old.nonce(), &test_token(), &context);

    let new = client.begin();
    assert_eq!(
        new.verify_auth_response(&old_resp),
        Err(AuthError::Hmac(HmacError::Mismatch)),
        "旧连接响应在重连后必须失效"
    );
}
