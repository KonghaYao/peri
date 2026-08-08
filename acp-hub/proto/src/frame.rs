//! 帧模型：`Frame` 枚举（serde tag `"t"`）与解析入口（§4.2）。
//!
//! 每条 WebSocket 文本消息是一个 JSON 对象 `{ "t": <frame_type>, ... }`。
//! `Frame` 采用**双层 internally tagged** 形态：外层 `t` 判别帧类，`Action`
//! 变体 newtype 包裹 tag `"type"` 的 [`ActionEnvelope`]。
//!
//! [`Frame::parse`] 先提取 `t` 查全量注册表（[`FRAME_TAGS`]），
//! 区分「未知 tag」（→ [`ProtoError::Unsupported`]）与「已知 tag 反序列化失败」
//! （→ [`ProtoError::Malformed`]），不 panic、不静默。

use serde::{Deserialize, Serialize};

use crate::ack::{ActionAck, ActionError};
use crate::action::ActionEnvelope;
use crate::conn::{Auth, AuthResponse, KeepAlive, Pong, Ready};
use crate::event::EventFrame;
use crate::machine::{
    MachineBufferSync, MachineEvent, MachineForward, MachineForwardAck, MachineHeartbeat,
    MachineHello, MachineKill, MachineKillAck, MachineProcessExit, MachineSpawn, MachineSpawnAck,
};
use crate::whitelist::FRAME_TAGS;
use crate::ysync::{YsyncAwareness, YsyncSubscribe, YsyncSync, YsyncUnsubscribe, YsyncUpdate};

/// 帧解析与白名单检查的错误面。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProtoError {
    /// JSON 不可解析 / 字段缺失 / 已知 tag 但载荷反序列化失败。
    #[error("malformed frame: {0}")]
    Malformed(String),
    /// `t` 未注册，或已知但不在当前 M1 白名单 → 上层映射为 `UNSUPPORTED_FRAME`（§4.8）。
    #[error("unsupported frame tag: {0}")]
    Unsupported(String),
    /// 帧在白名单内但方向约束违反（如 C→S 的 `ysync.update`，§5.6）。
    #[error("frame rejected by direction: {0}")]
    DirectionRejected(String),
}

/// `"t"` 的静态注册表条目（见 [`FRAME_TAGS`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameTag(pub &'static str);

impl std::fmt::Display for FrameTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// 全量帧枚举（§4.2 完整面 → M1 收窄见 [`crate::whitelist`]）。
///
/// tag 值含 `.`（`ysync.*`）与 `/`（`machine/*`），无法由 `rename_all` 派生，
/// 逐变体显式 `#[serde(rename = ...)]`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum Frame {
    /// C→S 控制命令；`ActionEnvelope` 自身是 tag=`"type"` 的 internally tagged 枚举。
    #[serde(rename = "action")]
    Action(ActionEnvelope),
    /// S→C 两阶段 Ack（§4.4）。
    #[serde(rename = "action_ack")]
    ActionAck(ActionAck),
    /// S→C 稳定错误码（§4.4）。
    #[serde(rename = "action_error")]
    ActionError(ActionError),
    /// S→C `events/subscribe` 推送（M3 启用，类型保留）。
    #[serde(rename = "event")]
    Event(EventFrame),
    /// S→C 心跳（§4.7）。
    #[serde(rename = "keep_alive")]
    KeepAlive(KeepAlive),
    /// C→S keep_alive 回执（§4.7）。
    #[serde(rename = "pong")]
    Pong(Pong),
    /// S→C 快照推送完成握手（§4.6）。
    #[serde(rename = "ready")]
    Ready(Ready),
    /// C→S 连接后第一帧；角色由 token 解析（§4.2）。
    #[serde(rename = "auth")]
    Auth(Auth),
    /// S→M server 身份证明（§9.2 步骤 2；M1 machine 面）。
    #[serde(rename = "auth_response")]
    AuthResponse(AuthResponse),
    /// C→S 订阅 Doc（§4.2）。
    #[serde(rename = "ysync.subscribe")]
    YsyncSubscribe(YsyncSubscribe),
    /// C→S 退订 Doc（§4.2）。
    #[serde(rename = "ysync.unsubscribe")]
    YsyncUnsubscribe(YsyncUnsubscribe),
    /// S→C 单向 update（base64，§5.6；客户端上行一律拒绝）。
    #[serde(rename = "ysync.update")]
    YsyncUpdate(YsyncUpdate),
    /// y-sync Step 1/2（§5.6 不采用双向增量握手，保留定义）。
    #[serde(rename = "ysync.sync")]
    YsyncSync(YsyncSync),
    /// y-protocol awareness（M3 启用，保留定义）。
    #[serde(rename = "ysync.awareness")]
    YsyncAwareness(YsyncAwareness),
    /// M→S 注册 + 重连握手（§4.5）。
    #[serde(rename = "machine/hello")]
    MachineHello(MachineHello),
    /// M→S 周期心跳（§4.5）。
    #[serde(rename = "machine/heartbeat")]
    MachineHeartbeat(MachineHeartbeat),
    /// M→S 原始 ACP 帧转发（带 seq 与流纪元，§4.5.1）。
    #[serde(rename = "machine/event")]
    MachineEvent(MachineEvent),
    /// M→S 断线缓冲补推（§4.5）。
    #[serde(rename = "machine/buffer_sync")]
    MachineBufferSync(MachineBufferSync),
    /// S→M 启动 ACP 进程（按 session_id 幂等，§4.5）。
    #[serde(rename = "machine/spawn")]
    MachineSpawn(MachineSpawn),
    /// S→M 停止 ACP 进程（幂等，§4.5）。
    #[serde(rename = "machine/kill")]
    MachineKill(MachineKill),
    /// S→M 下行 ACP JSON-RPC 透传（L1+L2 确认，§4.5/§4.4）。
    #[serde(rename = "machine/forward")]
    MachineForward(MachineForward),
    /// M→S spawn 结果（§4.5）。
    #[serde(rename = "machine/spawn_ack")]
    MachineSpawnAck(MachineSpawnAck),
    /// M→S kill 结果（§4.5）。
    #[serde(rename = "machine/kill_ack")]
    MachineKillAck(MachineKillAck),
    /// M→S 下行转发结果（L1+L2 合并确认，§4.4）。
    #[serde(rename = "machine/forward_ack")]
    MachineForwardAck(MachineForwardAck),
    /// M→S ACP 进程退出事件（§4.5）。
    #[serde(rename = "machine/process_exit")]
    MachineProcessExit(MachineProcessExit),
}

