//! Ack 与稳定错误码（§4.4）。
//!
//! 协议投影说明：`AckStatus` 是 server command outbox 状态机（`received →
//! … → completed`）的线协议投影，非状态机本体；outbox 完整语义定义在
//! `server/src/persist` + `server/src/channel/command-coordinator`，不暴露给
//! 客户端（§4.4 原文），本模块只承载其线类型。

use serde::{Deserialize, Serialize};

/// 两阶段 Ack 状态（§4.4）。
///
/// - `accepted` = 命令进入有界处理队列；
/// - `committed` = 业务事实已持久化（对应 update 已落盘，见架构 §8.4）；
/// - `duplicate` = 已提交命令重发（§4.4 去重表），返回原 Ack 与 turnId。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AckStatus {
    Accepted,
    Committed,
    Duplicate,
}

/// `action_ack` 帧载荷（§4.4）。每个 action 至多一个最终 Ack。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionAck {
    /// 幂等键（回显原 action 的 commandId）。
    pub command_id: String,
    pub status: AckStatus,
    /// 重发 duplicate 时必带（§4.4：返回原 Ack 与 turnId）。
    pub turn_id: Option<String>,
    /// `session/create` 的 committed 必须携带（server 生成 id 的唯一告知路径）。
    pub session_id: Option<String>,
    /// 字段预留（对齐 chat types.ts，乐观并发校验二期启用）。
    pub committed_projection_version: Option<u32>,
}

/// `action_error` 帧载荷（§4.4）。失败即返回，不静默。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionError {
    pub command_id: String,
    /// 稳定错误码（封闭集合，见 [`ErrorCode`]）。
    pub code: ErrorCode,
    /// 脱敏信息（§9.3：截断前先剔除命令参数/env 值/认证材料）。
    pub message: String,
    /// 是否可安全重试（分类事实源见 [`ErrorCode::default_retryable`]）。
    pub retryable: bool,
    /// 建议重试等待（ms）。
    pub retry_after_ms: Option<u64>,
}

/// 稳定错误码（§4.4 九码 + §4.8 的 `UNSUPPORTED_FRAME`）。
///
/// 封闭集合：文档无 `INTERNAL` 等内部码——内部错误不直接上协议，经脱敏映射
/// 到现有码（§9.3）。`#[non_exhaustive]` 防御性扩展，新增码必须走文档修订。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum ErrorCode {
    /// 认证缺失/失败。
    Unauthenticated,
    /// 无权限（如无权限 session 的订阅 → FORBIDDEN，§4.3.1）。
    Forbidden,
    /// session 不存在。
    SessionNotFound,
    /// 目标 machine 离线。
    MachineOffline,
    /// 乐观并发版本冲突（二期启用）。
    VersionConflict,
    /// 当前状态不允许该操作（如 spawn env 白名单外键 → INVALID_STATE，§9.6）。
    InvalidState,
    /// 限流。
    RateLimited,
    /// ACP 进程不可用（spawn 失败/超时等）。
    AgentUnavailable,
    /// 载荷超上限（端到端 1MB/4KB，§9.3）。
    PayloadTooLarge,
    /// 白名单外 `t` → 稳定错误（§4.8），并计数不静默。
    UnsupportedFrame,
}

impl ErrorCode {
    /// retryable 分类事实源（§4.4）：`AGENT_UNAVAILABLE`/`MACHINE_OFFLINE` →
    /// `true`；`INVALID_STATE`/`FORBIDDEN`/`SESSION_NOT_FOUND` → `false`。
    ///
    /// 供两端对齐（server 裁决 + 客户端提示），不做协议字段默认。
    pub fn default_retryable(self) -> bool {
        match self {
            ErrorCode::AgentUnavailable | ErrorCode::MachineOffline => true,
            ErrorCode::Unauthenticated
            | ErrorCode::Forbidden
            | ErrorCode::SessionNotFound
            | ErrorCode::VersionConflict
            | ErrorCode::InvalidState
            | ErrorCode::RateLimited
            | ErrorCode::PayloadTooLarge
            | ErrorCode::UnsupportedFrame => false,
        }
    }
}
