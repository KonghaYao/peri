//! Hook 按阶段暴露的真实能力；StateView 不携带可变 queue/catalog 句柄。
//!
//! 底层适配器实现 MiddlewareState，hook 只能看到对应的窄接口。
//! 消息替换仅在 before_agent 提供，其余阶段不能修改输入缓存。

use super::state::MiddlewareState;
use crate::{
    agent::stages::SharedToolMap,
    messages::{BaseMessage, MessageId},
    session::{MessageQueue, QueuedMessage},
};

/// 只读消息与 turn 元数据，不暴露队列或工具目录。
///
/// ```compile_fail
/// use peri_agent::middleware::capabilities::StateView;
/// fn cannot_mutate_catalog(state: &dyn StateView) {
///     state.local_tools().unwrap().write().clear();
/// }
/// ```
/// ```compile_fail
/// use peri_agent::middleware::capabilities::StateView;
/// fn cannot_mutate_queue(state: &dyn StateView) {
///     state.v2_queue().drain_all();
/// }
/// ```
pub trait StateView: Send + Sync {
    fn cwd(&self) -> &str;
    fn messages(&self) -> &[BaseMessage];
    fn current_step(&self) -> usize;
}

/// 追加消息；生产适配器同时写入 transcript 与本次 hook 缓存。
pub trait MessageAppend: Send + Sync {
    fn add_message(&mut self, message: BaseMessage);
}

/// 按已有 ID 替换输入消息；before_agent 结束后（包括 Err）统一 reconcile。
pub trait MessageReplace: Send + Sync {
    #[must_use]
    fn replace_message(&mut self, message: BaseMessage) -> bool;
}

/// 显式队列能力；保留原消息类型的唤醒语义。
pub trait QueueState: Send + Sync {
    fn v2_queue(&self) -> &MessageQueue;
    fn inbox_handle(&self) -> Option<&peri_acp_types::session::InboxHandle>;
    fn enqueue_v2_message(&self, msg: QueuedMessage);
}

/// Reason 目录重绑能力；目录访问与刷新产生的 recall 在同一阶段提交。
pub trait CatalogState: Send + Sync {
    fn local_tools(&self) -> Option<&SharedToolMap>;
    fn push_recall(&mut self, item: String);
}

/// 本次 Receive 接纳的用户输入身份；空集合不能回退扫描历史。
pub trait InputBatchState: Send + Sync {
    /// None 仅表示 legacy 适配器没有批次信息。
    fn input_message_ids(&self) -> Option<&[MessageId]>;
}

/// 输入准备：读取本批身份、追加、稳定 ID 替换，以及初始工具目录重绑。
pub trait BeforeAgentState:
    StateView + InputBatchState + MessageAppend + MessageReplace + CatalogState
{
}

/// 工具审批仅观察状态，工具参数修改通过 ToolCall 返回值表达。
///
/// ```compile_fail
/// use peri_agent::{messages::BaseMessage, middleware::capabilities::BeforeToolState};
/// fn cannot_edit_input(state: &mut dyn BeforeToolState, message: BaseMessage) {
///     state.replace_message(message);
/// }
/// ```
/// ```compile_fail
/// use peri_agent::middleware::capabilities::BeforeToolState;
/// fn cannot_change_turn(state: &mut dyn BeforeToolState) {
///     state.set_current_step(99);
/// }
/// ```
pub trait BeforeToolState: StateView {}

/// 工具完成：观察状态并调度队列通知，不修改 transcript。
///
/// ```compile_fail
/// use peri_agent::{messages::BaseMessage, middleware::capabilities::AfterToolState};
/// fn cannot_append_history(state: &mut dyn AfterToolState, message: BaseMessage) {
///     state.add_message(message);
/// }
/// ```
pub trait AfterToolState: StateView + QueueState {}

/// Agent 结束：观察结果并经队列投递 goal/stop/todo 反馈。
///
/// ```compile_fail
/// use peri_agent::{messages::BaseMessage, middleware::capabilities::AfterAgentState};
/// fn cannot_edit_history(state: &mut dyn AfterAgentState, message: BaseMessage) {
///     state.replace_message(message);
/// }
/// ```
pub trait AfterAgentState: StateView + QueueState {}

/// 模型调用前：可追加消息和投递状态通知，不提供输入替换或目录写访问。
///
/// ```compile_fail
/// use peri_agent::{messages::BaseMessage, middleware::capabilities::BeforeModelState};
/// fn cannot_edit_cached_input(state: &mut dyn BeforeModelState, message: BaseMessage) {
///     state.replace_message(message);
/// }
/// ```
pub trait BeforeModelState: StateView + MessageAppend + QueueState {}

impl<T: MiddlewareState + ?Sized> StateView for T {
    fn cwd(&self) -> &str {
        MiddlewareState::cwd(self)
    }
    fn messages(&self) -> &[BaseMessage] {
        MiddlewareState::messages(self)
    }
    fn current_step(&self) -> usize {
        MiddlewareState::current_step(self)
    }
}

impl<T: MiddlewareState + ?Sized> MessageAppend for T {
    fn add_message(&mut self, message: BaseMessage) {
        MiddlewareState::add_message(self, message)
    }
}

impl<T: MiddlewareState + ?Sized> MessageReplace for T {
    fn replace_message(&mut self, message: BaseMessage) -> bool {
        MiddlewareState::replace_message(self, message)
    }
}

impl<T: MiddlewareState + ?Sized> QueueState for T {
    fn v2_queue(&self) -> &MessageQueue {
        MiddlewareState::v2_queue(self)
    }
    fn inbox_handle(&self) -> Option<&peri_acp_types::session::InboxHandle> {
        MiddlewareState::inbox_handle(self)
    }
    fn enqueue_v2_message(&self, msg: QueuedMessage) {
        MiddlewareState::enqueue_v2_message(self, msg)
    }
}

impl<T: MiddlewareState + ?Sized> CatalogState for T {
    fn local_tools(&self) -> Option<&SharedToolMap> {
        MiddlewareState::local_tools(self)
    }
    fn push_recall(&mut self, item: String) {
        MiddlewareState::push_recall(self, item)
    }
}

impl<T: MiddlewareState + ?Sized> InputBatchState for T {
    fn input_message_ids(&self) -> Option<&[MessageId]> {
        MiddlewareState::input_message_ids(self)
    }
}

impl<T: StateView + InputBatchState + MessageAppend + MessageReplace + CatalogState + ?Sized>
    BeforeAgentState for T
{
}
impl<T: StateView + ?Sized> BeforeToolState for T {}
impl<T: StateView + QueueState + ?Sized> AfterToolState for T {}
impl<T: StateView + QueueState + ?Sized> AfterAgentState for T {}
impl<T: StateView + MessageAppend + QueueState + ?Sized> BeforeModelState for T {}

#[cfg(test)]
#[path = "capabilities_test.rs"]
mod tests;
