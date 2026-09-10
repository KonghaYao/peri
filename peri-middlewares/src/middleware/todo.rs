use peri_agent::middleware::capabilities as hook_state;
use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::system_reminder::{
    ReminderAudience, ReminderAudiences, ReminderCategory, ReminderDelivery, ReminderSeverity,
    ReminderSource, SystemReminder, TrustedSystemReminderFactory, SYSTEM_REMINDER_VERSION,
};
use peri_agent::{
    error::{AgentError, AgentResult},
    middleware::r#trait::Middleware,
    session::{MessageKind, MessageSource, QueuedMessage},
    tools::BaseTool,
};
use serde_json::json;
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
        state: &mut dyn hook_state::AfterAgentState,
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
        let reminder = TrustedSystemReminderFactory::for_producer()
            .construct(SystemReminder {
                version: SYSTEM_REMINDER_VERSION,
                category: ReminderCategory::Guidance,
                source: ReminderSource("todo".into()),
                kind: "require_completion".into(),
                severity: ReminderSeverity::Warning,
                delivery: ReminderDelivery::Required,
                audiences: ReminderAudiences(vec![
                    ReminderAudience::Model,
                    ReminderAudience::Automation,
                ]),
                body: template,
                summary: Some(format!("{pending_count} 个 Todo 尚未完成")),
                metadata: json!({ "pending_count": pending_count }),
            })
            .map_err(|error| AgentError::MiddlewareError {
                middleware: self.name().to_string(),
                reason: error.to_string(),
            })?;
        state.enqueue_v2_message(QueuedMessage::system_reminder(
            MessageKind::Defer,
            MessageSource::TodoSteering,
            reminder,
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
