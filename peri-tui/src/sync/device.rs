//! 设备身份与信任（r2-encrypted-transfer v1）。
//!
//! 每台机器持有 Ed25519 身份签名密钥与 X25519 静态密钥；**仅公钥**进入
//! `identity.json`、trusted peer 文件与服务端。私钥存于 keystore
//! （见 [`crate::sync::keystore`]）。
//!
//! 首次信任通过 `peri://device/<id>?ed=<pub>&x=<pub>&n=<name>` 邀请文本完成；
//! 用户人工确认双方公钥 fingerprint 后才写入 `trusted_peers.json`。没有用户
//! 账户或服务器撤销表；本地 untrust 后不得再选择该 peer。

use std::fmt;
use std::path::Path;
use std::str::FromStr;

use anyhow::{Context, Result};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signature, VerifyingKey};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use x25519_dalek::PublicKey as XPublicKey;

use crate::sync::canonical;
use crate::sync::keystore::SecretStore;
use crate::sync::limits;

/// 设备 ID：16 随机字节，base64url-no-pad 显示。
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeviceId([u8; 16]);

impl DeviceId {
    /// 生成新的随机设备 ID（CSPRNG）。
    pub fn random() -> Result<Self> {
        let rng = SystemRandom::new();
        let mut bytes = [0u8; 16];
        rng.fill(&mut bytes)
            .map_err(|_| anyhow::anyhow!("OS RNG failure"))?;
        Ok(Self(bytes))
    }

    /// 原始字节。
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// base64url-no-pad 编码。
    pub fn to_b64(&self) -> String {
        canonical::b64url_nopad(&self.0)
    }

    /// 从 base64url-no-pad 解析；长度不符即拒绝。
    pub fn from_b64(s: &str) -> Result<Self> {
        let bytes = URL_SAFE_NO_PAD
            .decode(s)
            .context("invalid device id encoding")?;
        if bytes.len() != 16 {
            anyhow::bail!("device id must decode to 16 bytes, got {}", bytes.len());
        }
        let mut id = [0u8; 16];
        id.copy_from_slice(&bytes);
        Ok(Self(id))
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_b64())
    }
}

impl fmt::Debug for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_b64())
    }
}

impl FromStr for DeviceId {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::from_b64(s)
    }
}

impl Serialize for DeviceId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_b64())
    }
}

impl<'de> Deserialize<'de> for DeviceId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::from_b64(&s).map_err(serde::de::Error::custom)
    }
}

/// 32 字节公钥的 base64url-no-pad serde 辅助。
mod b64_32 {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8; 32], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&URL_SAFE_NO_PAD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u8; 32], D::Error> {
        let s = String::deserialize(deserializer)?;
        let bytes = URL_SAFE_NO_PAD
            .decode(&s)
            .map_err(serde::de::Error::custom)?;
        if bytes.len() != 32 {
            return Err(serde::de::Error::custom("expected 32 bytes"));
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes);
        Ok(out)
    }
}

/// 设备公开身份（`identity.json` / 邀请 / trusted peer 记录只含公钥）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DevicePublic {
    pub device_id: DeviceId,
    /// Ed25519 身份公钥（base64url-no-pad）。
    #[serde(with = "b64_32")]
    pub ed_pub: [u8; 32],
    /// X25519 静态公钥（base64url-no-pad）。
    #[serde(with = "b64_32")]
    pub x_pub: [u8; 32],
    /// 用户可见设备名（不参与任何签名 transcript）。
    pub name: String,
}

impl DevicePublic {
    /// 由公钥与名称构造；名称非法（空/超长）即拒绝。
    pub fn from_keys(
        device_id: DeviceId,
        ed_pub: VerifyingKey,
        x_pub: XPublicKey,
        name: &str,
    ) -> Result<Self> {
        limits::validate_device_name(name)?;
        Ok(Self {
            device_id,
            ed_pub: ed_pub.to_bytes(),
            x_pub: x_pub.to_bytes(),
            name: name.to_string(),
        })
    }

    /// Ed25519 验证公钥。
    pub fn ed_verifying_key(&self) -> Result<VerifyingKey> {
        VerifyingKey::from_bytes(&self.ed_pub).context("invalid ed25519 public key")
    }

