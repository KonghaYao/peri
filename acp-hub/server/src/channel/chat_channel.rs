//! ChatChannel：客户端连接归一化（架构 §4.6，设计稿 `f5-channel-control.md` §6）。
//!
//! 每个 client 连接一个 `ChatChannel`，承载连接级协议状态：
//!
//! - **首帧纪律**：认证后首帧必须是 `ysync.subscribe`/`action`（其余 →
//!   [`DispatchOutcome::Disconnect`]，§6 分派规则）；
//! - **relayReady 前 Action 缓冲**（§4.6：不处理；上限 = 命令队列上限 64，
//!   超限按 `RATE_LIMITED` 回 error 不排队），`ready` 后 flush；
//! - 订阅状态维护（`ysync.subscribe`/`unsubscribe`，§4.2）；
//! - 帧分派：I/O 经注入的依赖句柄（[`ChannelDeps`]）；M1 白名单检查（proto
//!   `m1_check`，§4.8）在 gateway 先行，本方法只处理已放行的帧。

use std::collections::{HashSet, VecDeque};

use tokio::sync::mpsc;

use acp_hub_proto::ack::{AckStatus, ActionAck, ActionError, ErrorCode};
use acp_hub_proto::action::ActionEnvelope;
use acp_hub_proto::conn::DocId;
use acp_hub_proto::frame::Frame;
use acp_hub_proto::whitelist::m1_allows_action_type;

use crate::auth::ConnectionCtx;
use crate::channel::{Broadcaster, OutboundMsg};
use crate::channel::{CommandCoordinator, SubmitAck};
use crate::channel::ConnectionRegistry;
use crate::control::InstanceRegistry;
use crate::control::ChatRegistry;

/// ready 前 Action 缓冲上限（§4.6：上限 = 命令队列上限 64，§7.4 规则 1）。
pub const PENDING_ACTION_LIMIT: usize = 64;

/// 分派依赖（channel 层内部接口，hub 装配注入）。
#[derive(Clone)]
pub struct ChannelDeps {
    /// 命令协调器。
    pub coordinator: std::sync::Arc<CommandCoordinator>,
    /// 状态广播器。
    pub broadcast: std::sync::Arc<Broadcaster>,
    /// instance 注册表。
    pub instance: std::sync::Arc<InstanceRegistry>,
    /// chat 注册表。
    pub chats: std::sync::Arc<ChatRegistry>,
    /// 连接注册表。
    pub conns: std::sync::Arc<ConnectionRegistry>,
}

/// 分派结果（gateway 执行副作用；单帧同步方法，I/O 经注入句柄）。
#[derive(Debug)]
pub enum DispatchOutcome {
    /// 需发送的出站消息（action_ack/action_error；gateway 经连接队列下发）。
    Send(Vec<OutboundMsg>),
    /// 订阅/首订阅（gateway 打开 doc + 推快照 + broadcaster 接线；
    /// `first=true` 时推 ready + `mark_ready` + flush 缓冲）。
    Subscribe {
        /// 订阅的 doc 集。
        docs: Vec<DocId>,
        /// 是否连接首个订阅（§4.6 步骤 4：ready 握手时机）。
        first: bool,
    },
    /// 退订（gateway 经 broadcaster 退订）。
    Unsubscribe {
        /// 退订 doc 集。
        docs: Vec<DocId>,
    },
    /// 连接级关闭（协议违规/首帧纪律；携带关闭码）。
    Disconnect(u16),
    /// 无动作。
    None,
}

/// 客户端连接的会话归一化层（§4.6 步骤 4 + 帧分派）。
#[derive(Debug)]
pub struct ChatChannel {
    /// 连接身份上下文。
    pub ctx: ConnectionCtx,
    /// relayReady 标志（§4.6：ready 前 Action 缓冲不处理）。
    relay_ready: bool,
    /// 认证后首帧纪律（§6 分派规则）。
    first_frame: bool,
    /// ready 前有界缓冲（≤64，§4.6）。
    pending: VecDeque<ActionEnvelope>,
    /// ysync.subscribe 状态（§4.2）。
    subscriptions: HashSet<DocId>,
}

impl ChatChannel {
    /// 新建（认证后调用）。
    pub fn new(ctx: ConnectionCtx) -> Self {
        ChatChannel {
            ctx,
            relay_ready: false,
            first_frame: true,
            pending: VecDeque::new(),
            subscriptions: HashSet::new(),
        }
    }

    /// 入帧分派（§6 分派规则；gateway 已做 M1 白名单与方向检查）。
    ///
    /// `tx` 为客户端连接发送队列（coordinator 执行器终态回投）。
    pub async fn dispatch(
        &mut self,
        frame: Frame,
        deps: &ChannelDeps,
        tx: mpsc::Sender<OutboundMsg>,
    ) -> DispatchOutcome {
        // 首帧纪律（§6）：认证后首帧必须是 ysync.subscribe/action。
        if self.first_frame {
            self.first_frame = false;
            let ok = matches!(
                frame,
                Frame::Action(_) | Frame::YsyncSubscribe(_)
            );
            if !ok {
                return DispatchOutcome::Disconnect(1011);
            }
        }
        match frame {
            Frame::Action(action) => self.dispatch_action(action, deps, tx).await,
            Frame::YsyncSubscribe(sub) => {
                let docs = sub.docs.clone();
                let first = self.subscriptions.is_empty();
                for d in &docs {
                    self.subscriptions.insert(d.clone());
                }
                DispatchOutcome::Subscribe { docs, first }
            }
            Frame::YsyncUnsubscribe(sub) => {
                let docs = sub.docs.clone();
                for d in &docs {
                    self.subscriptions.remove(d);
                }
                DispatchOutcome::Unsubscribe { docs }
            }
            // 上行 ysync.update：方向拒绝（§5.6 server 是唯一写入者；客户端
            // 无写租约）→ UNSUPPORTED_FRAME（§4.8 不静默）。
            Frame::YsyncUpdate(_) => {
                DispatchOutcome::Send(vec![OutboundMsg::Frame(unsupported_frame(
                    "ysync.update is server-to-client only",
                ))])
            }
            // 其余 S→C 帧上行（ready/keep_alive/action_ack/action_error 等）
            // 协议违规 → UNSUPPORTED_FRAME。
            other => {
                tracing::warn!(tag = %other.tag(), "unexpected client inbound frame");
                DispatchOutcome::Send(vec![OutboundMsg::Frame(unsupported_frame(
                    "frame not allowed on this connection",
                ))])
            }
        }
    }

