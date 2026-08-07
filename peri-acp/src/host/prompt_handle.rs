//! ACP 侧执行薄壳的 `SessionHandle` 实现（3.0 批 2：执行发起经 Controller）。
//!
//! 每轮 `session/prompt` 的执行发起路径：
//! `Controller::run_session(session_id)` → Runtime 查映射 → 本句柄 `run()` →
//! `run_session_loop`（执行本体，L5 时随 executor 拆分迁入 peri-agent）。
//!
//! 生命周期：句柄 = 本轮执行。调用方（`host/prompt.rs` / `host/stdio/...`）
//! 构造本句柄（持有本轮 `SessionContext` + `TurnInput`）→
//! `Controller::register_session`（注册或替换，不递增 epoch/seq）→
//! `Controller::run_session`（返回时执行已完成）→ `take_result` 取结果。
//!
//! 销毁六阶段方法为 no-op（句柄生命周期 = 本轮执行，无 owned tasks 需要
//! 编排）；cancel 走 `SessionState.cancel_token`（`run_session_loop` 消费），
//! 不经本句柄。

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use peri_acp_types::identity::CancelRequest;
use peri_acp_types::messages::MessageContent;
use peri_acp_types::runtime::{SessionHandle, UnstampedEvent};

use crate::session::executor::{PromptResult, SessionContext, TurnInput};

/// 本轮执行的运行句柄（`SessionHandle` 的 ACP 侧实现）。
pub struct PromptHandle {
    /// 本轮执行输入（run() 消费后置 None）。
    turn: Mutex<Option<(SessionContext, TurnInput)>>,
    /// 执行结果（`Controller::run_session` 返回后就绪）。
    result: Mutex<Option<PromptResult>>,
}

impl PromptHandle {
    /// 构造本轮执行句柄。
    pub fn new(ctx: SessionContext, turn: TurnInput) -> Self {
        Self {
            turn: Mutex::new(Some((ctx, turn))),
            result: Mutex::new(None),
        }
    }

    /// 取执行结果（`Controller::run_session` 返回时 `run()` 已完成）。
    ///
    /// 防御性回退：结果缺失（run() 未执行 / panic 传播路径）时返回空失败结果，
    /// 不 panic——调用方（prompt 后处理）按失败语义处理。
    pub fn take_result(&self) -> PromptResult {
        self.result.lock().take().unwrap_or_else(|| PromptResult {
            messages: Vec::new(),
            ok: false,
            stop_reason: crate::session::executor::PromptStopReason::EndTurn,
            history_replaced_by_compaction: false,
            recall_items: Vec::new(),
        })
    }
}

#[async_trait]
impl SessionHandle for PromptHandle {
    async fn run(&self) -> Result<(), anyhow::Error> {
        let (ctx, turn) = self
            .turn
            .lock()
            .take()
            .ok_or_else(|| anyhow::anyhow!("prompt handle: turn already consumed"))?;
        let result = crate::session::executor::run_session_loop(ctx, turn).await;
        *self.result.lock() = Some(result);
        Ok(())
    }

    fn cancel(&self, _request: &CancelRequest) {
        // cancel 走 SessionState.cancel_token（run_session_loop 消费）；
        // 本句柄为透传 no-op（§9 cancel 最终执行权在 Agent 层）。
    }

    fn submit_input(&self, _input: MessageContent) -> Result<(), anyhow::Error> {
        Err(anyhow::anyhow!(
            "prompt handle: submit_input 不支持（每轮执行输入在句柄构造时注入）"
        ))
    }

    fn stop_accepting(&self) {}

    fn cancel_owned(&self) {}

    async fn join(&self, _deadline: std::time::Duration) -> bool {
        // 句柄生命周期 = 本轮执行；run_session 返回即已结束。
        true
    }

    fn abort(&self) {}

    async fn persist(&self) -> Result<(), anyhow::Error> {
        Ok(())
    }

    fn drain(&self) -> Vec<UnstampedEvent> {
        Vec::new()
    }
}

/// 便捷包装：`Arc<PromptHandle>` 注册进 Controller 的类型擦除入口。
pub fn register_prompt_handle(
    controller: &peri_controller::Controller,
    session_id: &str,
    handle: Arc<PromptHandle>,
) {
    controller.register_session(session_id, handle);
}
