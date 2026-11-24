//! 每会话 monitor token：`base64url(sha256(secret ++ sessionId))[..22]`。
//! 派生值只对当前 Live 记录有效；网关重启不恢复旧记录或旧地址。

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use sha2::{Digest, Sha256};

/// Number of base64url characters kept from the digest.
pub const TOKEN_LEN: usize = 22;

/// Derives the monitor token for `session_id`.
#[must_use]
pub fn derive_token(secret: &[u8], session_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(secret);
    hasher.update(session_id.as_bytes());
    let digest = hasher.finalize();
    let encoded = URL_SAFE_NO_PAD.encode(digest);
    encoded[..TOKEN_LEN].to_owned()
}

/// Checks `candidate` against the token derived for `session_id`.
#[cfg(test)]
#[must_use]
pub fn validate(secret: &[u8], session_id: &str, candidate: &str) -> bool {
    constant_time_eq(
        derive_token(secret, session_id).as_bytes(),
        candidate.as_bytes(),
    )
}

/// Compares two byte strings without early exit on the first difference.
///
/// The length check leaks only the public length; the content comparison scans
/// every byte so a caller cannot learn a matching prefix from timing.
#[must_use]
pub fn constant_time_eq(expected: &[u8], candidate: &[u8]) -> bool {
    if expected.len() != candidate.len() {
        return false;
    }
    let mut diff = 0u8;
    for (left, right) in expected.iter().zip(candidate) {
        diff |= left ^ right;
    }
    diff == 0
}

#[cfg(test)]
#[path = "token_test.rs"]
mod tests;
