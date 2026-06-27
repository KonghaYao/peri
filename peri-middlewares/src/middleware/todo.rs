use async_trait::async_trait;
use peri_agent::{middleware::r#trait::Middleware, tools::BaseTool};
use tokio::sync::mpsc;

use crate::tools::todo::{TodoItem, TodoWriteTool};

/// TodoMiddleware - 提供 todo_write 工具，与 TypeScript todo_write_tool 对齐
pub struct TodoMiddleware {
    notify_tx: mpsc::Sender<Vec<TodoItem>>,
}

impl TodoMiddleware {
    pub fn new(notify_tx: mpsc::Sender<Vec<TodoItem>>) -> Self {
        Self { notify_tx }
    }
}

#[async_trait]
impl Middleware for TodoMiddleware {
    fn collect_tools(&self, _cwd: &str) -> Vec<Box<dyn BaseTool>> {
        vec![Box::new(TodoWriteTool::new(self.notify_tx.clone()))]
    }

    fn name(&self) -> &str {
        "TodoMiddleware"
    }
}
