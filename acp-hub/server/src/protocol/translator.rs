//! Translator：出站翻译（架构 §6.1，设计稿 `f5-channel-control.md` §4）。
//!
//! 客户端 Action → ACP JSON-RPC（`session/prompt`/`session/cancel`/
//! `permission.resolve`/`initialize`/`session/new`）。`cwd` 由 server 按已
//! 认证上下文注入（§4.3 裁决——客户端字段不可覆盖 binding），`rpcId` 由
//! server 分配（避免消息被当作 notification，§6.1）。
//!
//! create 序列两段式（§6.2）：`initialize` → `session/new` 由 coordinator
//! 流程分两次调用（[`Translator::initialize_rpc`] / [`Translator::session_new_rpc`]）。

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::json;

use acp_hub_proto::action::ActionEnvelope;

/// 客户端 cwd 形态校验（§4.3 裁决）：绝对路径 + 无 NUL/控制字符 + ≤ 4KB。
/// 存在性由 machine spawn 结果判定（失败走 `AGENT_UNAVAILABLE`）。
pub const CWD_MAX_BYTES: usize = 4096;

/// 出站翻译错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TranslateError {
    /// 该 Action 不在 M1 出站方法面（Load/SubscribeEvents 等，§4.3）。
    #[error("unsupported action for outbound translation: {0}")]
    UnsupportedAction(&'static str),
    /// 缺省 cwd 且 server 未注入默认目录（配置缺失）。
    #[error("cwd required")]
    MissingCwd,
    /// cwd 形态非法（相对路径 / NUL、控制字符 / 超长）。
    #[error("invalid cwd: {0}")]
    BadCwd(&'static str),
}

/// 出站上下文（server 按连接绑定注入；客户端字段不可覆盖 binding，§4.3）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundCtx {
    /// 最终 cwd：已认证上下文默认目录（§4.3 裁决）。
    pub cwd: String,
    /// binding 翻译后的 acp_session_id（hub session_id → 协议投递 id，§6.1）。
    pub acp_session_id: String,
    /// prompt 注入的 turnId（§4.4 生成规则：server 生成 uuid，同 commandId
    /// 重试复用）。随 `session/prompt` 下发——machine 侧 ACP 会话沿用同一
    /// turnId，聚合器以 turnId 为幂等键（§6.3/§6.5），事件 turn_id 必须与
    /// 之一致；非 prompt 方法面不使用。
    pub turn_id: String,
}

/// 出站产物。
#[derive(Debug, Clone, PartialEq)]
pub enum OutboundMessage {
    /// 单条 JSON-RPC（带 id）：prompt/cancel/resolve。
    JsonRpc(serde_json::Value),
    /// `session/new`（M1 create 序列第二步，§6.2）。
    SessionNew(serde_json::Value),
}

/// 出站翻译器（§6.1 出站翻译边界）。`cwd` 由 server 注入，`rpcId` 由
/// server 分配。
#[derive(Debug, Default)]
pub struct Translator {
    next_rpc_id: AtomicU64,
}

impl Translator {
    /// 空翻译器（rpcId 从 1 起）。
    pub fn new() -> Self {
        Translator::default()
    }

    /// rpcId 分配【决策】：全局单调，格式 `hub-{n}`（n 从 1 起）。文档只要求
    /// 「server 分配」，未指定形态；全局计数避免 per-session 状态，与
    /// pending_rpc 表（relay 模块）以字符串匹配。
    pub fn alloc_rpc_id(&self) -> String {
        let n = self.next_rpc_id.fetch_add(1, Ordering::Relaxed) + 1;
        format!("hub-{n}")
    }

