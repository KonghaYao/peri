//! Session 级 `on_bg_complete` 构造器（MCP 适配 v4 wave 3 / AW3-11 第二成员）。
//!
//! 背景：`BashTool` 的 bg 完成通知在生产链上是 **per-turn** executor 闭包
//! （`session/exec/executor/agent_build.rs`）。当 `Bash` 作为 `workspace` builtin
//! 实例被装配时，闭包必须能随实例一起送达，因此需要一个**不依赖 turn** 的
//! session 级构造入口（本模块）。
//!
//! 语义依据（主计划 §2.1）：[`AsyncRouter::route_bg_result`] 的全部动作只依赖
//! session inbox —— 构造 `background_result_reminder` →
//! `InboxHandle::push_system_reminder_with_delivery_id` →
//! 同一 mailbox 内入队 + wake；**不看 agent / turn**。`BgTaskKind::Shell` 的
//! continuation 请求在宿主侧本就因 kind 门槛被丢弃
//! （`peri-acp/src/host/continuation.rs`），因此本 helper 产出的闭包对 Shell 与
//! per-turn 闭包**逐位等价**。
//!
//! 时序约束：装配点（会话环境装配）早于 session 注册，彼时
//! [`SessionAccessPort::session_inbox`] 恒为 `None`。因此 inbox **必须在调用期**
//! lazy resolve —— 构造期解析会让回调永久失效。

use std::sync::Arc;

use peri_acp_types::session::SessionAccessPort;
use peri_acp_types::tasks::BgTaskKind;

use crate::agent::events::BackgroundTaskResult;
use crate::session::async_router::AsyncRouter;
use crate::session::factory::OnBgCompleteFn;

/// 由 `(SessionAccessPort, session_id)` 构造 **session 级** `on_bg_complete` 回调。
///
/// 返回的闭包在每次被调用时：
/// 1. 用 `session_id` lazy resolve 会话 inbox；
/// 2. 命中则经 [`AsyncRouter::route_bg_result`] 把 bg 结果作为 `Defer` 投递并唤醒
///    idle 中的会话循环；
/// 3. 未命中（session 尚未注册 / 已销毁）返回 Err，由 owner 保留结果等待重试。
///
/// 除 inbox 外不读取任何会话状态，也不发起 continuation（依据见模块文档）。
pub fn session_bg_complete_callback(
    session_access: Arc<dyn SessionAccessPort>,
    session_id: String,
) -> OnBgCompleteFn {
    Arc::new(move |result: &BackgroundTaskResult, kind: BgTaskKind| {
        match session_access.session_inbox(&session_id) {
            Some(inbox) => {
                AsyncRouter::new(inbox.handle()).route_bg_result(result, kind);
                Ok(())
            }
            None => Err(format!(
                "completion inbox unavailable for session {session_id}"
            )),
        }
    })
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "bg_complete_test.rs"]
mod tests;
