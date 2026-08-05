use std::fmt;
use std::num::NonZeroU32;

use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit, Payload},
};
use ring::pbkdf2::{self, PBKDF2_HMAC_SHA256};
use ring::rand::{SecureRandom, SystemRandom};
use zeroize::Zeroizing;

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
    let rng = SystemRandom::new();
    let mut nonce_bytes = [0u8; IV_LEN];
    rng.fill(&mut nonce_bytes).expect("OS RNG should not fail");
    let nonce = Nonce::try_from(&nonce_bytes[..]).expect("12 bytes should construct a valid Nonce");
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
    let nonce = Nonce::try_from(iv).map_err(|e| anyhow::anyhow!("invalid nonce: {e}"))?;
    let plaintext = cipher
        .decrypt(&nonce, ciphertext)
        .map_err(|e| anyhow::anyhow!("AES-GCM decryption failed: {e}"))?;
    Ok(plaintext)
}

// ═══════════════════════════════════════════════════════════════════════
// 新协议 API（r2-encrypted-transfer v1）
//
// 与上方旧的 pair-code API 完全隔离：新模块只允许调用本段 API，禁止新增
// 任何 pair-code 调用点。载荷 envelope 为 `[version(1)][nonce(12)]
// [ciphertext+tag(16)]`；AAD 必须经 [`payload_aad`] 构造（绑定协议版本、
// channel ID、part index 与 manifest hash）。
// ═══════════════════════════════════════════════════════════════════════

/// envelope 版本字节。
pub const ENVELOPE_VERSION: u8 = 1;

/// envelope 头部长度：版本(1) + nonce(12)。
pub const ENVELOPE_HEADER_LEN: usize = 1 + IV_LEN;

/// AEAD 认证标签长度（AES-256-GCM，16 字节）。
pub const AEAD_TAG_LEN: usize = 16;

/// 进程内存中的 channel data key（32 字节，drop 时清零，Debug 脱敏）。
///
/// v1 语义：data key 仅驻留进程内存；进程重启后中止该 transfer，用户需重新
/// send/receive。禁止把 data key 写入 staging 或任意磁盘。
#[derive(Clone)]
pub struct DataKey(Zeroizing<[u8; AES_KEY_LEN]>);

impl DataKey {
    /// 生成 channel 新鲜随机 data key（CSPRNG）。
    pub fn random() -> anyhow::Result<Self> {
        let rng = SystemRandom::new();
        let mut key = [0u8; AES_KEY_LEN];
        rng.fill(&mut key)
            .map_err(|_| anyhow::anyhow!("OS RNG failure"))?;
        Ok(Self(Zeroizing::new(key)))
    }

    /// 从 32 字节构造（Noise msg1 载荷提取路径）。
    pub fn from_array(key: [u8; AES_KEY_LEN]) -> Self {
        Self(Zeroizing::new(key))
    }

    /// 以 `[u8; 32]` 形式访问。
    pub fn as_array(&self) -> &[u8; AES_KEY_LEN] {
        &self.0
    }
}

impl std::ops::Deref for DataKey {
    type Target = [u8; AES_KEY_LEN];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl fmt::Debug for DataKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("DataKey").field(&"[REDACTED]").finish()
    }
}

/// 构造 payload 版本化 AAD：
/// `peri-sync/v1|payload|<channel_id>|<part_index>|<manifest_hash_b64>`。
///
/// `channel_id` 为 16 字节 base64url-no-pad 字符串；`manifest_hash` 为
/// manifest 的 SHA-256 摘要字节（AAD 内以 base64url-no-pad 编码）。
pub fn payload_aad(
    channel_id: &str,
    part_index: u64,
    manifest_hash: &[u8],
) -> anyhow::Result<Vec<u8>> {
    Ok(crate::sync::canonical::context(
        "payload",
        &[
            channel_id,
            &part_index.to_string(),
            &crate::sync::canonical::b64url_nopad(manifest_hash),
        ],
    )?
    .into_bytes())
}

/// 使用版本化 envelope 加密：`[version(1)][nonce(12)][ciphertext+tag(16)]`。
pub fn seal(key: &[u8; AES_KEY_LEN], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let cipher = Aes256Gcm::new_from_slice(key).expect("AES-256 key must be 32 bytes");
    let rng = SystemRandom::new();
    let mut nonce_bytes = [0u8; IV_LEN];
    rng.fill(&mut nonce_bytes).expect("OS RNG should not fail");
    let nonce = Nonce::try_from(&nonce_bytes[..]).expect("12 bytes should construct a valid Nonce");
    let ciphertext = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .expect("AES-GCM encryption should not fail with valid inputs");

    let mut result = Vec::with_capacity(ENVELOPE_HEADER_LEN + ciphertext.len());
    result.push(ENVELOPE_VERSION);
    result.extend_from_slice(&nonce_bytes);
    result.extend_from_slice(&ciphertext);
    result
}

/// 校验版本与认证标签后解密；任何篡改/版本不符/长度不足均返回错误。
///
/// 错误消息不含密钥材料。
pub fn open(key: &[u8; AES_KEY_LEN], aad: &[u8], envelope: &[u8]) -> anyhow::Result<Vec<u8>> {
    if envelope.len() < ENVELOPE_HEADER_LEN {
        anyhow::bail!(
            "envelope too short: {} bytes, need at least {}",
            envelope.len(),
            ENVELOPE_HEADER_LEN
        );
    }
    if envelope[0] != ENVELOPE_VERSION {
        anyhow::bail!("unsupported envelope version: {}", envelope[0]);
    }
    let (_, rest) = envelope.split_at(1);
    let (nonce_bytes, ciphertext) = rest.split_at(IV_LEN);
    let cipher = Aes256Gcm::new_from_slice(key).expect("AES-256 key must be 32 bytes");
    let nonce = Nonce::try_from(nonce_bytes).map_err(|_| anyhow::anyhow!("invalid nonce"))?;
    cipher
        .decrypt(
            &nonce,
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| anyhow::anyhow!("AES-GCM authentication failed"))
}