    /// X25519 公钥。
    pub fn x_public(&self) -> XPublicKey {
        XPublicKey::from(self.x_pub)
    }

    /// 人工核对指纹：SHA-256(ed_pub ‖ x_pub) 前 16 字节，hex，每 4 字符一组。
    pub fn fingerprint(&self) -> String {
        let mut input = [0u8; 64];
        input[..32].copy_from_slice(&self.ed_pub);
        input[32..].copy_from_slice(&self.x_pub);
        let digest = ring::digest::digest(&ring::digest::SHA256, &input);
        let hex: String = digest.as_ref()[..16]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        format!(
            "{}-{}-{}-{}-{}-{}-{}-{}",
            &hex[0..4],
            &hex[4..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..24],
            &hex[24..28],
            &hex[28..32]
        )
    }

    /// 邀请文本：`peri://device/<id>?ed=<pub>&x=<pub>&n=<name>`。
    pub fn invite_uri(&self) -> String {
        format!(
            "peri://device/{}?ed={}&x={}&n={}",
            self.device_id.to_b64(),
            canonical::b64url_nopad(&self.ed_pub),
            canonical::b64url_nopad(&self.x_pub),
            percent_encode(&self.name),
        )
    }

    /// 解析邀请文本；任何字段缺失/非法即拒绝。
    pub fn parse_invite_uri(uri: &str) -> Result<Self> {
        let rest = uri
            .strip_prefix("peri://device/")
            .context("not a peri device invite")?;
        let (id_part, query) = rest.split_once('?').context("missing query parameters")?;
        let device_id = DeviceId::from_b64(id_part)?;
        let mut ed_pub: Option<[u8; 32]> = None;
        let mut x_pub: Option<[u8; 32]> = None;
        let mut name: Option<String> = None;
        for pair in query.split('&') {
            let (key, value) = pair.split_once('=').context("malformed query parameter")?;
            let decoded = percent_decode(value)?;
            match key {
                "ed" => ed_pub = Some(decode_pub(&decoded)?),
                "x" => x_pub = Some(decode_pub(&decoded)?),
                "n" => name = Some(decoded),
                _ => {} // 忽略未知参数（向前兼容）
            }
        }
        let ed_pub = ed_pub.context("missing ed parameter")?;
        let x_pub = x_pub.context("missing x parameter")?;
        let name = name.context("missing n parameter")?;
        limits::validate_device_name(&name)?;
        Ok(Self {
            device_id,
            ed_pub,
            x_pub,
            name,
        })
    }
}

fn decode_pub(s: &str) -> Result<[u8; 32]> {
    let bytes = URL_SAFE_NO_PAD
        .decode(s)
        .context("invalid public key encoding")?;
    if bytes.len() != 32 {
        anyhow::bail!("public key must decode to 32 bytes, got {}", bytes.len());
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

/// RFC 3986 unreserved 之外的字节做百分号编码。
fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn percent_decode(s: &str) -> Result<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                if i + 3 > bytes.len() {
                    anyhow::bail!("truncated percent escape");
                }
                let hex =
                    std::str::from_utf8(&bytes[i + 1..i + 3]).context("bad percent escape")?;
                let value = u8::from_str_radix(hex, 16).context("bad percent escape")?;
                out.push(value);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).context("invite field is not valid UTF-8")
}

/// 已信任设备记录。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrustedPeer {
    pub device_id: DeviceId,
    #[serde(with = "b64_32")]
    pub ed_pub: [u8; 32],
    #[serde(with = "b64_32")]
    pub x_pub: [u8; 32],
    pub name: String,
    /// 首次信任时间（unix 秒）。
    pub trusted_at: u64,
}

impl TrustedPeer {
    /// 从设备公开身份构造信任记录。
    pub fn from_device(device: &DevicePublic, trusted_at: u64) -> Self {
        Self {
            device_id: device.device_id,
            ed_pub: device.ed_pub,
            x_pub: device.x_pub,
            name: device.name.clone(),
            trusted_at,
        }
    }

