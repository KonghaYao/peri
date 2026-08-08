//! HMAC 双向认证密码原语（§9.2 顾问3 线格式精度）。
//!
//! **纯函数**：无 I/O、无连接状态——nonce 单次使用等状态在 server `auth`
//! 模块（本模块仅提供无 I/O 的 [`SeenNonces`] 防重放集合，供 server 持有）。
//!
//! 线格式要点（实现必须满足）：
//!
//! 1. 算法 `HMAC-SHA256`，输出 base64（RFC 4648 标准字母表 + padding）；
//! 2. MAC 输入按**固定字节序（大端，u16 长度前缀）**拼接
//!    （`challenge_nonce ‖ session_context ‖ protocol_version ‖ role`），
//!    字段顺序即文档顺序，不得重排；
//! 3. 比较常量时间（`hmac::Mac::verify_slice` 内建）；
//! 4. 密钥经 HKDF-SHA256 派生（salt 空、info = `b"acp-hub-auth" ‖ role`），
//!    **token 本体不出现在 MAC 输入**；
//! 5. 协议级属性（状态在 server）：nonce 单次使用 + 30s 窗口、session_context
//!    连接绑定、角色/版本绑定、失败即断开（关闭码 4502）+ 审计计数。
//!
//! client（TUI）连接**无**双向认证（§9.2 仅覆盖 machine 连接）。

use std::collections::HashSet;
use std::time::Duration;

use base64::Engine as _;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand::Rng as _;
use sha2::Sha256;

/// challenge_nonce 长度：32B CSPRNG 原始字节（§9.2 顾问3）。
pub const CHALLENGE_NONCE_LEN: usize = 32;

/// session_context 长度：32B CSPRNG 原始字节（§9.2 顾问3）。
pub const SESSION_CONTEXT_LEN: usize = 32;

/// HMAC-SHA256 输出长度。
pub const HMAC_OUTPUT_LEN: usize = 32;

/// challenge 短期有效窗口（§9.2「短期有效窗口 30s 过期」）。
pub const NONCE_TTL: Duration = Duration::from_secs(30);

/// 派生上下文前缀（HKDF info = `b"acp-hub-auth" ‖ role`）——随字节级测试向量
/// 固化（设计文档 §10【决策】）。
pub const DERIVE_INFO_PREFIX: &[u8] = b"acp-hub-auth";

type HmacSha256 = Hmac<Sha256>;

/// HMAC 校验错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HmacError {
    /// MAC 输出长度非 32B（或 nonce/context 长度非 32B 传入）。
    #[error("bad length")]
    BadLength,
    /// `expected_b64` 不是合法 base64。
    #[error("invalid base64")]
    InvalidBase64,
    /// 校验失败（MAC 不匹配）。
    #[error("MAC mismatch")]
    Mismatch,
}

/// 生成一次性 challenge_nonce（32B CSPRNG）。
pub fn generate_challenge_nonce() -> [u8; CHALLENGE_NONCE_LEN] {
    let mut b = [0u8; CHALLENGE_NONCE_LEN];
    rand::rng().fill_bytes(&mut b);
    b
}

/// 生成连接级 session_context（32B CSPRNG）。
pub fn generate_session_context() -> [u8; SESSION_CONTEXT_LEN] {
    let mut b = [0u8; SESSION_CONTEXT_LEN];
    rand::rng().fill_bytes(&mut b);
    b
}

/// HKDF-SHA256 派生单连接密钥（§9.2 顾问3）。
///
/// `ikm = machine_token`（32B）；salt = 空（RFC 5869 零串）；
/// info = `b"acp-hub-auth" ‖ role_utf8`；输出 32B。派生上下文含 role，
/// 防止跨角色重放；token 本体不出现在 MAC 输入。
pub fn derive_mac_key(machine_token: &[u8; CHALLENGE_NONCE_LEN], role: &str) -> [u8; HMAC_OUTPUT_LEN] {
    let mut info = Vec::with_capacity(DERIVE_INFO_PREFIX.len() + role.len());
    info.extend_from_slice(DERIVE_INFO_PREFIX);
    info.extend_from_slice(role.as_bytes());

    let hk = Hkdf::<Sha256>::new(None, machine_token);
    let mut okm = [0u8; HMAC_OUTPUT_LEN];
    hk.expand(&info, &mut okm)
        .expect("32-byte output is within HKDF-SHA256 limits");
    okm
}

