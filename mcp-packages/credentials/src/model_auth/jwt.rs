use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::Deserialize;

use super::types::{AuthError, AuthResult};

#[derive(Deserialize)]
pub(super) struct Jwks {
    pub keys: Vec<Jwk>,
}

#[derive(Deserialize)]
pub(super) struct Jwk {
    kty: String,
    kid: Option<String>,
    alg: Option<String>,
    #[serde(rename = "use")]
    usage: Option<String>,
    n: Option<String>,
    e: Option<String>,
}

#[derive(Deserialize)]
struct Header {
    alg: String,
    kid: String,
    crit: Option<Vec<String>>,
    b64: Option<bool>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Audience {
    Single(String),
    Multiple(Vec<String>),
}

#[derive(Deserialize)]
pub(super) struct Claims {
    iss: String,
    pub sub: String,
    aud: Audience,
    azp: Option<String>,
    exp: u64,
    nbf: Option<u64>,
    iat: Option<u64>,
    nonce: Option<String>,
    pub email: Option<String>,
}

/// Only RS256 keys obtained from the configured issuer's trusted discovery
/// document are accepted. No URL or algorithm in an untrusted JWT selects keys.
pub(super) fn verify(
    token: &str,
    keys: &Jwks,
    issuer: &str,
    client_id: &str,
    nonce: Option<&str>,
    now: u64,
) -> AuthResult<Claims> {
    if token.len() > 96 * 1024 {
        return Err(AuthError::InvalidIdToken);
    }
    let mut parts = token.split('.');
    let header = parts.next().ok_or(AuthError::InvalidIdToken)?;
    let payload = parts.next().ok_or(AuthError::InvalidIdToken)?;
    let signature = parts.next().ok_or(AuthError::InvalidIdToken)?;
    if parts.next().is_some() || header.len() > 8192 {
        return Err(AuthError::InvalidIdToken);
    }
    let header: Header =
        serde_json::from_slice(&decode(header)?).map_err(|_| AuthError::InvalidIdToken)?;
    if header.alg != "RS256"
        || header.kid.is_empty()
        || header
            .crit
            .as_ref()
            .is_some_and(|fields| !fields.is_empty())
        || header.b64 == Some(false)
    {
        return Err(AuthError::InvalidIdToken);
    }
    let candidates: Vec<_> = keys
        .keys
        .iter()
        .filter(|key| key.kid.as_deref() == Some(&header.kid))
        .collect();
    if candidates.len() != 1 {
        return Err(AuthError::InvalidIdToken);
    }
    let key = candidates[0];
    if key.kty != "RSA"
        || key.alg.as_deref().is_some_and(|alg| alg != "RS256")
        || key.usage.as_deref().is_some_and(|usage| usage != "sig")
    {
        return Err(AuthError::InvalidIdToken);
    }
    let modulus = decode(key.n.as_deref().ok_or(AuthError::InvalidIdToken)?)?;
    let exponent = decode(key.e.as_deref().ok_or(AuthError::InvalidIdToken)?)?;
    validate_public_components(&modulus, &exponent)?;
    let signed = token.rsplit_once('.').ok_or(AuthError::InvalidIdToken)?.0;
    aws_lc_rs::signature::RsaPublicKeyComponents {
        n: &modulus,
        e: &exponent,
    }
    .verify(
        &aws_lc_rs::signature::RSA_PKCS1_2048_8192_SHA256,
        signed.as_bytes(),
        &decode(signature)?,
    )
    .map_err(|_| AuthError::InvalidIdToken)?;
    let claims: Claims =
        serde_json::from_slice(&decode(payload)?).map_err(|_| AuthError::InvalidIdToken)?;
    let (audience_matches, audience_count) = match &claims.aud {
        Audience::Single(audience) => (audience == client_id, 1),
        Audience::Multiple(audiences) => (
            audiences.iter().any(|aud| aud == client_id),
            audiences.len(),
        ),
    };
    if claims.iss != issuer
        || !audience_matches
        || claims.exp <= now
        || claims.nbf.is_some_and(|nbf| nbf > now.saturating_add(60))
        || claims.iat.is_some_and(|iat| iat > now.saturating_add(60))
        || claims.sub.is_empty()
        || claims.sub.len() > 512
        || (audience_count > 1 && claims.azp.as_deref() != Some(client_id))
        || claims.azp.as_deref().is_some_and(|azp| azp != client_id)
        || nonce.is_some_and(|expected| claims.nonce.as_deref() != Some(expected))
    {
        return Err(AuthError::InvalidIdToken);
    }
    Ok(claims)
}

/// Keep the RSA public-key acceptance boundary that ring enforced: the
/// exponent is odd, between 3 and 2^33-1, and neither component carries a
/// leading zero. aws-lc-rs accepts wider exponents, so these checks must stay
/// local to the JWT verifier.
fn validate_public_components(modulus: &[u8], exponent: &[u8]) -> AuthResult<()> {
    if modulus.is_empty()
        || modulus[0] == 0
        || exponent.is_empty()
        || exponent.len() > 5
        || exponent[0] == 0
    {
        return Err(AuthError::InvalidIdToken);
    }
    let mut value = 0_u64;
    for byte in exponent {
        value = (value << 8) | u64::from(*byte);
    }
    if !(3..=(1_u64 << 33) - 1).contains(&value) || value & 1 != 1 {
        return Err(AuthError::InvalidIdToken);
    }
    Ok(())
}

fn decode(value: &str) -> AuthResult<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| AuthError::InvalidIdToken)
}

#[cfg(test)]
#[path = "jwt_test.rs"]
mod tests;
