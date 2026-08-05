//! 私钥存储抽象（r2-encrypted-transfer v1）。
//!
//! 已冻结策略：
//! - 优先级：OS keyring → 0600、PBKDF2/AES-GCM 加密文件；
//! - 无 TTY 且无可用 keyring 时 **fail closed**（禁止明文或自动初始化回退）；
//! - 显式 `--keystore-path` 只允许打开**已存在**的加密 keystore；
//! - 所有 secret 类型的 Debug 均脱敏，错误信息不含任何密钥材料。
//!
//! 文件格式：`PERISYNC-KS1` 魔数(12B) + salt(16B) + versioned envelope
//! （见 `crypto::seal`/`crypto::open`，AAD 绑定 salt）。本模块只调用
//! `crypto` 的新协议 API，不触碰旧 pair-code API。

use std::fmt;
use std::io::Write;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ed25519_dalek::{Signature, SigningKey, VerifyingKey};
use ring::pbkdf2::{self, PBKDF2_HMAC_SHA256};
use ring::rand::{SecureRandom, SystemRandom};
use x25519_dalek::{PublicKey as XPublicKey, StaticSecret};
use zeroize::Zeroizing;

use crate::sync::canonical;
use crate::sync::crypto;

/// keystore 文件魔数。
pub const KEYSTORE_MAGIC: &[u8; 12] = b"PERISYNC-KS1";

/// PBKDF2-SHA256 迭代次数（计划冻结值 600k，见 03-plan §已冻结的安全语义）。
pub const KEYSTORE_PBKDF2_ITERATIONS: u32 = 600_000;

/// 文件 salt 长度（16 字节）。
pub const KEYSTORE_SALT_LEN: usize = 16;

/// keyring 服务名。
pub const KEYRING_SERVICE: &str = "peri-sync";

/// 默认 keystore 文件路径（keyring 不可用且允许回退时使用）。
pub fn default_keystore_path() -> Result<PathBuf> {
    let home = dirs_next::home_dir().context("failed to determine home directory")?;
    Ok(home.join(".peri").join("sync-keystore"))
}

/// 设备私钥材料（Debug 脱敏）。
///
/// 零化语义（依赖已启用的 crate features，升级依赖时必须保持默认 features）：
/// - `ed25519`（`SigningKey`）：ed25519-dalek 2.x 默认启用 `zeroize` feature，
///   seed 在 drop 时清零（`impl Drop for SigningKey`）；
/// - `x25519`（`StaticSecret`）：x25519-dalek `static_secrets` feature 下为
///   `Zeroizing` 包装，drop 时清零；
/// - 序列化/派生产生的中间副本一律用 `Zeroizing` 包裹：`material_to_bytes`
///   的 64B 数组、`derive_key_from_password` 的 AES key、`decrypt_file` 的
///   明文、`KeyMaterial::generate`/`material_from_bytes` 的 seed 数组。
pub struct KeyMaterial {
    pub ed25519: SigningKey,
    pub x25519: StaticSecret,
}

impl fmt::Debug for KeyMaterial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyMaterial")
            .field("ed25519", &"[REDACTED]")
            .field("x25519", &"[REDACTED]")
            .finish()
    }
}

impl KeyMaterial {
    /// 随机生成新的设备私钥材料（CSPRNG）。
    pub fn generate() -> Result<Self> {
        let rng = SystemRandom::new();
        // 中间 seed 数组用 Zeroizing 包裹，drop 时清零。
        let mut ed_seed = Zeroizing::new([0u8; 32]);
        let mut x_secret = Zeroizing::new([0u8; 32]);
        rng.fill(&mut ed_seed[..])
            .map_err(|_| anyhow::anyhow!("OS RNG failure"))?;
        rng.fill(&mut x_secret[..])
            .map_err(|_| anyhow::anyhow!("OS RNG failure"))?;
        Ok(Self {
            ed25519: SigningKey::from_bytes(&ed_seed),
            x25519: StaticSecret::from(*x_secret),
        })
    }

    /// Ed25519 身份公钥。
    pub fn ed25519_public(&self) -> VerifyingKey {
        self.ed25519.verifying_key()
    }

    /// X25519 静态公钥。
    pub fn x25519_public(&self) -> XPublicKey {
        XPublicKey::from(&self.x25519)
    }
}

/// 私钥存储接口。实现必须保证：Debug 与错误信息不泄露任何密钥材料。
pub trait SecretStore: Send + Sync {
    /// 使用设备 Ed25519 身份私钥签名。
    fn sign(&self, msg: &[u8]) -> Result<Signature>;

    /// 设备 X25519 静态私钥（用于 Noise 握手）；返回克隆，drop 时清零。
    fn x25519_private(&self) -> Result<StaticSecret>;
}

/// 序列化私钥材料为 64 字节（ed seed ‖ x secret）；返回 Zeroizing 包裹，
/// drop 时清零，避免明文副本残留栈上。
fn material_to_bytes(m: &KeyMaterial) -> Zeroizing<[u8; 64]> {
    let mut out = Zeroizing::new([0u8; 64]);
    out[..32].copy_from_slice(&m.ed25519.to_bytes());
    out[32..].copy_from_slice(&m.x25519.to_bytes());
    out
}