/// MAC 输入规范化（§9.2 顾问3）：`challenge_nonce ‖ session_context ‖
/// protocol_version ‖ role`，每字段 = **u16 大端长度前缀 + UTF-8 字节**。
///
/// challenge/session_context 为 32B **原始字节**（非 base64）；protocol_version
/// /role 用其 UTF-8 表示（如 `"1"`、`"machine"`）。字段顺序即文档顺序，
/// 不得重排。
pub fn mac_input(
    challenge: &[u8; CHALLENGE_NONCE_LEN],
    context: &[u8; SESSION_CONTEXT_LEN],
    protocol_version: &str,
    role: &str,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 * 2 + 32 + 32 + protocol_version.len() + role.len());
    push_length_prefixed(&mut out, challenge);
    push_length_prefixed(&mut out, context);
    push_length_prefixed(&mut out, protocol_version.as_bytes());
    push_length_prefixed(&mut out, role.as_bytes());
    out
}

/// 计算 MAC：`HMAC-SHA256(key, input)`。
pub fn compute_mac(key: &[u8; HMAC_OUTPUT_LEN], input: &[u8]) -> [u8; HMAC_OUTPUT_LEN] {
    let mut mac = new_hmac(key);
    mac.update(input);
    mac.finalize().into_bytes().into()
}

/// 常量时间校验 MAC（§9.2 顾问3：比较常量时间，杜绝时序侧信道）。
///
/// 流程：base64 解码 `expected_b64`（失败 → [`HmacError::InvalidBase64`]）→
/// 长度防御（非 32B → [`HmacError::BadLength`]）→
/// `Mac::verify_slice`（crate 内建常量时间比较，失败 → [`HmacError::Mismatch`]）。
pub fn verify_mac(
    key: &[u8; HMAC_OUTPUT_LEN],
    input: &[u8],
    expected_b64: &str,
) -> Result<(), HmacError> {
    let expected = base64::engine::general_purpose::STANDARD
        .decode(expected_b64)
        .map_err(|_| HmacError::InvalidBase64)?;
    if expected.len() != HMAC_OUTPUT_LEN {
        return Err(HmacError::BadLength);
    }
    let mut mac = new_hmac(key);
    mac.update(input);
    mac.verify_slice(&expected)
        .map_err(|_| HmacError::Mismatch)
}

/// 防重放辅助集合（纯内存，无 I/O）。
///
/// 语义由调用方（server `auth` 模块）执行：challenge_nonce 单次使用 +
/// [`NONCE_TTL`] 过期 + 连接断开即失效；本类型只提供无 I/O 的去重记录容器。
#[derive(Debug, Clone, Default)]
pub struct SeenNonces {
    seen: HashSet<[u8; CHALLENGE_NONCE_LEN]>,
}

impl SeenNonces {
    /// 空集合。
    pub fn new() -> Self {
        Self::default()
    }

    /// 是否已见过该 nonce（重放查询，不修改集合）。
    pub fn contains(&self, nonce: &[u8; CHALLENGE_NONCE_LEN]) -> bool {
        self.seen.contains(nonce)
    }

    /// 首次使用登记：**返回 `true` 表示该 nonce 从未见过并已记录**（可放行）；
    /// 返回 `false` 表示重放（拒绝）。
    pub fn check_and_mark(&mut self, nonce: &[u8; CHALLENGE_NONCE_LEN]) -> bool {
        self.seen.insert(*nonce)
    }

    /// 已记录 nonce 数量。
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

fn new_hmac(key: &[u8]) -> HmacSha256 {
    // HMAC 接受任意长度密钥，32B 必然合法。
    HmacSha256::new_from_slice(key).expect("HMAC accepts keys of any length")
}

fn push_length_prefixed(out: &mut Vec<u8>, bytes: &[u8]) {
    let len = u16::try_from(bytes.len()).expect("MAC input field exceeds u16 length prefix");
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(bytes);
}

#[cfg(test)]
#[path = "hmac_test.rs"]
mod hmac_test;
