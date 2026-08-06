//! 执行句柄的 `SessionHandle` 实现（L5：自 peri-acp/src/host/exec/prompt_handle.rs
//! 迁入；泛型化——执行输入与执行体经 runner 注入，ACP 装配点注入
//! `run_session_loop` 包装）。
//!
//! 每轮 `session/prompt` 的执行发起路径：
//! `Controller::run_session(session_id)` → Runtime 查映射 → 本句柄 `run()` →
//! 注入的 runner（执行本体，ACP 装配点为 `run_session_loop`）。
//!
//! 生命周期：句柄 = 本轮执行。调用方（ACP `host/prompt.rs` /
//! `host/stdio/...`）构造本句柄（持有本轮执行输入 + runner）→
//! 注册进 Runtime 映射（注册或替换，不递增 epoch/seq）→
//! `run_session`（返回时执行已完成）→ `take_result` 取结果。
//!
//! 销毁六阶段方法为 no-op（句柄生命周期 = 本轮执行，无 owned tasks 需要
//! 编排）；cancel 走 `SessionState.cancel_token`（执行体消费），不经本句柄。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use peri_acp_types::identity::CancelRequest;
use peri_acp_types::messages::MessageContent;
use peri_acp_types::runtime::{SessionHandle, UnstampedEvent};
use peri_acp_types::session::PromptResult;

/// 执行体类型：`(输入) -> PromptResult` 异步闭包（ACP 装配点注入）。
pub type PromptRunner<Ctx, Turn> =
    Arc<dyn Fn(Ctx, Turn) -> Pin<Box<dyn Future<Output = PromptResult> + Send>> + Send + Sync>;

/// 本轮执行的运行句柄（`SessionHandle` 的泛型实现；执行输入与执行体注入）。
///
/// 泛型参数为执行输入（ACP 侧 `SessionContext` / `TurnInput`）——本模块
/// 不解释其具体类型（§0：Agent 层不引用 ACP 执行上下文），只缓存并转交
/// 注入的 runner。
pub struct PromptHandle<Ctx, Turn> {
    /// 本轮执行输入（run() 消费后置 None）。
    turn: Mutex<Option<(Ctx, Turn)>>,
    /// 执行结果（`run_session` 返回后就绪）。
    result: Mutex<Option<PromptResult>>,
    /// 执行体（ACP 装配点注入 `run_session_loop` 包装）。
    runner: PromptRunner<Ctx, Turn>,
}

impl<Ctx, Turn> PromptHandle<Ctx, Turn> {
    /// 构造本轮执行句柄。
    pub fn new(
        ctx: Ctx,
        turn: Turn,
        runner: impl Fn(Ctx, Turn) -> Pin<Box<dyn Future<Output = PromptResult> + Send>>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        Self {
            turn: Mutex::new(Some((ctx, turn))),
            result: Mutex::new(None),
            runner: Arc::new(runner),
        }
    }

    /// 取执行结果（`run_session` 返回时 `run()` 已完成）。
    ///
    /// 防御性回退：结果缺失（run() 未执行 / panic 传播路径）时返回空失败结果
    /// （`PromptResult::default()`，语义与原 ACP 实现一致），不 panic——调用方
    /// （prompt 后处理）按失败语义处理。
    pub fn take_result(&self) -> PromptResult {
        self.result.lock().take().unwrap_or_default()
    }
}

#[async_trait]
impl<Ctx: Send + Sync + 'static, Turn: Send + Sync + 'static> SessionHandle
    for PromptHandle<Ctx, Turn>
{
    async fn run(&self) -> Result<(), anyhow::Error> {
        let (ctx, turn) = self
            .turn
            .lock()
            .take()
            .ok_or_else(|| anyhow::anyhow!("prompt handle: turn already consumed"))?;
        let result = (self.runner)(ctx, turn).await;
        *self.result.lock() = Some(result);
        Ok(())
    }

    fn cancel(&self, _request: &CancelRequest) {
        // cancel 走 SessionState.cancel_token（执行体消费）；
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