impl Frame {
    /// 提取 `"t"` 并完整解析单条消息。
    ///
    /// 未知 tag → [`ProtoError::Unsupported`]；已知 tag 但 JSON 畸形/字段缺失 →
    /// [`ProtoError::Malformed`]。不 panic、不静默。
    pub fn parse(raw: &str) -> Result<Frame, ProtoError> {
        let value: serde_json::Value =
            serde_json::from_str(raw).map_err(|e| ProtoError::Malformed(e.to_string()))?;
        let tag = value
            .get("t")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ProtoError::Malformed("missing or non-string \"t\"".to_string()))?;

        // 区分「未知 tag」与「已知但反序列化失败」。
        if !FRAME_TAGS.iter().any(|t| t.0 == tag) {
            return Err(ProtoError::Unsupported(tag.to_string()));
        }
        serde_json::from_value(value).map_err(|e| ProtoError::Malformed(e.to_string()))
    }

    /// 返回本帧的 `"t"` 注册表条目。
    pub fn tag(&self) -> FrameTag {
        match self {
            Frame::Action(_) => FrameTag("action"),
            Frame::ActionAck(_) => FrameTag("action_ack"),
            Frame::ActionError(_) => FrameTag("action_error"),
            Frame::Event(_) => FrameTag("event"),
            Frame::KeepAlive(_) => FrameTag("keep_alive"),
            Frame::Pong(_) => FrameTag("pong"),
            Frame::Ready(_) => FrameTag("ready"),
            Frame::Auth(_) => FrameTag("auth"),
            Frame::AuthResponse(_) => FrameTag("auth_response"),
            Frame::YsyncSubscribe(_) => FrameTag("ysync.subscribe"),
            Frame::YsyncUnsubscribe(_) => FrameTag("ysync.unsubscribe"),
            Frame::YsyncUpdate(_) => FrameTag("ysync.update"),
            Frame::YsyncSync(_) => FrameTag("ysync.sync"),
            Frame::YsyncAwareness(_) => FrameTag("ysync.awareness"),
            Frame::MachineHello(_) => FrameTag("machine/hello"),
            Frame::MachineHeartbeat(_) => FrameTag("machine/heartbeat"),
            Frame::MachineEvent(_) => FrameTag("machine/event"),
            Frame::MachineBufferSync(_) => FrameTag("machine/buffer_sync"),
            Frame::MachineSpawn(_) => FrameTag("machine/spawn"),
            Frame::MachineKill(_) => FrameTag("machine/kill"),
            Frame::MachineForward(_) => FrameTag("machine/forward"),
            Frame::MachineSpawnAck(_) => FrameTag("machine/spawn_ack"),
            Frame::MachineKillAck(_) => FrameTag("machine/kill_ack"),
            Frame::MachineForwardAck(_) => FrameTag("machine/forward_ack"),
            Frame::MachineProcessExit(_) => FrameTag("machine/process_exit"),
        }
    }
}

#[cfg(test)]
#[path = "frame_test.rs"]
mod frame_test;