    /// 与 [`DevicePublic::fingerprint`] 相同的指纹格式。
    pub fn fingerprint(&self) -> String {
        let mut input = [0u8; 64];
        input[..32].copy_from_slice(&self.ed_pub);
        input[32..].copy_from_slice(&self.x_pub);
        let digest = ring::digest::digest(&ring::digest::SHA256, &input);
        let hex: String = digest.as_ref()[..16]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        format!(
            "{}-{}-{}-{}-{}-{}-{}-{}",
            &hex[0..4],
            &hex[4..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..24],
            &hex[24..28],
            &hex[28..32]
        )
    }
}

/// 本地 trusted peers 存储（`trusted_peers.json`）。
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct TrustedPeers {
    #[serde(default)]
    pub peers: Vec<TrustedPeer>,
}

impl TrustedPeers {
    /// 加载文件；文件不存在视为空列表（首次使用）。
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read trusted peers file {}", path.display()))?;
        let peers: Self = serde_json::from_str(&raw)
            .with_context(|| format!("invalid trusted peers file {}", path.display()))?;
        Ok(peers)
    }

    /// 保存到文件（JSON，带缩进；原子写：临时文件 + rename，unix 下 0600）。
    ///
    /// rename 在同一文件系统内原子替换，读方永远不会看到半写内容；unix 下
    /// 临时文件以 0600 创建（内容仅公钥，权限属习惯性防御）。
    pub fn save(&self, path: &Path) -> Result<()> {
        let raw = serde_json::to_string_pretty(self)?;
        let tmp = path.with_extension("json.tmp");
        write_atomic(&tmp, raw.as_bytes())?;
        std::fs::rename(&tmp, path).with_context(|| {
            format!(
                "cannot replace trusted peers file {} with {}",
                path.display(),
                tmp.display()
            )
        })?;
        Ok(())
    }

    pub fn get(&self, device_id: &DeviceId) -> Option<&TrustedPeer> {
        self.peers.iter().find(|p| &p.device_id == device_id)
    }

    pub fn contains(&self, device_id: &DeviceId) -> bool {
        self.get(device_id).is_some()
    }

    /// 添加信任；device_id 已存在时拒绝（必须先 untrust）。
    pub fn add(&mut self, peer: TrustedPeer) -> Result<()> {
        if self.contains(&peer.device_id) {
            anyhow::bail!("device {} is already trusted", peer.device_id);
        }
        self.peers.push(peer);
        Ok(())
    }

    /// 解除信任；返回是否移除。
    pub fn remove(&mut self, device_id: &DeviceId) -> bool {
        let before = self.peers.len();
        self.peers.retain(|p| &p.device_id != device_id);
        self.peers.len() != before
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    pub fn len(&self) -> usize {
        self.peers.len()
    }
}

/// 对 canonical transcript 签名：`peri-sync/v1|op|field...|unix_seconds`。
pub fn sign_transcript(
    store: &dyn SecretStore,
    op: &str,
    fields: &[&str],
    unix_secs: u64,
) -> Result<Signature> {
    let msg = canonical::transcript(op, fields, unix_secs)?;
    store.sign(msg.as_bytes())
}

/// 校验 canonical transcript 签名；任何篡改/密钥不符均失败。
pub fn verify_transcript(
    ed_pub: &VerifyingKey,
    op: &str,
    fields: &[&str],
    unix_secs: u64,
    signature: &Signature,
) -> Result<()> {
    use ed25519_dalek::Verifier;
    let msg = canonical::transcript(op, fields, unix_secs)?;
    ed_pub
        .verify(msg.as_bytes(), signature)
        .map_err(|_| anyhow::anyhow!("invalid transcript signature"))
}

/// 以 0600（unix）写出临时文件内容（create/truncate；调用方随后 rename）。
///
/// unix 下创建时直接以 `mode(0o600)` 限定权限，不受 umask 放宽影响。
fn write_atomic(tmp: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(tmp)
    };
    #[cfg(not(unix))]
    let file = {
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(tmp)
    };
    let mut file =
        file.with_context(|| format!("cannot write trusted peers file {}", tmp.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("cannot write trusted peers file {}", tmp.display()))?;
    Ok(())
}
