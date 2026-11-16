//! 出口任务归属：task id → capability 的调用期注册表。
//!
//! 代理生成的下游 id 带 `<capability>:task:` 前缀；in-process handler 的 id 由
//! 首次 `CreateTaskResult` 注册。两条路径都保证出口内唯一判定。

use std::collections::HashMap;

use parking_lot::Mutex;
use peri_acp_types::builtin_mcp::v5::capability as find_capability;
use rmcp::ErrorData as McpError;

/// 任务 id → 能力归属；出口内 task id 必须唯一判定。
#[derive(Default)]
pub(super) struct TaskRoutes {
    by_id: Mutex<HashMap<String, &'static str>>,
}

impl TaskRoutes {
    pub(super) fn register(&self, capability: &'static str, task_id: &str) -> Result<(), McpError> {
        let mut routes = self.by_id.lock();
        if let Some(existing) = routes.get(task_id) {
            if *existing != capability {
                tracing::error!(
                    capability,
                    existing,
                    task_id,
                    "duplicate builtin task id across capabilities"
                );
                return Err(McpError::internal_error(
                    format!("task id `{task_id}` is already owned by capability `{existing}`"),
                    None,
                ));
            }
            return Ok(());
        }
        routes.insert(task_id.to_owned(), capability);
        Ok(())
    }

    /// 先查调用期注册表，再按 `<capability>:task:` 前缀判定（代理生成的下游 id）。
    pub(super) fn capability_of(&self, task_id: &str) -> Option<&'static str> {
        if let Some(capability) = self.by_id.lock().get(task_id).copied() {
            return Some(capability);
        }
        let (prefix, _) = task_id.split_once(":task:")?;
        let capability = find_capability(prefix)?;
        Some(capability.name)
    }
}
