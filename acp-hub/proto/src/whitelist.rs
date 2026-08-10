//! 帧 tag 注册表与 M1 帧集白名单（§4.8）。
//!
//! 维护两件事：
//!
//! 1. **全量帧 tag 注册表**（[`FRAME_TAGS`]，含 M2/M3 保留帧）——
//!    [`Frame::parse`](crate::frame::Frame::parse) 用它区分「未知 tag」与
//!    「已知 tag 反序列化失败」；
//! 2. **M1 白名单 + 方向约束**（[`m1_check`]/[`m1_allows`]）——未列入白名单的
//!    `t` 一律返回稳定错误（`UNSUPPORTED_FRAME`）并计数，不静默（§4.8）。
//!
//! 检查失败映射约定（§9.1）：未知 `t` / 已知非 M1 →
//! [`ProtoError::Unsupported`](crate::frame::ProtoError::Unsupported)；方向违反 →
//! [`ProtoError::DirectionRejected`](crate::frame::ProtoError::DirectionRejected)。
//! server 侧两者均以 `UNSUPPORTED_FRAME` 回 `action_error`（若可回）或断开，
//! 并计数。

use crate::frame::FrameTag;

/// 全量帧 tag 注册表（§3.2 完整面，含 M2/M3 保留帧）。
///
/// `Frame::parse` 依赖：tag 出现在此表 = 已知帧，反序列化失败归类为
/// `Malformed`；不在此表 = 未知帧，归类为 `Unsupported`。
pub static FRAME_TAGS: &[FrameTag] = &[
    FrameTag("action"),
    FrameTag("action_ack"),
    FrameTag("action_error"),
    FrameTag("event"),
    FrameTag("keep_alive"),
    FrameTag("pong"),
    FrameTag("ready"),
    FrameTag("auth"),
    FrameTag("auth_response"),
    FrameTag("ysync.subscribe"),
    FrameTag("ysync.unsubscribe"),
    FrameTag("ysync.update"),
    FrameTag("ysync.sync"),
    FrameTag("ysync.awareness"),
    FrameTag("instance/hello"),
    FrameTag("instance/heartbeat"),
    FrameTag("instance/event"),
    FrameTag("instance/buffer_sync"),
    FrameTag("instance/spawn"),
    FrameTag("instance/kill"),
    FrameTag("instance/forward"),
    FrameTag("instance/spawn_ack"),
    FrameTag("instance/kill_ack"),
    FrameTag("instance/forward_ack"),
    FrameTag("instance/process_exit"),
    FrameTag("session_list"),
];

/// 连接侧角色（由 token 解析，§9.5：token 即身份）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// client（TUI/Web）连接。
    Client,
    /// instance 连接。
    Instance,
}

/// 相对 server 的方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// 对端 → server（client/instance 上行）。
    Inbound,
    /// server → 对端（server 下行）。
    Outbound,
}

/// M1 白名单检查结果（§9.1 映射约定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum M1Check {
    /// 允许。
    Allowed,
    /// 未知 `t` 或已知但不在 M1 帧集（如 `event`/`ysync.sync`/`ysync.awareness`）。
    NotInM1,
    /// 帧在 M1 帧集但方向约束违反（如 C→S 的 `ysync.update`，§5.6）。
    DirectionRejected,
}

/// M1 允许的 action `type` 子集（§4.8 收窄）。
///
/// `events/subscribe`/`events/unsubscribe`（M3）类型保留定义，白名单外。
/// `chat/load`（原 M2 预留）于 §8.5 启用：当前对话内切换 ACP 会话
/// （会话是进程内实体——load 不新建进程，无两阶段创建语义）。
pub const M1_ACTION_TYPES: &[&str] = &[
    "chat/create",
    "chat/load",
    "chat/prompt",
    "chat/cancel",
    "chat/close",
    "permission/resolve",
    "workspace/create",
    "workspace/remove",
    "session/list",
];

/// `action` 帧的 `type` 子集是否在 M1 白名单内。
pub fn m1_allows_action_type(action_type: &str) -> bool {
    M1_ACTION_TYPES.contains(&action_type)
}

/// 布尔便捷形式：是否允许该 (tag, role, dir) 组合（M1 帧集 + 方向约束）。
pub fn m1_allows(tag: FrameTag, role: Role, dir: Direction) -> bool {
    m1_check(tag, role, dir) == M1Check::Allowed
}

/// M1 白名单 + 方向约束检查（§4.8 / §9.1）。
///
/// 先判「是否 M1 帧集成员」（与角色/方向无关的全集），再判该 (role, dir)
/// 组合是否允许；区分 [`M1Check::NotInM1`] 与 [`M1Check::DirectionRejected`]，
/// 供上层映射为 [`ProtoError::Unsupported`](crate::frame::ProtoError::Unsupported)
/// 或 [`ProtoError::DirectionRejected`](crate::frame::ProtoError::DirectionRejected)。
pub fn m1_check(tag: FrameTag, role: Role, dir: Direction) -> M1Check {
    // M1 帧集全集（§9.2 收窄；含 auth_response——见设计文档 §9.2 注：
    // §4.8 instance 帧表未列 auth_response，但 §9.2 步骤 2 要求 server 以 HMAC
    // 响应证明身份，按 §9.2 处理）。
    let in_m1_frame_set = matches!(
        tag.0,
        "action"
            | "action_ack"
            | "action_error"
            | "ysync.subscribe"
            | "ysync.unsubscribe"
            | "ysync.update"
            | "ready"
            | "keep_alive"
            | "pong"
            | "auth"
            | "auth_response"
            | "instance/hello"
            | "instance/heartbeat"
            | "instance/event"
            | "instance/buffer_sync"
            | "instance/spawn"
            | "instance/kill"
            | "instance/forward"
            | "instance/spawn_ack"
            | "instance/kill_ack"
            | "instance/forward_ack"
            | "instance/process_exit"
    );
    if !in_m1_frame_set {
        return M1Check::NotInM1;
    }

    let allowed = match role {
        Role::Client => match dir {
            Direction::Inbound => matches!(
                tag.0,
                "action" | "ysync.subscribe" | "ysync.unsubscribe" | "pong" | "auth"
            ),
            Direction::Outbound => matches!(
                tag.0,
                "action_ack" | "action_error" | "ysync.update" | "ready" | "keep_alive"
            ),
        },
        Role::Instance => match dir {
            Direction::Inbound => matches!(
                tag.0,
                "instance/hello"
                    | "instance/heartbeat"
                    | "instance/event"
                    | "instance/buffer_sync"
                    | "instance/spawn_ack"
                    | "instance/kill_ack"
                    | "instance/forward_ack"
                    | "instance/process_exit"
            ),
            Direction::Outbound => matches!(
                tag.0,
                "instance/spawn" | "instance/kill" | "instance/forward" | "auth_response"
            ),
        },
    };

    if allowed {
        M1Check::Allowed
    } else {
        M1Check::DirectionRejected
    }
}

#[cfg(test)]
#[path = "whitelist_test.rs"]
mod whitelist_test;