    /// ready 前 Action 缓冲（§4.6：不处理；超限 RATE_LIMITED 不排队）。
    async fn dispatch_action(
        &mut self,
        action: ActionEnvelope,
        deps: &ChannelDeps,
        tx: mpsc::Sender<OutboundMsg>,
    ) -> DispatchOutcome {
        // M1 action type 收窄（§4.8：`session/load`（M2）、`events/*`（M3）
        // 类型保留但白名单外 → UNSUPPORTED_FRAME，不静默）。先于 read-only
        // 与 chat 解析——Load 的 chat 可能不存在，语义仍是方法不支持
        // （防御路径原在 coordinator::exec_command，但 submit 的 chat
        // 检查会提前返回 ChatNotFound，必须在此层拦截）。
        if !m1_allows_action_type(action.type_str()) {
            let command_id = extract_command_id(&action).unwrap_or_default();
            return DispatchOutcome::Send(vec![OutboundMsg::Frame(Frame::ActionError(
                ActionError {
                    command_id,
                    code: ErrorCode::UnsupportedFrame,
                    message: format!("unsupported action type: {}", action.type_str()),
                    retryable: false,
                    retry_after_ms: None,
                },
            ))]);
        }
        if !self.ctx.can_send_action() {
            // read-only 档位（§9.2.2：M1 即强制，仅读）。
            return DispatchOutcome::Send(vec![OutboundMsg::Frame(unsupported_frame(
                "read-only token cannot send actions",
            ))]);
        }
        if !self.relay_ready {
            if self.pending.len() >= PENDING_ACTION_LIMIT {
                let command_id = extract_command_id(&action).unwrap_or_default();
                return DispatchOutcome::Send(vec![OutboundMsg::Frame(Frame::ActionError(
                    ActionError {
                        command_id,
                        code: ErrorCode::RateLimited,
                        message: "action buffer full (relay not ready)".to_string(),
                        retryable: false,
                        retry_after_ms: None,
                    },
                ))]);
            }
            self.pending.push_back(action);
            return DispatchOutcome::None;
        }
        match deps.coordinator.submit(&self.ctx, action, tx).await {
            SubmitAck::Accepted { command_id } => DispatchOutcome::Send(vec![
                OutboundMsg::Frame(Frame::ActionAck(ActionAck {
                    command_id,
                    status: AckStatus::Accepted,
                    turn_id: None,
                    chat_id: None,
                    acp_session_id: None,
                    committed_projection_version: None,
                })),
            ]),
            SubmitAck::Duplicate(ack) => {
                DispatchOutcome::Send(vec![OutboundMsg::Frame(Frame::ActionAck(ack))])
            }
            SubmitAck::Failed(err) => {
                DispatchOutcome::Send(vec![OutboundMsg::Frame(Frame::ActionError(err))])
            }
        }
    }

    /// ready 握手完成（§4.6 步骤 4）：置 relayReady = true，返回待 flush 的
    /// 缓冲 Action。
    pub fn mark_ready(&mut self) -> Vec<ActionEnvelope> {
        self.relay_ready = true;
        self.pending.drain(..).collect()
    }

    /// 是否已 ready（gateway 诊断）。
    pub fn is_ready(&self) -> bool {
        self.relay_ready
    }

    /// 缓冲深度（gateway 诊断）。
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

fn extract_command_id(action: &ActionEnvelope) -> Option<String> {
    match action {
        ActionEnvelope::Create { command_id, .. }
        | ActionEnvelope::Load { command_id, .. }
        | ActionEnvelope::Close { command_id, .. }
        | ActionEnvelope::Prompt { command_id, .. }
        | ActionEnvelope::SessionNew { command_id, .. }
        | ActionEnvelope::Cancel { command_id, .. }
        | ActionEnvelope::ResolvePermission { command_id, .. }
        | ActionEnvelope::SubscribeEvents { command_id, .. }
        | ActionEnvelope::UnsubscribeEvents { command_id, .. }
        | ActionEnvelope::WorkspaceCreate { command_id, .. }
        | ActionEnvelope::WorkspaceRemove { command_id, .. }
        | ActionEnvelope::SessionList { command_id, .. } => Some(command_id.clone()),
    }
}

fn unsupported_frame(message: &str) -> Frame {
    Frame::ActionError(ActionError {
        command_id: String::new(),
        code: ErrorCode::UnsupportedFrame,
        message: message.to_string(),
        retryable: false,
        retry_after_ms: None,
    })
}

#[cfg(test)]
#[path = "chat_channel_test.rs"]
mod chat_channel_test;
