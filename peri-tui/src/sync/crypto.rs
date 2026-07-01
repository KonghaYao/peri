use std::num::NonZeroU32;

use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, AeadCore, KeyInit, OsRng},
};
use ring::pbkdf2::{self, PBKDF2_HMAC_SHA256};

/// AES-256 密钥长度（32 字节）
pub const AES_KEY_LEN: usize = 32;

/// AES-GCM IV（nonce）长度（12 字节）
pub const IV_LEN: usize = 12;

/// PBKDF2-SHA256 迭代次数
pub const PBKDF2_ITERATIONS: u32 = 100_000;

/// 数据分片大小（64KB）
pub const CHUNK_SIZE: usize = 65536;

/// 从配对码派生 AES-256 密钥
///
/// 使用 PBKDF2-SHA256，salt 为配对码本身，迭代 100000 次。
/// 相同的配对码始终产���相同的密钥，用于 sender 和 receiver 之间的端到端加密。
pub fn derive_key(pair_code: &str) -> [u8; AES_KEY_LEN] {
    let mut key = [0u8; AES_KEY_LEN];
    pbkdf2::derive(
        PBKDF2_HMAC_SHA256,
        NonZeroU32::new(PBKDF2_ITERATIONS).expect("100000 > 0"),
        pair_code.as_bytes(),
        pair_code.as_bytes(),
        &mut key,
    );
    key
}

/// AES-256-GCM 加密
///
/// 随机生成 12 字节 IV，返回 `IV(12B) + ciphertext + auth_tag(16B)` 的拼接。
pub fn encrypt(plaintext: &[u8], key: &[u8; AES_KEY_LEN]) -> Vec<u8> {
    let cipher = Aes256Gcm::new_from_slice(key).expect("AES-256 key must be 32 bytes");
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, plaintext)
        .expect("AES-GCM encryption should not fail with valid inputs");

    let mut result = Vec::with_capacity(IV_LEN + ciphertext.len());
    result.extend_from_slice(&nonce);
    result.extend_from_slice(&ciphertext);
    result
}

/// AES-256-GCM 解密
///
/// 从 `IV(12B) + ciphertext + auth_tag(16B)` 格式的数据中提取 IV 并解密。
/// 返回解密后的明文，认证失败时返回错误。
pub fn decrypt(encrypted_data: &[u8], key: &[u8; AES_KEY_LEN]) -> anyhow::Result<Vec<u8>> {
    if encrypted_data.len() < IV_LEN {
        anyhow::bail!(
            "encrypted data too short: {} bytes, need at least {}",
            encrypted_data.len(),
            IV_LEN
        );
    }

    let (iv, ciphertext) = encrypted_data.split_at(IV_LEN);
    let cipher = Aes256Gcm::new_from_slice(key).expect("AES-256 key must be 32 bytes");
    let nonce = Nonce::from_slice(iv);
    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| anyhow::anyhow!("AES-GCM decryption failed: {e}"))?;
    Ok(plaintext)
}