    /// 翻译入口（M1 方法面子集：prompt/cancel/resolve；create 序列见
    /// [`Translator::initialize_rpc`]/[`Translator::session_new_rpc`]）。
    pub fn translate(
        &self,
        action: &ActionEnvelope,
        ctx: &OutboundCtx,
    ) -> Result<OutboundMessage, TranslateError> {
        let rpc_id = self.alloc_rpc_id();
        match action {
            ActionEnvelope::Prompt { payload, .. } => {
                validate_cwd(&ctx.cwd)?;
                Ok(OutboundMessage::JsonRpc(json!({
                    "jsonrpc": "2.0",
                    "id": rpc_id,
                    "method": "session/prompt",
                    "params": {
                        "sessionId": ctx.acp_session_id,
                        "message": payload.message,
                        // server 生成的 turnId（§4.4）：machine 侧 ACP 会话
                        // 沿用同一 id，事件 turn_id 与聚合器幂等键对齐（§6.5）。
                        "turnId": ctx.turn_id,
                    },
                })))
            }
            ActionEnvelope::Cancel { .. } => {
                validate_cwd(&ctx.cwd)?;
                Ok(OutboundMessage::JsonRpc(json!({
                    "jsonrpc": "2.0",
                    "id": rpc_id,
                    "method": "session/cancel",
                    "params": { "sessionId": ctx.acp_session_id },
                })))
            }
            ActionEnvelope::ResolvePermission { payload, .. } => {
                validate_cwd(&ctx.cwd)?;
                Ok(OutboundMessage::JsonRpc(json!({
                    "jsonrpc": "2.0",
                    "id": rpc_id,
                    "method": "permission.resolve",
                    "params": {
                        "permissionId": payload.permission_id,
                        "decision": match payload.decision {
                            acp_hub_proto::action::PermissionDecision::Allow => "allow",
                            acp_hub_proto::action::PermissionDecision::Deny => "deny",
                        },
                    },
                })))
            }
            // create/close 不走此入口（create 两段式；close = machine/kill，
            // 由 coordinator 直接构造 MachineKill）。
            ActionEnvelope::Create { .. } => {
                Err(TranslateError::UnsupportedAction("session/create (two-phase)"))
            }
            ActionEnvelope::Close { .. } => {
                Err(TranslateError::UnsupportedAction("session/close (machine/kill)"))
            }
            ActionEnvelope::Load { .. } => Err(TranslateError::UnsupportedAction("session/load (M2)")),
            ActionEnvelope::SubscribeEvents { .. } => {
                Err(TranslateError::UnsupportedAction("events/subscribe (M3)"))
            }
            ActionEnvelope::UnsubscribeEvents { .. } => {
                Err(TranslateError::UnsupportedAction("events/unsubscribe (M3)"))
            }
        }
    }

    /// create 序列第一步：`initialize` JSON-RPC（§6.2；10s 超时由 coordinator
    /// 执行）。返回 `(rpc_id, 请求帧)`。
    pub fn initialize_rpc(&self, cwd: &str) -> (String, serde_json::Value) {
        validate_cwd(cwd).expect("server-injected cwd must be valid");
        let rpc_id = self.alloc_rpc_id();
        let msg = json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "method": "initialize",
            "params": { "cwd": cwd },
        });
        (rpc_id, msg)
    }

    /// create 序列第二步：`session/new`（M1 create 序列，§6.2；binding 30s
    /// 超时由 coordinator 执行）。返回 `(rpc_id, 请求帧)`。
    pub fn session_new_rpc(&self, cwd: &str, title: Option<&str>) -> (String, serde_json::Value) {
        validate_cwd(cwd).expect("server-injected cwd must be valid");
        let rpc_id = self.alloc_rpc_id();
        let mut params = serde_json::Map::new();
        params.insert("cwd".to_string(), json!(cwd));
        if let Some(t) = title {
            params.insert("title".to_string(), json!(t));
        }
        let msg = json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "method": "session/new",
            "params": params,
        });
        (rpc_id, msg)
    }
}

/// cwd 形态校验（§4.3 裁决）：绝对路径 + 无 NUL/控制字符 + ≤ 4KB。
///
/// M1 默认目录 = **server 进程工作目录**（常驻进程由托管系统设定，是「已认证
/// 上下文」可得的唯一稳定目录；ws 无法获取 TUI 本地 cwd）。
pub fn validate_cwd(cwd: &str) -> Result<(), TranslateError> {
    if cwd.is_empty() {
        return Err(TranslateError::MissingCwd);
    }
    if cwd.len() > CWD_MAX_BYTES {
        return Err(TranslateError::BadCwd("too long"));
    }
    if !cwd.starts_with('/') {
        return Err(TranslateError::BadCwd("relative path"));
    }
    if cwd
        .chars()
        .any(|c| c == '\0' || c.is_control() || c == '\n' || c == '\r')
    {
        return Err(TranslateError::BadCwd("control characters"));
    }
    Ok(())
}

#[cfg(test)]
#[path = "translator_test.rs"]
mod translator_test;
