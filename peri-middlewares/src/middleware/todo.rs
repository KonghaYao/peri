use std::sync::Arc;

use async_trait::async_trait;
use peri_agent::{
    error::AgentResult,
    messages::{BaseMessage, MessageContent},
    middleware::r#trait::Middleware,
    session::{MessageKind, MessageSource, QueuedMessage},
    tools::BaseTool,
};
use tokio::sync::{mpsc, Mutex};

use crate::tools::todo::{render_todo_status, TodoItem, TodoState, TodoStatus, TodoWriteTool};

/// TodoMiddleware - 提供 todo_write 工具，与 TypeScript todo_write_tool 对齐；
/// 当 agent 以 `requireCompletion: true` 创建 todo 后停止轮仍未标记完成时，
/// 注入当前 todo 状态 + 设 block_continue 续跑（类似 GoalMiddleware）。
pub struct TodoMiddleware {
    notify_tx: mpsc::Sender<Vec<TodoItem>>,
    /// 共享 todo 状态（工具与 after_agent 同源）
    state: Arc<Mutex<TodoState>>,
}

impl TodoMiddleware {
    pub fn new(notify_tx: mpsc::Sender<Vec<TodoItem>>) -> Self {
        Self {
            notify_tx,
            state: Arc::new(Mutex::new(TodoState::default())),
        }
    }

    /// 渲染 requireCompletion steering 模板（含当前 todo 状态）
    fn render_steering(items: &[TodoItem]) -> String {
        format!(
            "<todo-message>\n\
             [TODO Steering]\n\
             You created the todo list with requireCompletion, but stopped while these items \
             are not yet marked completed:\n\
             {}\n\
             Call TodoWrite to mark every item status=\"completed\" (or set \
             requireCompletion=false to explicitly release the requirement), then give your final answer.\n\
             </todo-message>",
            render_todo_status(items)
        )
    }
}

#[async_trait]
impl Middleware for TodoMiddleware {
    fn collect_tools(&self, _cwd: &str) -> Vec<Box<dyn BaseTool>> {
        vec![Box::new(TodoWriteTool::new(
            self.notify_tx.clone(),
            Arc::clone(&self.state),
        ))]
    }

    fn name(&self) -> &str {
        "TodoMiddleware"
    }

    async fn after_agent(
        &self,
        state: &mut dyn peri_agent::middleware::state::MiddlewareState,
        output: &peri_agent::agent::react::AgentOutput,
    ) -> AgentResult<peri_agent::agent::react::AgentOutput> {
        // 1. 前面已有 block_continue → 不干预，尊重优先级（防御性 guard：
        // 链序中 Todo 在 Hook/Goal 之前，当前实际看不到它们的 block；
        // 若未来链序前移出现会设 block_continue 的中间件，此处生效）
        if output.block_continue.is_some() {
            return Ok(output.clone());
        }

        // 2. 检查 requireCompletion 标记：未开启 / 空列表 / 已全部完成 → 放行
        let snap = self.state.lock().await;
        if !snap.require_completion
            || snap.items.is_empty()
            || snap.items.iter().all(|i| i.status == TodoStatus::Completed)
        {
            return Ok(output.clone());
        }

        // 3. 标记开启且存在未完成项 → 注入当前 todo 状态 + block_continue 续跑
        let pending_count = snap
            .items
            .iter()
            .filter(|i| i.status != TodoStatus::Completed)
            .count();
        let template = Self::render_steering(&snap.items);
        drop(snap); // 模板渲染完成，释放状态锁（注入路径不依赖 todo 状态）
                    // [TRAP] 必须用 Human + <system-reminder> 注入，禁止 BaseMessage::system。
                    // System 消息会被 invoke hoist 到 system prompt 顶部，污染 frozen_system_prompt。
                    // （与 goal_middleware.rs / hooks/middleware.rs 注入路径一致）
        let reminder = format!("<system-reminder>\n{}\n</system-reminder>", template);
        state.v2_queue().push(QueuedMessage::new(
            MessageKind::Defer,
            MessageSource::TodoSteering,
            BaseMessage::human(MessageContent::text(reminder)),
        ));

        tracing::debug!(
            pending = pending_count,
            "TodoMiddleware: requireCompletion 未完成，注入 after_agent steering"
        );

        // 4. 设 block_continue，executor 自动续跑
        let mut output = output.clone();
        output.block_continue = Some("todo_require_completion".to_string());
        Ok(output)
    }
}

#[cfg(test)]
#[path = "todo_test.rs"]
mod tests;
