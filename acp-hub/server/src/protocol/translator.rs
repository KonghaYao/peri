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
/// 存在性由 instance spawn 结果判定（失败走 `AGENT_UNAVAILABLE`）。
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
/// turn_id 不入出站帧（#6 死字段清理）：turnId 由聚合器以事件序列归位，
/// 宿主侧不随 prompt 下发（§7.2），translator 无需感知。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundCtx {
    /// 最终 cwd：已认证上下文默认目录（§4.3 裁决）。
    pub cwd: String,
    /// binding 翻译后的 acp_session_id（hub session_id → 协议投递 id，§6.1）。
    pub acp_session_id: String,
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
                // agent-client-protocol（peri acp 实测）：prompt 为 ContentBlock
                // 序列（`prompt: [{type:"text",text}]`），非 message 字符串。
                // 官方 PromptRequest = {sessionId, prompt}（schema v1）——无
                // cwd/effort（#7 非官方字段清理：cwd 已由 spawn/会话绑定目录
                // 隐含，effort 由 agent 侧默认档位决定）。
                let mut params = serde_json::Map::new();
                params.insert("sessionId".to_string(), json!(ctx.acp_session_id));
                params.insert(
                    "prompt".to_string(),
                    json!([{ "type": "text", "text": payload.message }]),
                );
                Ok(OutboundMessage::JsonRpc(json!({
                    "jsonrpc": "2.0",
                    "id": rpc_id,
                    "method": "session/prompt",
                    "params": params,
                })))
            }
            ActionEnvelope::Cancel { .. } => {
                validate_cwd(&ctx.cwd)?;
                // agent-client-protocol（peri acp 实测）：session/cancel 是
                // **notification**（无 id、无响应帧），非 request。官方
                // CancelNotification = {sessionId}（schema v1）——无 cwd（#7）。
                Ok(OutboundMessage::JsonRpc(json!({
                    "jsonrpc": "2.0",
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
                        "cwd": ctx.cwd,
                    },
                })))
            }
            // chat/session-new（§8.5 当前对话内新建会话）：等价 create 序列
            // 的 `session/new` 一步——进程已存在，直接向目标会话发
            // session/new；响应含新 sessionId（coordinator 据此更新 binding）。
            ActionEnvelope::SessionNew { .. } => {
                validate_cwd(&ctx.cwd)?;
                // 复用 session_new_rpc 的帧构造（cwd/mcpServers 约定一致）；
                // 帧 id 重写为本次 translate 分配的 rpc_id——coordinator 以
                // 帧 id 为 register_rpc 键（§6.1），与 create 序列的两段式
                // （自取返回 id）不同。
                let (_, mut msg) = self.session_new_rpc(&ctx.cwd, None);
                msg["id"] = json!(rpc_id);
                Ok(OutboundMessage::JsonRpc(msg))
            }
            // create/close 不走此入口（create 两段式；close = instance/kill，
            // 由 coordinator 直接构造 InstanceKill）。
            ActionEnvelope::Create { .. } => Err(TranslateError::UnsupportedAction(
                "session/create (two-phase)",
            )),
            ActionEnvelope::Close { .. } => Err(TranslateError::UnsupportedAction(
                "session/close (instance/kill)",
            )),
            ActionEnvelope::Load { .. } => {
                Err(TranslateError::UnsupportedAction("session/load (M2)"))
            }
            ActionEnvelope::SubscribeEvents { .. } => {
                Err(TranslateError::UnsupportedAction("events/subscribe (M3)"))
            }
            ActionEnvelope::UnsubscribeEvents { .. } => {
                Err(TranslateError::UnsupportedAction("events/unsubscribe (M3)"))
            }
            // workspace 管理命令：submit 层直接执行（不经过出站翻译）。
            ActionEnvelope::WorkspaceCreate { .. } => Err(TranslateError::UnsupportedAction(
                "workspace/create (control-plane)",
            )),
            ActionEnvelope::WorkspaceRemove { .. } => Err(TranslateError::UnsupportedAction(
                "workspace/remove (control-plane)",
            )),
            ActionEnvelope::SessionList { .. } => Err(TranslateError::UnsupportedAction(
                "session/list (control-plane)",
            )),
            ActionEnvelope::ProjectCreate { .. }
            | ActionEnvelope::ProjectArchive { .. }
            | ActionEnvelope::PersistedSessionCreate { .. }
            | ActionEnvelope::PersistedSessionOpen { .. }
            | ActionEnvelope::PersistedSessionRename { .. }
            | ActionEnvelope::PersistedSessionImport { .. } => {
                Err(TranslateError::UnsupportedAction("metadata control-plane"))
            }
        }
    }

    /// 官方 `session/request_permission` 响应构造（schema v1，#1 权限机制
    /// 官方化）：
    /// result = `{ outcome: { outcome: "selected", optionId } | { outcome: "cancelled" } }`，
    /// id = agent request id 原样回显。响应帧不回 L3（JSON-RPC response 无
    /// 回执，§4.4 以 forward_ack 为确认点）。
    ///
    /// 选档规则：`Allow` → 第一个 `options[i].kind ∈ {allow_once, allow_always}`
    /// 的 `optionId`（无匹配 → 第一个元素的 `optionId` 保底）；`Deny` → 第一
    /// 个 `kind ∈ {reject_once, reject_always}` 的 `optionId`（有则
    /// `selected`+optionId；无 → `cancelled`）。kind 兼容 camelCase 别名
    /// （`allowOnce`/`allowSession`，对齐 relay 投影层 P2-e 兼容先例）。
    pub fn permission_response_rpc(
        &self,
        request_id: &serde_json::Value,
        decision: acp_hub_proto::action::PermissionDecision,
        options: &[serde_json::Value],
    ) -> serde_json::Value {
        let outcome = match decision {
            acp_hub_proto::action::PermissionDecision::Allow => {
                match pick_option_id(options, &["allow_once", "allow_always"])
                    .or_else(|| first_option_id(options))
                {
                    Some(option_id) => {
                        json!({ "outcome": "selected", "optionId": option_id })
                    }
                    // 入站校验允许空 options 数组（评审 P2-1）：无任何
                    // optionId 可回显时不得写 `"optionId": null`（官方契约
                    // selected 分支 optionId 必须为 string）——回落 cancelled，
                    // 与 Deny 分支一致。
                    None => json!({ "outcome": "cancelled" }),
                }
            }
            acp_hub_proto::action::PermissionDecision::Deny => {
                match pick_option_id(options, &["reject_once", "reject_always"]) {
                    Some(option_id) => {
                        json!({ "outcome": "selected", "optionId": option_id })
                    }
                    None => json!({ "outcome": "cancelled" }),
                }
            }
        };
        json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": { "outcome": outcome },
        })
    }

    /// create 序列第一步：`initialize` JSON-RPC（§6.2；10s 超时由 coordinator
    /// 执行）。返回 `(rpc_id, 请求帧)`。
    ///
    /// `protocolVersion` 必填（agent-client-protocol / peri acp 实测：缺省
    /// 即 `missing field protocolVersion`）。官方 InitializeRequest =
    /// `{protocolVersion, clientCapabilities?, clientInfo?}`（schema v1）——
    /// 无 cwd（#7）；`protocolVersion` 官方为 integer，值 1 合法。
    pub fn initialize_rpc(&self, cwd: &str) -> (String, serde_json::Value) {
        validate_cwd(cwd).expect("server-injected cwd must be valid");
        let rpc_id = self.alloc_rpc_id();
        let msg = json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "method": "initialize",
            "params": { "protocolVersion": 1 },
        });
        (rpc_id, msg)
    }

    /// create 序列第二步：`session/new`（M1 create 序列，§6.2；binding 30s
    /// 超时由 coordinator 执行）。返回 `(rpc_id, 请求帧)`。
    ///
    /// `mcpServers` 必填（agent-client-protocol / peri acp 实测：缺省即
    /// `missing field mcpServers`；空数组 = 无 MCP）。
    pub fn session_new_rpc(&self, cwd: &str, title: Option<&str>) -> (String, serde_json::Value) {
        validate_cwd(cwd).expect("server-injected cwd must be valid");
        let rpc_id = self.alloc_rpc_id();
        let mut params = serde_json::Map::new();
        params.insert("cwd".to_string(), json!(cwd));
        params.insert("mcpServers".to_string(), json!([]));
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

    /// create 序列第二步（历史会话恢复，§8.5）：`session/load`（M2，点击
    /// ACP 历史会话条目进入）。返回 `(rpc_id, 请求帧)`。
    ///
    /// 与 `session/new` 不同：**目标会话 id 由请求参数携带**（来自
    /// session/list 的 acp_session_id），load 响应体不含 sessionId——binding
    /// 以请求参数为准（coordinator 预绑定，回放通知先于响应到达）。
    ///
    /// `mcpServers` 必填（agent-client-protocol `LoadSessionRequest` 该字段
    /// 无 `#[serde(default)]`；缺省即 peri 反序列化失败 `-32602 missing field
    /// mcpServers`，与 `session_new_rpc` 同规则——实测必填）。
    pub fn session_load_rpc(&self, cwd: &str, session_id: &str) -> (String, serde_json::Value) {
        validate_cwd(cwd).expect("server-injected cwd must be valid");
        let rpc_id = self.alloc_rpc_id();
        let msg = json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "method": "session/load",
            "params": { "sessionId": session_id, "cwd": cwd, "mcpServers": [] },
        });
        (rpc_id, msg)
    }
}

