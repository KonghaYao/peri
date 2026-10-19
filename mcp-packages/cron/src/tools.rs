use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use peri_agent::tools::BaseTool;
use serde_json::Value;

use super::CronScheduler;

/// 注册回执与列表共用的下次触发时间格式（调度与 `next_fire` 均为 UTC）。
fn format_next_fire(next: Option<DateTime<Utc>>) -> String {
    next.map(|time| time.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| "N/A".to_string())
}

pub struct CronRegisterTool {
    scheduler: Arc<Mutex<CronScheduler>>,
}

impl CronRegisterTool {
    pub fn new(scheduler: Arc<Mutex<CronScheduler>>) -> Self {
        Self { scheduler }
    }
}

#[async_trait]
impl BaseTool for CronRegisterTool {
    fn name(&self) -> &str {
        "cron_register"
    }

    fn description(&self) -> &str {
        "Register a scheduled task that will automatically send a user message at the specified cron interval. The task runs in-memory only (lost on restart). Use a standard 5-field cron expression. Cron expressions and next fire times are evaluated in UTC."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "expression": {
                    "type": "string",
                    "description": "Standard 5-field cron expression interpreted in UTC (e.g. '*/5 * * * *' for every 5 minutes)"
                },
                "prompt": {
                    "type": "string",
                    "description": "The user message to send when the task triggers"
                }
            },
            "required": ["expression", "prompt"]
        })
    }

    async fn invoke(
        &self,
        input: Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let expression = input["expression"]
            .as_str()
            .ok_or_else(|| "missing expression field".to_string())?;
        let prompt = input["prompt"]
            .as_str()
            .ok_or_else(|| "missing prompt field".to_string())?
            .trim();
        if prompt.is_empty() {
            return Err("prompt 不能为空".into());
        }

        let mut scheduler = self.scheduler.lock();

        match scheduler.register(expression, prompt) {
            Ok(id) => {
                let next = scheduler.get_task(&id).and_then(|task| task.next_fire);
                Ok(format!(
                    "已注册定时任务 {}（{}），下次触发: {} | prompt: {}",
                    id,
                    expression,
                    format_next_fire(next),
                    prompt
                ))
            }
            Err(e) => Err(e.into()),
        }
    }
}

pub struct CronListTool {
    scheduler: Arc<Mutex<CronScheduler>>,
}

impl CronListTool {
    pub fn new(scheduler: Arc<Mutex<CronScheduler>>) -> Self {
        Self { scheduler }
    }
}

#[async_trait]
impl BaseTool for CronListTool {
    fn name(&self) -> &str {
        "cron_list"
    }

    fn description(&self) -> &str {
        "List all registered cron/scheduled tasks with their status, next fire time (UTC), and prompt. Each line starts with `- ` followed by the full task ID accepted by cron_remove."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {}
        })
    }

    async fn invoke(
        &self,
        _input: Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let scheduler = self.scheduler.lock();

        let tasks = scheduler.list_tasks();
        if tasks.is_empty() {
            return Ok("无定时任务".to_string());
        }

        let mut lines = Vec::new();
        for task in tasks {
            let status = if task.enabled { "启用" } else { "禁用" };
            lines.push(format!(
                "- {} [{}] {} | 下次触发: {} | prompt: {}",
                task.id,
                status,
                task.expression,
                format_next_fire(task.next_fire),
                task.prompt
            ));
        }
        Ok(lines.join("\n"))
    }
}

pub struct CronRemoveTool {
    scheduler: Arc<Mutex<CronScheduler>>,
}

impl CronRemoveTool {
    pub fn new(scheduler: Arc<Mutex<CronScheduler>>) -> Self {
        Self { scheduler }
    }
}

#[async_trait]
impl BaseTool for CronRemoveTool {
    fn name(&self) -> &str {
        "cron_remove"
    }

    fn description(&self) -> &str {
        "Remove/delete a registered cron task by its ID."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "The cron task ID to remove"
                }
            },
            "required": ["id"]
        })
    }

    async fn invoke(
        &self,
        input: Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let id = input["id"]
            .as_str()
            .ok_or_else(|| "missing id field".to_string())?;

        let mut scheduler = self.scheduler.lock();

        if scheduler.remove(id) {
            Ok(format!("Deleted scheduled task {}", id))
        } else {
            Err(format!("Scheduled task {} not found", id).into())
        }
    }
}

/// The same state transition is used by MCP callers and the ACP cron panel.
pub struct CronToggleTool {
    scheduler: Arc<Mutex<CronScheduler>>,
}

impl CronToggleTool {
    pub fn new(scheduler: Arc<Mutex<CronScheduler>>) -> Self {
        Self { scheduler }
    }
}

#[async_trait]
impl BaseTool for CronToggleTool {
    fn name(&self) -> &str {
        "cron_toggle"
    }

    fn description(&self) -> &str {
        "Toggle a registered cron task between enabled and disabled by its ID."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {"id": {"type": "string", "minLength": 1}},
            "required": ["id"]
        })
    }

    async fn invoke(
        &self,
        input: Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let id = input["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or("missing id field")?;
        let mut scheduler = self.scheduler.lock();
        if !scheduler.toggle(id) {
            return Err(format!("Scheduled task {id} not found").into());
        }
        Ok(format!("Toggled scheduled task {id}"))
    }
}

#[cfg(test)]
#[path = "tools_test.rs"]
mod tests;

pub(crate) struct CronAcknowledgeTool {
    scheduler: Arc<Mutex<CronScheduler>>,
}

impl CronAcknowledgeTool {
    pub(crate) fn new(scheduler: Arc<Mutex<CronScheduler>>) -> Self {
        Self { scheduler }
    }
}

#[async_trait]
impl BaseTool for CronAcknowledgeTool {
    fn name(&self) -> &str {
        "cron_acknowledge"
    }

    fn description(&self) -> &str {
        "Acknowledge delivery of one pending scheduled firing. Used by the host continuation consumer."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type":"object",
            "properties": {"task_id":{"type":"string"}, "firing_id":{"type":"string"}},
            "required":["task_id","firing_id"]
        })
    }

    async fn invoke(
        &self,
        input: Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let task_id = input["task_id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or("missing task_id")?;
        let firing_id = input["firing_id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or("missing firing_id")?;
        self.scheduler
            .lock()
            .acknowledge_trigger(task_id, firing_id);
        Ok("Acknowledged".into())
    }
}
