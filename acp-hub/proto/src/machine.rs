//! machine 协议帧（§4.5 / §4.5.1）：server ↔ machine 全 9 帧。
//!
//! 全部 `#[serde(rename_all = "camelCase")]`。下行指令（spawn/kill）均携带
//! `command_id`，以 `session_id` 为天然幂等键（server 可安全重发）。
//!
//! 流纪元约定（§4.5.1）：`epoch` 为 machine 侧 per-session 流代际标识
//! （daemon 重启或 ACP 子进程重建时 +1，session 新开为 1）；`seq` 为 machine
//! 侧单调序号（每 session 独立）。server 持久化 `(epoch, last_seq)` 对属
//! server 内部状态，无独立线类型；本模块字段即线协议校验面。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// 下行（server → machine）
// ---------------------------------------------------------------------------

/// `machine/spawn`：启动 ACP 进程（§4.5）。
///
/// 按 `session_id` 幂等（已存在返回现有句柄，不二次起进程）；`env` 受 server
/// 白名单约束（§9.6，proto 只承载形态，双端校验在 server/machine）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineSpawn {
    pub command_id: String,
    pub session_id: String,
    /// ACP 启动命令（argv）。
    pub cmd: Vec<String>,
    pub cwd: String,
    /// 附加环境变量；键名受 §9.6 env 白名单约束。
    pub env: Option<HashMap<String, String>>,
}

/// `machine/kill`：停止 ACP 进程（§4.5）。
///
/// 幂等——已死成功返回；`grace` 为优雅关闭宽限（ms）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineKill {
    pub command_id: String,
    pub session_id: String,
    /// 优雅关闭宽限（ms），缺省由 machine 决定。
    pub grace: Option<u64>,
}

/// `machine/forward`：下行 ACP JSON-RPC 指令透传（§4.5 透传语义——
/// machine 保持 dumb，不解析指令内容，只写 ACP stdin）。
///
/// 【冲突 1 裁决】M1 machine 帧集（§4.8）原缺下行转发帧（f6-machine.md 记录
/// 的未决冲突）；集成测试（F7）确认 server 下行 JSON-RPC 无传输载体。裁决：
/// 新增本帧进 M1 帧集（与 spawn/kill 同族，`command_id` 幂等），machine 写
/// stdin 成功（字节级）后回 [`MachineForwardAck`]（L1+L2 合并确认，§4.4）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineForward {
    pub command_id: String,
    pub session_id: String,
    /// JSON-RPC 指令（initialize / session/new / prompt / cancel /
    /// permission_resolve；server 生成，带 `id`=rpcId）。
    pub frame: serde_json::Value,
}

// ---------------------------------------------------------------------------
// 上行（machine → server）
// ---------------------------------------------------------------------------

/// `machine/hello`：注册 + 重连握手（§4.5）。
///
/// 幂等替换语义：新 hello 到达即 fencing 旧连接（旧连接事件丢弃、关闭——
/// server 行为）。`nonce` 为一次性 challenge（32B CSPRNG，base64，§9.2），
/// 供 server 身份证明（`auth_response`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineHello {
    pub token: String,
    pub hostname: String,
    /// 【决策】文档未展开 caps 结构，M1 不透明透传。
    pub caps: serde_json::Value,
    /// 断线缓冲待补推。
    pub buffered: Option<bool>,
    /// daemon 崩溃缓冲丢失（§7.5）。
    pub buffer_lost: Option<bool>,
    /// per-session 流纪元映射（§4.5.1）。
    pub stream_epochs: Option<HashMap<String, u64>>,
    /// challenge_nonce（32B CSPRNG，base64）。
    pub nonce: String,
}

/// `machine/heartbeat`：周期心跳（默认 5s，§4.5）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineHeartbeat {
    /// 【决策】load 语义文档未展开，M1 取 0–100 整数百分比。
    pub load: u32,
    /// machine 侧当前存活的 session_id 列表。
    pub alive_sessions: Vec<String>,
}

/// `machine/event`：原始 ACP 帧转发（§4.5）。
///
/// `frame` 为原始 ACP 帧（`{type,payload}` 或 JSON-RPC `session/update`，
/// §6.1），machine 保持 dumb 透传；`epoch` 与 server 记录不一致的帧直接
/// 丢弃（server 行为，§4.5.1）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineEvent {
    pub session_id: String,
    /// 流纪元（§4.5.1）。
    pub epoch: u64,
    /// machine 侧单调序号（每 session 独立）。
    pub seq: u64,
    /// 原始 ACP 帧（不透明 JSON）。
    pub frame: serde_json::Value,
}

/// `machine/buffer_sync`：断线缓冲补推（§4.5）。
///
/// 补推起点 `from_seq` = server 持久化 `last_seq + 1`；`epoch` 由 server
/// 回传校验，与记录不一致即拒绝该批（防旧纪元缓冲混入新纪元流）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineBufferSync {
    pub session_id: String,
    pub epoch: u64,
    pub from_seq: u64,
    /// 每帧带 seq。
    pub frames: Vec<BufferedFrame>,
}

/// `machine/buffer_sync.frames` 单帧条目。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BufferedFrame {
    pub seq: u64,
    /// 原始 ACP 帧（不透明 JSON）。
    pub frame: serde_json::Value,
}

/// `machine/spawn_ack`：spawn 结果（§4.5）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineSpawnAck {
    pub command_id: String,
    pub session_id: String,
    pub ok: bool,
    /// 脱敏失败原因（失败时携带）。
    pub error: Option<String>,
}

/// `machine/kill_ack`：kill 结果（§4.5）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineKillAck {
    pub command_id: String,
    pub session_id: String,
    pub ok: bool,
}

/// `machine/forward_ack`：下行转发结果（§4.4 L1+L2 合并确认）。
///
/// `ok=true` 表示指令已完整写入 ACP 进程 stdin（字节级，§4.4 L2）；
/// `ok=false` 携带脱敏原因（进程已退出/管道关闭等），server 侧映射为
/// retryable 失败（AGENT_UNAVAILABLE）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineForwardAck {
    pub command_id: String,
    pub session_id: String,
    pub ok: bool,
    /// 脱敏失败原因（失败时携带）。
    pub error: Option<String>,
}

/// `machine/process_exit`：ACP 进程退出事件（§4.5）。
///
/// `crashed`/`ended` 状态由此驱动（server 行为）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineProcessExit {
    pub session_id: String,
    /// 进程退出码。
    pub code: i32,
}
