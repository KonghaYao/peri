pub mod middleware;
pub mod tools;

use std::collections::HashMap;
use std::str::FromStr;

use chrono::{DateTime, Utc};
pub use middleware::CronMiddleware;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::mpsc;
pub use tools::{CronListTool, CronRegisterTool, CronRemoveTool};
use tracing::warn;
use uuid::Uuid;

/// 定时任务最大数量限制
pub const MAX_CRON_TASKS: usize = 20;

/// Cron 注册表错误（结构化，取代 String 错误）
///
/// 参考已有 `lsp/tool.rs:LspToolError` 模式：实现 `std::error::Error`，
/// 调用方可通过 `?` 自动转 `Box<dyn Error>` / `anyhow::Error`。
#[derive(Debug, Error)]
pub enum CronError {
    #[error("cron 表达式无效: {0}")]
    InvalidExpression(String),
    #[error("已达到定时任务上限（{0}）")]
    TaskLimitReached(usize),
}

/// 定时任务
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronTask {
    pub id: String,
    pub expression: String,               // 标准 5 段 cron 表达式
    pub prompt: String,                   // 触发时提交的用户输入
    pub next_fire: Option<DateTime<Utc>>, // 下次触发时间（UTC）
    pub enabled: bool,                    // 是否启用
}

/// 触发事件（由 CronScheduler 发送到 App）
#[derive(Debug, Clone)]
pub struct CronTrigger {
    pub task_id: String,
    pub prompt: String,
}

/// 定时任务调度器（纯内存）
pub struct CronScheduler {
    tasks: HashMap<String, CronTask>,
    trigger_tx: mpsc::UnboundedSender<CronTrigger>,
}

impl CronScheduler {
    pub fn new(trigger_tx: mpsc::UnboundedSender<CronTrigger>) -> Self {
        Self {
            tasks: HashMap::new(),
            trigger_tx,
        }
    }

    /// 注册新任务
    pub fn register(&mut self, expression: &str, prompt: &str) -> Result<String, CronError> {
        // 解析 cron 表达式（验证）
        let _cron = croner::Cron::from_str(expression)
            .map_err(|e| CronError::InvalidExpression(e.to_string()))?;

        // 检查上限
        if self.tasks.len() >= MAX_CRON_TASKS {
            return Err(CronError::TaskLimitReached(MAX_CRON_TASKS));
        }

        let id = Uuid::now_v7().to_string();
        let next_fire = Self::calculate_next_fire(expression, Utc::now());

        let task = CronTask {
            id: id.clone(),
            expression: expression.to_string(),
            prompt: prompt.to_string(),
            next_fire,
            enabled: true,
        };

        self.tasks.insert(id.clone(), task);
        Ok(id)
    }

    /// 删除任务
    pub fn remove(&mut self, id: &str) -> bool {
        self.tasks.remove(id).is_some()
    }

    /// 切换 enabled/disabled
    pub fn toggle(&mut self, id: &str) -> bool {
        if let Some(task) = self.tasks.get_mut(id) {
            task.enabled = !task.enabled;
            if task.enabled {
                task.next_fire = Self::calculate_next_fire(&task.expression, Utc::now());
            }
            true
        } else {
            false
        }
    }

    /// 每秒调用：检查是否有任务到时触发
    pub fn tick(&mut self) {
        let now = Utc::now();
        for task in self.tasks.values_mut() {
            if !task.enabled {
                continue;
            }
            if let Some(next) = task.next_fire {
                if now >= next {
                    if self
                        .trigger_tx
                        .send(CronTrigger {
                            task_id: task.id.clone(),
                            prompt: task.prompt.clone(),
                        })
                        .is_err()
                    {
                        warn!(
                            task_id = %task.id,
                            "cron tick: failed to send trigger (channel closed)"
                        );
                    }
                    // 计算下次触发时间
                    task.next_fire = Self::calculate_next_fire(&task.expression, now);
                }
            }
        }
    }

    /// 获取所有任务（按下次触发时间排序，无触发时间的排最后）
    pub fn list_tasks(&self) -> Vec<&CronTask> {
        let mut tasks: Vec<&CronTask> = self.tasks.values().collect();
        tasks.sort_by(|a, b| match (&a.next_fire, &b.next_fire) {
            (Some(a_time), Some(b_time)) => a_time.cmp(b_time),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });
        tasks
    }

    /// 获取单个任务
    pub fn get_task(&self, id: &str) -> Option<&CronTask> {
        self.tasks.get(id)
    }

    /// 计算下次触发时间
    fn calculate_next_fire(expression: &str, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let cron = croner::Cron::from_str(expression).ok()?;
        cron.iter_after(after).next()
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
