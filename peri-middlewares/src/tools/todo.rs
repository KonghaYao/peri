use std::sync::Arc;

use async_trait::async_trait;
use peri_agent::tools::BaseTool;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, Mutex};

// ─── TodoStatus ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

// ─── TodoItem ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub content: String,
    #[serde(
        default,
        rename = "activeForm",
        skip_serializing_if = "Option::is_none"
    )]
    pub active_form: Option<String>,
    pub status: TodoStatus,
}

// ─── TodoWriteTool ────────────────────────────────────────────────────────────

const TODO_WRITE_DESCRIPTION: &str = r#"Maintain a todo list for complex multi-step tasks. Call this to create or update your todo list with the complete current state. Each call fully replaces the previous list.

Usage:
- Use this tool when working on complex, multi-step tasks that benefit from tracking progress
- Each call sends the COMPLETE todo list — this is a full replacement, not a partial update
- Include ALL items in every call, not just changed ones
- Mark items as "in_progress" when starting work on them, and "completed" when done
- Keep descriptions concise but specific enough to understand at a glance

When to use:
- Use for tasks with 3+ distinct steps that require tracking
- Use when the user explicitly asks for a plan or task breakdown
- Do NOT use for simple, single-step tasks
- Do NOT use for tasks that can be completed in a single tool call

Status values:
- "pending": Not yet started
- "in_progress": Currently being worked on
- "completed": Finished successfully"#;

/// TodoWrite 工具：全量覆盖 todo 列表，并通过 channel 通知 TUI 侧
pub struct TodoWriteTool {
    todos: Arc<Mutex<Vec<TodoItem>>>,
    notify_tx: Option<mpsc::Sender<Vec<TodoItem>>>,
}

impl TodoWriteTool {
    pub fn new(notify_tx: mpsc::Sender<Vec<TodoItem>>) -> Self {
        Self {
            todos: Arc::new(Mutex::new(Vec::new())),
            notify_tx: Some(notify_tx),
        }
    }

    /// 获取当前 todo 列表的快照
    pub async fn snapshot(&self) -> Vec<TodoItem> {
        self.todos.lock().await.clone()
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

    fn description(&self) -> &str {
        TODO_WRITE_DESCRIPTION
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
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

        // 对比新旧列表，生成变更摘要
        let summary = {
            let old = self.todos.lock().await;
            summarize_changes(&old, &items)
        };

        // 全量覆盖
        {
            let mut guard = self.todos.lock().await;
            *guard = items.clone();
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