fn material_from_bytes(raw: &[u8]) -> Result<KeyMaterial> {
    if raw.len() != 64 {
        anyhow::bail!("invalid keystore payload length: {}", raw.len());
    }
    // 中间 seed 数组用 Zeroizing 包裹，drop 时清零。
    let mut ed_seed = Zeroizing::new([0u8; 32]);
    let mut x_secret = Zeroizing::new([0u8; 32]);
    ed_seed.copy_from_slice(&raw[..32]);
    x_secret.copy_from_slice(&raw[32..]);
    Ok(KeyMaterial {
        ed25519: SigningKey::from_bytes(&ed_seed),
        x25519: StaticSecret::from(*x_secret),
    })
}

fn derive_key_from_password(password: &str, salt: &[u8]) -> Zeroizing<[u8; crypto::AES_KEY_LEN]> {
    let mut key = Zeroizing::new([0u8; crypto::AES_KEY_LEN]);
    pbkdf2::derive(
        PBKDF2_HMAC_SHA256,
        NonZeroU32::new(KEYSTORE_PBKDF2_ITERATIONS).expect("600000 > 0"),
        salt,
        password.as_bytes(),
        &mut key[..],
    );
    key
}

/// OS keyring 存储（macOS Keychain / Windows Credential Manager / Linux
/// Secret Service）。密钥以二进制 secret 形式存于 keyring；打开时载入内存，
/// 之后的操作只使用内存材料（keyring 为持久化事实源）。
pub struct KeyringStore {
    material: KeyMaterial,
}

impl fmt::Debug for KeyringStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyringStore")
            .field("material", &"[REDACTED]")
            .finish()
    }
}

impl KeyringStore {
    /// 打开 keyring 中的设备密钥；不存在或存储不可用时返回错误（fail closed）。
    pub fn open(service: &str, account: &str) -> Result<Self> {
        let entry = keyring::Entry::new(service, account)
            .map_err(|e| anyhow::anyhow!("keyring unavailable: {e}"))?;
        let raw = entry
            .get_secret()
            .map_err(|e| anyhow::anyhow!("no device key in keyring: {e}"))?;
        let material = material_from_bytes(&raw)?;
        Ok(Self { material })
    }

    /// 把新生成的密钥写入 keyring。
    pub fn create(service: &str, account: &str, material: &KeyMaterial) -> Result<Self> {
        let entry = keyring::Entry::new(service, account)
            .map_err(|e| anyhow::anyhow!("keyring unavailable: {e}"))?;
        let raw = material_to_bytes(material);
        entry
            .set_secret(&raw[..])
            .map_err(|e| anyhow::anyhow!("failed to write keyring: {e}"))?;
        Ok(Self {
            material: KeyMaterial {
                ed25519: material.ed25519.clone(),
                x25519: material.x25519.clone(),
            },
        })
    }
}

impl SecretStore for KeyringStore {
    fn sign(&self, msg: &[u8]) -> Result<Signature> {
        use ed25519_dalek::Signer;
        Ok(self.material.ed25519.sign(msg))
    }

    fn x25519_private(&self) -> Result<StaticSecret> {
        Ok(self.material.x25519.clone())
    }
}

/// 只读探测 keyring 是否可用（不写入任何凭据）。
///
/// `NoEntry` 表示存储可用、只是没有该凭据；其它错误视为不可用。
pub fn keyring_available(service: &str) -> bool {
    match keyring::Entry::new(service, "peri-sync-probe") {
        Ok(entry) => match entry.get_password() {
            Ok(_) | Err(keyring::Error::NoEntry) => true,
            Err(_) => false,
        },
        Err(_) => false,
    }
}

/// 0600、PBKDF2/AES-GCM 加密的 keystore 文件。
pub struct FileStore {
    path: PathBuf,
    material: KeyMaterial,
}

impl fmt::Debug for FileStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileStore")
            .field("path", &self.path)
            .field("material", &"[REDACTED]")
            .finish()
    }
}

impl FileStore {
    /// 创建新的加密 keystore 文件（unix 下权限 0600；已存在则拒绝，避免覆盖）。
    ///
    /// 用 `create_new` 独占原子创建，不存在 `exists()` 的 TOCTOU 窗口；unix 下
    /// 直接在创建时以 `mode(0o600)` 限定权限——umask 只会移除权限位，不可能把
    /// 结果放宽到 0600 以上，因此也没有“先写后 chmod”的暴露窗口。
    pub fn create(path: &Path, password: &str, material: &KeyMaterial) -> Result<Self> {
        let rng = SystemRandom::new();
        let mut salt = [0u8; KEYSTORE_SALT_LEN];
        rng.fill(&mut salt)
            .map_err(|_| anyhow::anyhow!("OS RNG failure"))?;
        let key = derive_key_from_password(password, &salt);
        let aad = canonical::context("keystore", &[&canonical::b64url_nopad(&salt)])?;
        let envelope = crypto::seal(&key, aad.as_bytes(), &material_to_bytes(material)[..]);

        let mut bytes =
            Vec::with_capacity(KEYSTORE_MAGIC.len() + KEYSTORE_SALT_LEN + envelope.len());
        bytes.extend_from_slice(KEYSTORE_MAGIC);
        bytes.extend_from_slice(&salt);
        bytes.extend_from_slice(&envelope);
        let mut file = open_new_exclusive(path)?;
        file.write_all(&bytes)
            .with_context(|| format!("cannot write keystore {}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
            material: KeyMaterial {
                ed25519: material.ed25519.clone(),
                x25519: material.x25519.clone(),
            },
        })
    }

