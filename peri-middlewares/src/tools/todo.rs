use std::sync::Arc;

use async_trait::async_trait;
use peri_agent::tools::BaseTool;
use serde_json::Value;
use tokio::sync::{mpsc, Mutex};

// ─── TodoStatus / TodoItem（L5：契约化至 peri-acp-types::tools，re-export 保兼容）──

pub use peri_acp_types::tools::{TodoItem, TodoStatus};

// ─── TodoState ────────────────────────────────────────────────────────────────

/// Todo 共享状态（TodoWriteTool 写入 + TodoMiddleware after_agent 读取）。
///
/// `require_completion`：agent 创建 todo 时通过 `TodoWrite({ requireCompletion: true })`
/// 构建的标记。开启后 agent 停止轮若仍存在未完成项，TodoMiddleware 会像 goal 一样
/// 注入当前 todo 状态并 block_continue 续跑，直到全部 completed（自动解除）或
/// agent 显式传 `requireCompletion: false`。
#[derive(Debug, Default)]
pub struct TodoState {
    pub items: Vec<TodoItem>,
    pub require_completion: bool,
}

/// 渲染当前 todo 状态文本（注入 steering 用）
pub(crate) fn render_todo_status(items: &[TodoItem]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let status = match item.status {
            TodoStatus::Pending => "pending",
            TodoStatus::InProgress => "in_progress",
            TodoStatus::Completed => "completed",
        };
        lines.push(format!("[{i}] [{status}] {}", item.content));
    }
    lines.join("\n")
}

// ─── TodoWriteTool ────────────────────────────────────────────────────────────

const TODO_WRITE_DESCRIPTION: &str = include_str!("descriptions/todo.md");

/// TodoWrite 工具：全量覆盖 todo 列表，并通过 channel 通知 TUI 侧
pub struct TodoWriteTool {
    /// 共享状态（与 TodoMiddleware after_agent 同源，工具实例重建不丢状态）
    state: Arc<Mutex<TodoState>>,
    notify_tx: Option<mpsc::Sender<Vec<TodoItem>>>,
}

impl TodoWriteTool {
    pub fn new(notify_tx: mpsc::Sender<Vec<TodoItem>>, state: Arc<Mutex<TodoState>>) -> Self {
        Self {
            state,
            notify_tx: Some(notify_tx),
        }
    }

    /// 获取当前 todo 列表的快照
    pub async fn snapshot(&self) -> Vec<TodoItem> {
        self.state.lock().await.items.clone()
    }
}

/// 对比新旧 todo 列表，生成变更摘要（用于 TUI 显示）
fn summarize_changes(old: &[TodoItem], new: &[TodoItem]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let max_len = old.len().max(new.len());

    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut status_changes = Vec::new();

    for i in 0..max_len {
        match (old.get(i), new.get(i)) {
            (None, Some(_)) => added.push(format!("[{i}]")),
            (Some(_), None) => removed.push(format!("[{i}]")),
            (Some(old_item), Some(new_item)) => {
                if old_item.status != new_item.status {
                    let status_str = match &new_item.status {
                        TodoStatus::Pending => "pending",
                        TodoStatus::InProgress => "in_progress",
                        TodoStatus::Completed => "completed",
                    };
                    status_changes.push(format!("[{i}]→{status_str}"));
                }
            }
            (None, None) => {}
        }
    }

    if !added.is_empty() {
        parts.push(format!("+{}", added.join(",")));
    }
    if !removed.is_empty() {
        parts.push(format!("-{}", removed.join(",")));
    }
    if !status_changes.is_empty() {
        parts.push(status_changes.join(","));
    }

    if parts.is_empty() {
        "saved".to_string()
    } else {
        parts.join(" ")
    }
}

#[async_trait]
impl BaseTool for TodoWriteTool {
    fn name(&self) -> &str {
        "TodoWrite"
    }

    fn is_direct(&self) -> bool {
        true
    }

    /// 提示词层声明分组（design v2 §2.5.1）：交互类工具归入 `interaction`。
    fn namespace(&self) -> Option<&str> {
        Some("interaction")
    }

    /// 提示词层声明模板（design v2 §2.5.3）：多步任务跟踪 + 3 步以上使用纪律。
    ///
    /// title 不覆盖——走 `BaseTool::tool_description` 默认路径由 name 推导。
    /// 05_using_tools.md 手写条目在渐进迁移完成前保留（守护测试防逐字重复）。
    fn prompt_declaration(&self) -> Option<String> {
        Some(
            "Maintain a visible task list → `{{name}}` ({{title}}) to track multi-step progress and cut context sprawl. Update it for any task with 3 or more distinct steps."
                .to_string(),
        )
    }

    fn description(&self) -> &str {
        TODO_WRITE_DESCRIPTION
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "requireCompletion": {
                    "type": "boolean",
                    "description": "Require every item to be marked status=\"completed\" before you end the turn. When set, if you stop with unfinished items, the system injects the current todo state and asks you to mark them completed. Omit it when updating the list to keep the previous setting; set to false (or mark all items completed) to release"
                },
                "todos": {
                    "type": "array",
                    "description": "The complete todo list (replaces all previous items). Include ALL items in every call, not just new or changed ones. Items not included will be removed",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": {
                                "type": "string",
                                "description": "A concise description of the task to be done (1-2 sentences)"
                            },
                            "activeForm": {
                                "type": "string",
                                "description": "Present-tense form of the task description (e.g. 'Running tests'), used for UI spinner display"
                            },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed"],
                                "description": "Current status: 'pending' (not started), 'in_progress' (actively working), 'completed' (done)"
                            }
                        },
                        "required": ["content", "status"]
                    }
                }
            },
            "required": ["todos"]
        })
    }

    async fn invoke(
        &self,
        input: Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let items: Vec<TodoItem> = serde_json::from_value(input["todos"].clone())
            .map_err(|e| format!("TodoWrite: invalid input: {e}"))?;

        // requireCompletion：显式布尔值更新标记；缺省或非布尔（畸形值）保留已有标记
        // （agent 中途更新列表时不带参数，不应丢失创建时的要求；畸形值不静默解除）
        let require_completion = match input.get("requireCompletion") {
            Some(v) => v.as_bool(),
            None => None,
        };

        // 对比新旧列表，生成变更摘要
        let summary = {
            let old = self.state.lock().await;
            summarize_changes(&old.items, &items)
        };

        // 全量覆盖；同步维护 require_completion 标记：
        // - 全部 completed（或清空列表）→ 标记使命完成，自动解除
        // - 显式 true → 开启；显式 false → 解除
        // - 缺省 / 畸形值且未全部完成 → 保留已有标记
        {
            let mut guard = self.state.lock().await;
            guard.items = items.clone();
            let all_done = items.iter().all(|i| i.status == TodoStatus::Completed);
            if all_done {
                guard.require_completion = false;
            } else if let Some(flag) = require_completion {
                guard.require_completion = flag;
            }
        }

        // 通知 TUI；channel 关闭时说明 TUI 已退出，记录 warn 后继续（不影响工具返回值）
        if let Some(tx) = &self.notify_tx {
            if tx.send(items).await.is_err() {
                tracing::warn!("TodoWrite: notify channel closed, TUI may have disconnected");
            }
        }

        Ok(summary)
    }
}

#[cfg(test)]
#[path = "todo_test.rs"]
mod tests;
