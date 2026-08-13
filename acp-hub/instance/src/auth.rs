//! instance 侧双向认证（§9.2）：hello 构造（nonce 每次连接新生成）+ auth_response
//! HMAC 校验 + 认证状态机。
//!
//! 复用 `acp-hub-proto` 密码原语（`hmac.rs`），不重复实现：nonce 生成、HKDF
//! 密钥派生、MAC 输入规范化、常量时间校验。
//!
//! 脱敏纪律（§9.3）：token 本体/派生密钥/HMAC 输出永不进入日志与错误 Display
//! （[`AuthError`] 只携带失败类别）。
//!
//! 握手语义（§9.2 步骤 1–3）：instance 发起连接 → 发 `instance/hello`（含 token
//! 与本次连接**新生成**的 32B challenge_nonce）→ server 以 HMAC 应答证明身份。
//! instance 校验通过前不执行任何 spawn/kill；校验失败即断开（关闭码 4502 +
//! 审计计数），**不自动重连**（防冒充 server 反复投毒）。

use std::collections::HashMap;

use base64::Engine as _;
use thiserror::Error;

use acp_hub_proto::conn::AuthResponse;
use acp_hub_proto::hmac::{
    derive_mac_key, generate_challenge_nonce, mac_input, verify_mac, HmacError,
    CHALLENGE_NONCE_LEN, CONNECTION_CONTEXT_LEN,
};
use acp_hub_proto::instance::InstanceHello;
use acp_hub_proto::version::PROTOCOL_VERSION;

/// HMAC 派生 role 字符串（§9.2：仅 instance 连接走双向认证，取值恒为 `"instance"`）。
const ROLE: &str = "instance";

/// token 本体长度：32B CSPRNG → base64 标准字母表 44 字符（§9.2.1，与 server 侧
/// `TOKEN_B64_LEN` 对齐）。
const TOKEN_B64_LEN: usize = 44;

/// 认证失败原因（脱敏：不含 token/nonce/hmac/connection_context 本体，§9.3）。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AuthError {
    /// token 非 44 字符 base64（或解码非 32B）。
    #[error("token 非法（需 {TOKEN_B64_LEN} 字符 base64）")]
    BadToken,
    /// auth_response 载荷结构非法（connection_context 非 base64 32B 等）。
    #[error("auth_response 载荷非法: {0}")]
    Malformed(String),
    /// HMAC 校验失败（base64 非法 / 长度非 32B / 不匹配）。
    #[error("HMAC 校验失败: {0}")]
    Hmac(#[from] HmacError),
}

/// 认证客户端：持有 token（配置注入，不落日志），可开启多次握手（重连重新握手，
/// §9.2「重连时新 nonce、新连接上下文」）。
#[derive(Debug, Clone)]
pub struct AuthClient {
    /// token 明文（44 字符 base64，仅 hello 载荷携带与密钥派生，不落日志）。
    token: String,
    /// 派生的单机密钥（token 不变则密钥不变；[`derive_mac_key`] HKDF 派生，
    /// token 本体不出现在 MAC 输入）。
    key: [u8; 32],
}

/// hello 上下文（由 hub 在每次握手时填充会话状态，§6.3）。
#[derive(Debug, Clone)]
pub struct HelloCtx {
    /// 机器 hostname（§4.5 `instance/hello.hostname`）。
    pub hostname: String,
    /// 任一 session 有待补推缓冲（`hello.buffered`）。
    pub buffered: bool,
    /// daemon 启动发生过缓冲丢失（重启后 true，§7.5）。
    pub buffer_lost: bool,
    /// 存活 session 的当前流纪元映射（`hello.stream_epochs`，§4.5.1）。
    pub stream_epochs: HashMap<String, u64>,
}

/// 单次连接的认证会话：challenge_nonce 一次性（§9.2），连接级绑定。
#[derive(Debug)]
pub struct AuthSession {
    auth: AuthClient,
    nonce: [u8; CHALLENGE_NONCE_LEN],
}

impl AuthClient {
    /// 以 token 构建客户端；token 必须为 44 字符 base64（解码 32B），fail-fast
    /// 于连接建立前（防启动后反复握手失败）。
    pub fn new(token: String) -> Result<Self, AuthError> {
        let trimmed = token.trim().to_string();
        if trimmed.len() != TOKEN_B64_LEN {
            return Err(AuthError::BadToken);
        }
        let bytes: [u8; CHALLENGE_NONCE_LEN] = base64::engine::general_purpose::STANDARD
            .decode(&trimmed)
            .map_err(|_| AuthError::BadToken)?
            .try_into()
            .map_err(|_| AuthError::BadToken)?;
        let key = derive_mac_key(&bytes, ROLE);
        Ok(AuthClient {
            token: trimmed,
            key,
        })
    }

    /// 开启一次握手：生成**新 nonce**（每次连接新生成，§9.2 挑战新鲜性）。
    pub fn begin(&self) -> AuthSession {
        AuthSession {
            auth: self.clone(),
            nonce: generate_challenge_nonce(),
        }
    }
}

impl AuthSession {
    /// 本次连接的 challenge_nonce 原始字节（32B）。
    pub fn nonce(&self) -> &[u8; CHALLENGE_NONCE_LEN] {
        &self.nonce
    }

    /// 构造 `instance/hello`（§4.5）：token + 本次连接 nonce + hostname + caps +
    /// 缓冲水位/纪元映射。caps M1 不透明透传（【决策】文档未展开结构）。
    pub fn build_hello(&self, ctx: &HelloCtx) -> InstanceHello {
        InstanceHello {
            token: self.auth.token.clone(),
            hostname: ctx.hostname.clone(),
            caps: serde_json::json!({}),
            buffered: Some(ctx.buffered),
            buffer_lost: Some(ctx.buffer_lost),
            stream_epochs: Some(ctx.stream_epochs.clone()),
            nonce: base64::engine::general_purpose::STANDARD.encode(self.nonce),
        }
    }

    /// 校验 auth_response（§9.2 步骤 2–3，常量时间）：
    /// `key = derive_mac_key(token, "instance")`；
    /// `input = mac_input(nonce, connection_context, PROTOCOL_VERSION, "instance")`。
    ///
    /// `connection_context` 来自应答（server 生成，§9.2），base64 解码为 32B 后
    /// 作为 MAC 输入；`hmac` 的 base64 合法性/长度/匹配由 [`verify_mac`] 内建
    /// 常量时间比较处理。
    pub fn verify_auth_response(&self, resp: &AuthResponse) -> Result<(), AuthError> {
        let context: [u8; CONNECTION_CONTEXT_LEN] = base64::engine::general_purpose::STANDARD
            .decode(&resp.connection_context)
            .map_err(|e| AuthError::Malformed(format!("connection_context: {e}")))?
            .try_into()
            .map_err(|_| AuthError::Malformed("connection_context 非 32B".to_string()))?;
        let input = mac_input(&self.nonce, &context, &PROTOCOL_VERSION.to_string(), ROLE);
        verify_mac(&self.auth.key, &input, &resp.hmac)?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "auth_test.rs"]
mod auth_test;
