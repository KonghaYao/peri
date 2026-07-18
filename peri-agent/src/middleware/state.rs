//! MiddlewareState trait — middleware 钩子的状态上下文
//!
//! object-safe trait，让 `trait Middleware` 接收 `&mut dyn MiddlewareState`。
//!
//! ## 与 `AgentState` 的关系
//!
//! - `AgentState`（`crate::agent::state::AgentState`）是唯一实现者
//! - middleware_runner 通过此 trait 桥接 v2 stages ↔ middleware 钩子

use std::sync::Arc;

use crate::{
    agent::state::AgentState,
    agent::token::TokenTracker,
    messages::BaseMessage,
    thread::{ThreadId, ThreadStore},
};

/// Middleware 在每次钩子调用中看到的状态上下文。
///
/// object-safe：无 `Clone`/`'static` 约束、无泛型方法（`impl Into<String>` 改为 `String`）。
/// 这让 `trait Middleware` 可以改为非泛型，钩子签名用 `&mut dyn MiddlewareState`。
pub trait MiddlewareState: Send + Sync {
    fn cwd(&self) -> &str;
    #[deprecated(
        since = "0.2.0",
        note = "v1→v2 桥接层 no-op；请使用 StageContext 对应方法"
    )]
    fn set_cwd(&mut self, cwd: String);

    fn messages(&self) -> &[BaseMessage];
    fn add_message(&mut self, message: BaseMessage);
    fn prepend_message(&mut self, message: BaseMessage);
    fn messages_mut(&mut self) -> &mut Vec<BaseMessage>;

    fn current_step(&self) -> usize;
    #[deprecated(
        since = "0.2.0",
        note = "v1→v2 桥接层 no-op；请使用 StageContext 对应方法"
    )]
    fn set_current_step(&mut self, step: usize);

    fn get_context(&self, key: &str) -> Option<&str>;
    fn set_context(&mut self, key: String, value: String);

    fn token_tracker(&self) -> &TokenTracker;
    fn token_tracker_mut(&mut self) -> &mut TokenTracker;

    fn push_recall(&mut self, item: String);
    fn drain_recall(&mut self) -> Vec<String>;

    fn ancestor_len(&self) -> usize;

    #[deprecated(
        since = "0.2.0",
        note = "v1→v2 桥接层 no-op；请使用 StageContext 对应方法"
    )]
    fn store(&self) -> Option<&Arc<dyn ThreadStore>>;

    #[deprecated(
        since = "0.2.0",
        note = "v1→v2 桥接层 no-op；请使用 StageContext 对应方法"
    )]
    fn own_thread_id(&self) -> Option<&ThreadId>;

    /// 返回共享的 v2 MessageQueue 引用（用于 goal steering / stop-hook feedback 等异步注入）
    ///
    /// 实现者必须返回**同一个** session 级实例（不能每次新建）。
    /// middleware push 的消息（Info / Defer）由 Receive / End 阶段统一消费。
    fn v2_queue(&self) -> &crate::session::MessageQueue;
}

/// `AgentState` 唯一实现 `MiddlewareState`。
///
/// 通过显式 `AgentState::method(self, ...)` 调用避免与 `MiddlewareState` 自身方法递归。
/// `String` 参数满足 `AgentState` 的 `impl Into<String>` 约束（`String: Into<String>`）。
impl MiddlewareState for AgentState {
    fn cwd(&self) -> &str {
        AgentState::cwd(self)
    }

    fn set_cwd(&mut self, cwd: String) {
        AgentState::set_cwd(self, cwd);
    }

    fn messages(&self) -> &[BaseMessage] {
        AgentState::messages(self)
    }

    fn add_message(&mut self, message: BaseMessage) {
        AgentState::add_message(self, message);
    }

    fn prepend_message(&mut self, message: BaseMessage) {
        AgentState::prepend_message(self, message);
    }

    fn messages_mut(&mut self) -> &mut Vec<BaseMessage> {
        AgentState::messages_mut(self)
    }

    fn current_step(&self) -> usize {
        AgentState::current_step(self)
    }

    fn set_current_step(&mut self, step: usize) {
        AgentState::set_current_step(self, step);
    }

    fn get_context(&self, key: &str) -> Option<&str> {
        AgentState::get_context(self, key)
    }

    fn set_context(&mut self, key: String, value: String) {
        AgentState::set_context(self, key, value);
    }

    fn token_tracker(&self) -> &TokenTracker {
        AgentState::token_tracker(self)
    }

    fn token_tracker_mut(&mut self) -> &mut TokenTracker {
        AgentState::token_tracker_mut(self)
    }

    fn push_recall(&mut self, item: String) {
        AgentState::push_recall(self, item);
    }

    fn drain_recall(&mut self) -> Vec<String> {
        AgentState::drain_recall(self)
    }

    fn ancestor_len(&self) -> usize {
        AgentState::ancestor_len(self)
    }

    fn store(&self) -> Option<&Arc<dyn ThreadStore>> {
        AgentState::store(self)
    }

    fn own_thread_id(&self) -> Option<&ThreadId> {
        AgentState::own_thread_id(self)
    }

    fn v2_queue(&self) -> &crate::session::MessageQueue {
        AgentState::v2_queue(self)
    }
}