/// 第一个 `options[i]` 的 `optionId`（保底选档；options 已在入站解析时
/// 校验为 `{optionId,name,kind}` 对象数组）。
fn first_option_id(options: &[serde_json::Value]) -> Option<String> {
    options.iter().find_map(|v| {
        v.get("optionId")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    })
}

/// 第一个 `kind` 命中 `kinds`（含 camelCase 别名）的 `optionId`。
fn pick_option_id(options: &[serde_json::Value], kinds: &[&str]) -> Option<String> {
    options.iter().find_map(|v| {
        let obj = v.as_object()?;
        let kind = obj.get("kind").and_then(serde_json::Value::as_str)?;
        if kinds
            .iter()
            .any(|k| kind == *k || camel_of(k) == Some(kind))
        {
            obj.get("optionId")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        } else {
            None
        }
    })
}

/// kebab-case → camelCase 别名（`allow_once` → `allowOnce`；`reject_once`
/// 官方无 camel 形态，防御性同兼容——评审 P2-e 先例）。
fn camel_of(kebab: &str) -> Option<&'static str> {
    match kebab {
        "allow_once" => Some("allowOnce"),
        "allow_always" => Some("allowSession"),
        "reject_once" => Some("rejectOnce"),
        "reject_always" => Some("rejectAlways"),
        _ => None,
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