    /// 打开已存在的加密 keystore；文件缺失、密码错误、格式/版本不符一律失败
    /// （fail closed，无明文回退、无自动初始化）。
    pub fn open(path: &Path, password: &str) -> Result<Self> {
        if !path.exists() {
            anyhow::bail!("keystore does not exist: {}", path.display());
        }
        let raw = std::fs::read(path)
            .with_context(|| format!("cannot read keystore {}", path.display()))?;
        let material = decrypt_file(&raw, password)?;
        warn_if_permissive(path);
        Ok(Self {
            path: path.to_path_buf(),
            material,
        })
    }

    /// keystore 文件路径。
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl SecretStore for FileStore {
    fn sign(&self, msg: &[u8]) -> Result<Signature> {
        use ed25519_dalek::Signer;
        Ok(self.material.ed25519.sign(msg))
    }

    fn x25519_private(&self) -> Result<StaticSecret> {
        Ok(self.material.x25519.clone())
    }
}

fn decrypt_file(raw: &[u8], password: &str) -> Result<KeyMaterial> {
    let header_len = KEYSTORE_MAGIC.len() + KEYSTORE_SALT_LEN + crypto::ENVELOPE_HEADER_LEN;
    if raw.len() < header_len {
        anyhow::bail!("keystore file too short");
    }
    let (magic, rest) = raw.split_at(KEYSTORE_MAGIC.len());
    if magic != KEYSTORE_MAGIC {
        anyhow::bail!("not a peri-sync keystore file");
    }
    let (salt, envelope) = rest.split_at(KEYSTORE_SALT_LEN);
    let key = derive_key_from_password(password, salt);
    let aad = canonical::context("keystore", &[&canonical::b64url_nopad(salt)])?;
    // 解密出的明文含私钥材料，用 Zeroizing 包裹，drop 时清零。
    let plaintext = Zeroizing::new(
        crypto::open(&key, aad.as_bytes(), envelope)
            .context("keystore decryption failed (wrong password or corrupted file)")?,
    );
    material_from_bytes(&plaintext[..])
}

/// 以 `create_new(true)` 独占创建文件；已存在即失败（保留“不覆盖已存在
/// keystore”的语义）。unix 下创建时直接限定 `mode(0o600)`；Windows 无 mode
/// 概念，沿用默认 ACL（合理行为）。
fn open_new_exclusive(path: &Path) -> Result<std::fs::File> {
    #[cfg(unix)]
    let result = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
    };
    #[cfg(not(unix))]
    let result = {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
    };
    match result {
        Ok(file) => Ok(file),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            anyhow::bail!("keystore already exists: {}", path.display())
        }
        Err(e) => Err(e).with_context(|| format!("cannot create keystore {}", path.display())),
    }
}

#[cfg(unix)]
fn warn_if_permissive(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(meta) if meta.permissions().mode() & 0o077 != 0 => {
            tracing::warn!(
                path = %path.display(),
                mode = %(meta.permissions().mode() & 0o777),
                "keystore file permissions are not 0600"
            );
        }
        _ => {}
    }
}

#[cfg(not(unix))]
fn warn_if_permissive(_path: &Path) {}

/// keystore 来源解析结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeystoreSource {
    /// OS keyring（默认）。
    Keyring,
    /// 加密文件（显式 `--keystore-path`，或 keyring 不可用时的 TTY 回退）。
    File(PathBuf),
}

/// fail-closed 来源决策：
/// - 显式路径 → 必须打开已存在的加密文件（缺失由 [`FileStore::open`] 拒绝）；
/// - 无显式路径 + keyring 可用 → [`KeystoreSource::Keyring`]；
/// - 无显式路径 + keyring 不可用 + 有 TTY → 回退默认加密文件；
/// - 无显式路径 + keyring 不可用 + 无 TTY → 错误（fail closed，禁止明文/自动初始化）。
pub fn resolve_source(
    explicit_path: Option<&Path>,
    keyring_available: bool,
    has_tty: bool,
) -> Result<KeystoreSource> {
    if let Some(path) = explicit_path {
        return Ok(KeystoreSource::File(path.to_path_buf()));
    }
    if keyring_available {
        return Ok(KeystoreSource::Keyring);
    }
    if !has_tty {
        anyhow::bail!(
            "no usable OS keyring and no TTY to unlock the encrypted keystore: \
             refusing to fall back to plaintext"
        );
    }
    Ok(KeystoreSource::File(default_keystore_path()?))
}
