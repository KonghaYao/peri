//! 连接生命周期帧（§4.2 / §4.6 / §4.7 / §9.2）与 Doc 标识。
//!
//! 包含：`auth`（单向 token 校验）、`auth_response`（§9.2 server 身份证明）、
//! `ready` 握手、`keep_alive`/`pong` 心跳、`DocId` 与关闭码常量。

use std::borrow::Cow;
use std::collections::HashMap;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// `auth` 帧载荷：C→S，连接后第一帧（§4.2）。
///
/// 角色由 token 解析，客户端不声明 role（架构 §9.5：token 即身份）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Auth {
    pub token: String,
}

/// `auth_response` 帧载荷：S→M server 身份证明（§9.2 步骤 2）。
///
/// machine 校验通过前不执行任何 spawn/kill；校验失败即断开（关闭码 4502 +
/// 审计计数）。`hmac` 为
/// `HMAC-SHA256(derive_mac_key(token, role), mac_input(nonce, context, version, role))`
/// 的 base64 输出（见 [`crate::hmac`]）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthResponse {
    /// 连接级 `session_context`（32B CSPRNG，base64）。
    ///
    /// 【决策】生成方为 server：随 auth_response 下发（machine 需其作为 MAC
    /// 输入）。文档仅规定「连接级随机 id」与「32B 原始字节」，未指定生成方/
    /// 传递帧——此为最小实现选择。
    pub session_context: String,
    /// HMAC-SHA256 输出，base64（§10 hmac 模块）。
    pub hmac: String,
}

/// `ready` 帧载荷：快照推送完成握手（§4.6 步骤 4）。
///
/// 携带各 Doc 的 `projection_version`，远端据此判断是否需要校准显示；
/// 置 `relayReady = true` 后 flush 缓冲的 Action。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ready {
    pub projection_versions: HashMap<DocId, u32>,
}

/// `keep_alive` 帧载荷：S→C 心跳（§4.7，载荷为 ping）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeepAlive {}

/// `pong` 帧载荷：C→S keep_alive 回执（§4.7）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pong {}

/// Doc 名称 newtype（§5.2 表）：`chat:{sid}` / `session:{sid}` / `hub:registry`。
///
/// 序列化为透明字符串（`ysync.subscribe` 的 `{ docs: ["chat:{sid}", ...] }`
/// 形态）。`FromStr` 校验 `{sid}` 段为合法标识符（ASCII 字母数字 +
/// `-`/`_`/`.`，非空、不含 `:`），防止 doc 名注入。
///
/// 内部用 [`Cow<'static, str>`] 承载：`REGISTRY` 需为 `const`（设计文档 §8
/// `pub const REGISTRY: Self`），而 Rust `const` 无法构造非空 `String`，
/// 借用静态字面量是最小形态。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DocId(Cow<'static, str>);

/// DocId 解析错误（`{sid}` 段不合法）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid doc id: {0}")]
pub struct DocIdError(pub String);

impl DocId {
    /// `chat:{sid}`——消息时间线 Doc（§5.2）。
    pub fn chat(sid: &str) -> Self {
        DocId(Cow::Owned(format!("chat:{sid}")))
    }

    /// `session:{sid}`——会话控制状态 Doc（§5.2）。
    pub fn session(sid: &str) -> Self {
        DocId(Cow::Owned(format!("session:{sid}")))
    }

    /// `hub:registry`——机器 + 活跃会话摘要 Doc（§5.2）。
    pub const REGISTRY: DocId = DocId(Cow::Borrowed("hub:registry"));

    /// 返回 doc 的完整名称（含前缀），如 `chat:{sid}`。
    pub fn as_str(&self) -> &str {
        self.0.as_ref()
    }
}

impl Default for DocId {
    fn default() -> Self {
        DocId::REGISTRY
    }
}

impl std::fmt::Display for DocId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0.as_ref())
    }
}

impl FromStr for DocId {
    type Err = DocIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (prefix, sid) = s
            .split_once(':')
            .ok_or_else(|| DocIdError(s.to_string()))?;
        if prefix != "chat" && prefix != "session" && prefix != "hub" {
            return Err(DocIdError(s.to_string()));
        }
        if sid.is_empty()
            || !sid
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return Err(DocIdError(s.to_string()));
        }
        Ok(DocId(Cow::Owned(s.to_string())))
    }
}

// ---------------------------------------------------------------------------
// 关闭码（§4.7）
// ---------------------------------------------------------------------------

/// 机器离线：停止自动重连，展示手动重试。
pub const CLOSE_MACHINE_OFFLINE: u16 = 4500;

/// keep_alive 超时：不在后台自动重连。
pub const CLOSE_KEEPALIVE_TIMEOUT: u16 = 4501;

/// 配置性永久失败（spawn 配置错误、machine 认证失败 §9.2 步骤 3）：停止自动重连。
pub const CLOSE_CONFIG_FATAL: u16 = 4502;

/// 通用失败：退避重连。
pub const CLOSE_GENERIC_FAILURE: u16 = 1011;

/// 连接配额超限：退避重连。
pub const CLOSE_QUOTA_EXCEEDED: u16 = 1013;
