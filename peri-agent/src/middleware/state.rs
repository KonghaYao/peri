//! MiddlewareState — AgentContext / AgentState 的底层状态适配接口
//!
//! hook 通过 capabilities 中的窄接口访问状态，不接收整个适配器接口。
//!
//! ## 与 `AgentState` 的关系
//!
//! - `AgentState` 提供 legacy/test 适配，`AgentContext` 提供生产 v2 适配
//! - middleware_runner 通过此 trait 桥接 v2 stages ↔ middleware 钩子

use crate::{agent::state::AgentState, messages::BaseMessage};

/// 状态适配器的真实操作集合；不直接作为 hook 参数。
///
/// object-safe：无 `Clone`/`'static` 约束、无泛型方法（`impl Into<String>` 改为 `String`）。
/// 各生命周期的公开能力由 `capabilities` 组合，适配器不持有新的 owner。
pub trait MiddlewareState: Send + Sync {
    fn cwd(&self) -> &str;

    fn messages(&self) -> &[BaseMessage];
    fn add_message(&mut self, message: BaseMessage);
    /// 按稳定 MessageId 替换已有可见消息，保持消息顺序和数量。
    ///
    /// 返回 false 表示 ID 不在当前视图，不插入新消息。生产 v2 在
    /// `before_agent` 链结束后（包括 Err）将替换同步至 transcript；
    /// 该操作用于输入附件转换，不支持 Vec 增删/重排。
    #[must_use]
    fn replace_message(&mut self, message: BaseMessage) -> bool;

    fn current_step(&self) -> usize;

    fn push_recall(&mut self, item: String);
    fn drain_recall(&mut self) -> Vec<String>;

    /// 返回共享的 v2 MessageQueue 引用（用于 goal steering / stop-hook feedback 等异步注入）
    ///
    /// 实现者必须返回**同一个** session 级实例（不能每次新建）。
    /// middleware push 的消息（Info / Defer）由 Receive / End 阶段统一消费。
    fn v2_queue(&self) -> &crate::session::MessageQueue;

    /// 会话级 inbox 句柄（middleware 注入 Defer/Prompt 时应经此 wake `await_wake`）。
    fn inbox_handle(&self) -> Option<&peri_acp_types::session::InboxHandle> {
        None
    }

    /// 写入会话队列；有 inbox 时经 `InboxHandle::push` 唤醒 idle loop。
    fn enqueue_v2_message(&self, msg: crate::session::QueuedMessage) {
        if let Some(inbox) = self.inbox_handle() {
            inbox.push(msg);
        } else {
            self.v2_queue().push(msg);
        }
    }

    /// 返回当前 turn 的本地工具视图（stage_builder 每 turn 构建，含当前链
    /// 全部工具，包括 deferred tools）。
    ///
    /// 默认 `None`（v1 路径 / 测试）；v2 实现（`AgentContext`）返回
    /// `Some(&StageContext.runtime.tools)`。背景：宿主级 `shared_tools`
    /// 生产路径写入点归零后恒为空表（`MIDDLEWARE_TOOL_NAMES` 注释），
    /// `ToolSearchMiddleware` 等消费方必须经此读取本地视图，否则 deferred
    /// tool 索引永不构建（issue 2026-08-15-workflow-deferred-tool-missing）。
    fn local_tools(&self) -> Option<&crate::agent::stages::SharedToolMap> {
        None
    }
}

/// `AgentState` 的 MiddlewareState 适配；自身完整状态 API 不经 hook 暴露。
///
/// 通过显式 `AgentState::method(self, ...)` 调用避免与 `MiddlewareState` 自身方法递归。
/// `String` 参数满足 `AgentState` 的 `impl Into<String>` 约束（`String: Into<String>`）。
impl MiddlewareState for AgentState {
    fn cwd(&self) -> &str {
        AgentState::cwd(self)
    }

    fn messages(&self) -> &[BaseMessage] {
        AgentState::messages(self)
    }

    fn add_message(&mut self, message: BaseMessage) {
        AgentState::add_message(self, message);
    }

    fn replace_message(&mut self, message: BaseMessage) -> bool {
        let Some(existing) = AgentState::messages_mut(self)
            .iter_mut()
            .find(|existing| existing.id() == message.id())
        else {
            return false;
        };
        *existing = message;
        true
    }

    fn current_step(&self) -> usize {
        AgentState::current_step(self)
    }

    fn push_recall(&mut self, item: String) {
        AgentState::push_recall(self, item);
    }

    fn drain_recall(&mut self) -> Vec<String> {
        AgentState::drain_recall(self)
    }

    fn v2_queue(&self) -> &crate::session::MessageQueue {
        AgentState::v2_queue(self)
    }
}
