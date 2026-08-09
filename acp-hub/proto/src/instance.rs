//! instance 协议帧（§4.5 / §4.5.1）：server ↔ instance 全 9 帧。
//!
//! 全部 `#[serde(rename_all = "camelCase")]`。下行指令（spawn/kill）均携带
//! `command_id`，以 `chat_id` 为天然幂等键（server 可安全重发）。
//!
//! 流纪元约定（§4.5.1）：`epoch` 为 instance 侧 per-chat 流代际标识
//! （daemon 重启或 ACP 子进程重建时 +1，chat 新开为 1）；`seq` 为 instance
//! 侧单调序号（每 chat 独立）。server 持久化 `(epoch, last_seq)` 对属
//! server 内部状态，无独立线类型；本模块字段即线协议校验面。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// 下行（server → instance）
// ---------------------------------------------------------------------------

/// `instance/spawn`：启动 ACP 进程（§4.5）。
///
/// 按 `chat_id` 幂等（已存在返回现有句柄，不二次起进程）；`env` 受 server
/// 白名单约束（§9.6，proto 只承载形态，双端校验在 server/instance）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceSpawn {
    pub command_id: String,
    pub chat_id: String,
    /// ACP 启动命令（argv）。
    pub cmd: Vec<String>,
    pub cwd: String,
    /// 附加环境变量；键名受 §9.6 env 白名单约束。
    pub env: Option<HashMap<String, String>>,
}

/// `instance/kill`：停止 ACP 进程（§4.5）。
///
/// 幂等——已死成功返回；`grace` 为优雅关闭宽限（ms）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceKill {
    pub command_id: String,
    pub chat_id: String,
    /// 优雅关闭宽限（ms），缺省由 instance 决定。
    pub grace: Option<u64>,
}

/// `instance/forward`：下行 ACP JSON-RPC 指令透传（§4.5 透传语义——
/// instance 保持 dumb，不解析指令内容，只写 ACP stdin）。
///
/// 【冲突 1 裁决】M1 instance 帧集（§4.8）原缺下行转发帧（f6-instance.md 记录
/// 的未决冲突）；集成测试（F7）确认 server 下行 JSON-RPC 无传输载体。裁决：
/// 新增本帧进 M1 帧集（与 spawn/kill 同族，`command_id` 幂等），instance 写
/// stdin 成功（字节级）后回 [`InstanceForwardAck`]（L1+L2 合并确认，§4.4）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceForward {
    pub command_id: String,
    pub chat_id: String,
    /// JSON-RPC 指令（initialize / session/new / prompt / cancel /
    /// permission_resolve；server 生成，带 `id`=rpcId）。
    pub frame: serde_json::Value,
}

// ---------------------------------------------------------------------------
// 上行（instance → server）
// ---------------------------------------------------------------------------

/// `instance/hello`：注册 + 重连握手（§4.5）。
///
/// 幂等替换语义：新 hello 到达即 fencing 旧连接（旧连接事件丢弃、关闭——
/// server 行为）。`nonce` 为一次性 challenge（32B CSPRNG，base64，§9.2），
/// 供 server 身份证明（`auth_response`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceHello {
    pub token: String,
    pub hostname: String,
    /// 【决策】文档未展开 caps 结构，M1 不透明透传。
    pub caps: serde_json::Value,
    /// 断线缓冲待补推。
    pub buffered: Option<bool>,
    /// daemon 崩溃缓冲丢失（§7.5）。
    pub buffer_lost: Option<bool>,
    /// per-chat 流纪元映射（§4.5.1）。
    pub stream_epochs: Option<HashMap<String, u64>>,
    /// challenge_nonce（32B CSPRNG，base64）。
    pub nonce: String,
}

/// `instance/heartbeat`：周期心跳（默认 5s，§4.5）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceHeartbeat {
    /// 【决策】load 语义文档未展开，M1 取 0–100 整数百分比。
    pub load: u32,
    /// instance 侧当前存活的 ACP 会话（session_id）列表。
    pub alive_sessions: Vec<String>,
}

/// `instance/event`：原始 ACP 帧转发（§4.5）。
///
/// `frame` 为原始 ACP 帧（`{type,payload}` 或 JSON-RPC `session/update`，
/// §6.1），instance 保持 dumb 透传；`epoch` 与 server 记录不一致的帧直接
/// 丢弃（server 行为，§4.5.1）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceEvent {
    pub chat_id: String,
    /// 流纪元（§4.5.1）。
    pub epoch: u64,
    /// instance 侧单调序号（每 chat 独立）。
    pub seq: u64,
    /// 原始 ACP 帧（不透明 JSON）。
    pub frame: serde_json::Value,
}

/// `instance/buffer_sync`：断线缓冲补推（§4.5）。
///
/// 补推起点 `from_seq` = server 持久化 `last_seq + 1`；`epoch` 由 server
/// 回传校验，与记录不一致即拒绝该批（防旧纪元缓冲混入新纪元流）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceBufferSync {
    pub chat_id: String,
    pub epoch: u64,
    pub from_seq: u64,
    /// 每帧带 seq。
    pub frames: Vec<BufferedFrame>,
}

/// `instance/buffer_sync.frames` 单帧条目。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BufferedFrame {
    pub seq: u64,
    /// 原始 ACP 帧（不透明 JSON）。
    pub frame: serde_json::Value,
}

/// `instance/spawn_ack`：spawn 结果（§4.5）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceSpawnAck {
    pub command_id: String,
    pub chat_id: String,
    pub ok: bool,
    /// 脱敏失败原因（失败时携带）。
    pub error: Option<String>,
}

/// `instance/kill_ack`：kill 结果（§4.5）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceKillAck {
    pub command_id: String,
    pub chat_id: String,
    pub ok: bool,
}

/// `instance/forward_ack`：下行转发结果（§4.4 L1+L2 合并确认）。
///
/// `ok=true` 表示指令已完整写入 ACP 进程 stdin（字节级，§4.4 L2）；
/// `ok=false` 携带脱敏原因（进程已退出/管道关闭等），server 侧映射为
/// retryable 失败（AGENT_UNAVAILABLE）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceForwardAck {
    pub command_id: String,
    pub chat_id: String,
    pub ok: bool,
    /// 脱敏失败原因（失败时携带）。
    pub error: Option<String>,
}

/// `instance/process_exit`：ACP 进程退出事件（§4.5）。
///
/// `crashed`/`ended` 状态由此驱动（server 行为）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceProcessExit {
    pub chat_id: String,
    /// 进程退出码。
    pub code: i32,
}
