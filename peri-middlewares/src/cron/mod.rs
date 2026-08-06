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
// 3.0 批 2 波 1：协议类型归契约层（`peri_acp_types::cron::CronTrigger`）。
pub use peri_acp_types::cron::CronTrigger;

/// 定时任务调度器（纯内存）
pub struct CronScheduler {
    tasks: HashMap<String, CronTask>,
    trigger_tx: mpsc::UnboundedSender<CronTrigger>,
    /// Additional trigger senders (for CronOwner bridge in ACP layer).
    /// Each sender receives a clone of every CronTrigger fired.
    extra_trigger_txs: Vec<mpsc::UnboundedSender<CronTrigger>>,
}

impl CronScheduler {
    pub fn new(trigger_tx: mpsc::UnboundedSender<CronTrigger>) -> Self {
        Self {
            tasks: HashMap::new(),
            trigger_tx,
            extra_trigger_txs: Vec::new(),
        }
    }

    /// Subscribe to cron triggers — returns a receiver that receives every trigger.
    ///
    /// The primary `trigger_tx` (from `new()`) is unaffected — this adds an additional
    /// sender. Used by `CronOwner` in the ACP layer to bridge triggers directly to
    /// the SessionInbox, bypassing TUI polling.
    pub fn subscribe(&mut self) -> mpsc::UnboundedReceiver<CronTrigger> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.extra_trigger_txs.push(tx);
        rx
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
                    let trigger = CronTrigger {
                        task_id: task.id.clone(),
                        prompt: task.prompt.clone(),
                    };
                    // Send to primary trigger_tx (TUI polling path)
                    if self.trigger_tx.send(trigger.clone()).is_err() {
                        warn!(
                            task_id = %task.id,
                            "cron tick: failed to send trigger (primary channel closed)"
                        );
                    }
                    // Send to extra trigger_txs (CronOwner bridge path)
                    self.extra_trigger_txs.retain(|tx| {
                        if tx.send(trigger.clone()).is_err() {
                            warn!(
                                task_id = %task.id,
                                "cron tick: extra trigger sender closed, removing"
                            );
                            false
                        } else {
                            true
                        }
                    });
                    // 计算下次触发时间
                    task.next_fire = Self::calculate_next_fire(&task.expression, now);
                }
            }
        }
    }

    /// 将任务的下次触发时间强制设为过去，使下一次 `tick` 必然触发。
    /// 测试/调试辅助（`sched.tasks` 为私有，跨 crate 测试无法直接操作）。
    #[doc(hidden)]
    pub fn force_next_fire_to_past(&mut self, task_id: &str) -> bool {
        let Some(task) = self.tasks.get_mut(task_id) else {
            return false;
        };
        task.next_fire = Some(Utc::now() - chrono::Duration::seconds(10));
        true
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

// 3.0 批 2 波 2：装配注入端口实现（ACP 侧只持 `Arc<dyn CronSchedulerPort>`）。
//
// 端口实现目标为本地 wrapper（`CronSchedulerPortHandle`），避免对外部
// `Mutex<CronScheduler>` 实现 trait 触发 orphan rule；宿主装配点构造
// `Arc::new(CronSchedulerPortHandle(Arc::new(Mutex::new(CronScheduler::new(tx)))))`
// 后 upcast 注入。装配面宿主（`host/workflow_agent.rs` / `host/stage_builder.rs`）
// 经 `downcast_arc` 还原取 `.0`。
pub struct CronSchedulerPortHandle(pub std::sync::Arc<parking_lot::Mutex<CronScheduler>>);

impl peri_acp_types::cron::CronSchedulerPort for CronSchedulerPortHandle {
    fn subscribe(&self) -> mpsc::UnboundedReceiver<CronTrigger> {
        self.0.lock().subscribe()
    }

    fn list_tasks(&self) -> Vec<peri_acp_types::cron::CronTaskInfo> {
        self.0
            .lock()
            .list_tasks()
            .into_iter()
            .map(|t| peri_acp_types::cron::CronTaskInfo {
                id: t.id.clone(),
                expression: t.expression.clone(),
                prompt: t.prompt.clone(),
                next_fire: t.next_fire,
                enabled: t.enabled,
            })
            .collect()
    }

    fn toggle(&self, id: &str) -> bool {
        self.0.lock().toggle(id)
    }

    fn remove(&self, id: &str) -> bool {
        self.0.lock().remove(id)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
