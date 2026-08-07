//! Cron 契约（触发事件 + 调度器端口）。
//!
//! `CronTrigger` 自 `peri-middlewares/src/cron/mod.rs` 迁入（3.0 批 2 波 1）；
//! `CronSchedulerPort` 为装配注入端口（波 2）：宿主装配点构造具体
//! `CronScheduler` 后 upcast 注入，ACP 侧只持端口接口。middlewares 的
//! `CronScheduler` 实现该端口（`impl CronSchedulerPort for Mutex<CronScheduler>`）。

use std::any::Any;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::mpsc;

/// 触发事件（由 CronScheduler 发送到 App）
#[derive(Debug, Clone)]
pub struct CronTrigger {
    pub task_id: String,
    pub prompt: String,
}

/// Cron 任务信息（`CronScheduler::list_tasks` 的契约镜像，供 cron/list 命令面
/// 与 TUI 面板经 ACP 拿数据——契约层不引入 middlewares 的 `CronTask`）。
#[derive(Debug, Clone)]
pub struct CronTaskInfo {
    pub id: String,
    pub expression: String,
    /// 触发时提交的用户输入
    pub prompt: String,
    /// 下次触发时间（UTC）
    pub next_fire: Option<DateTime<Utc>>,
    pub enabled: bool,
}

/// Cron 调度器端口（装配注入面，`peri-middlewares::cron::CronScheduler` 实现）。
///
/// ACP 侧只持 `Arc<dyn CronSchedulerPort>`；具体调度器（含注册/移除/时钟）
/// 由宿主装配点构造。订阅语义与 `CronScheduler::subscribe` 一致：
/// 返回的接收端收到每次触发的 clone。
pub trait CronSchedulerPort: Send + Sync {
    /// 订阅 cron 触发事件（每触发一次收到一条 `CronTrigger`）。
    fn subscribe(&self) -> mpsc::UnboundedReceiver<CronTrigger>;

    /// 全部任务快照（cron/list 命令面数据源；TUI 面板经 ACP 拿数据）。
    fn list_tasks(&self) -> Vec<CronTaskInfo>;

    /// 切换任务启用状态（返回是否命中）。
    fn toggle(&self, id: &str) -> bool;

    /// 移除任务（返回是否命中）。
    fn remove(&self, id: &str) -> bool;

    /// 还原具体实现（downcast 还原点，供 middlewares 装配面与装配面宿主使用）。
    fn as_any(&self) -> &dyn Any;
}

impl dyn CronSchedulerPort {
    /// 将 `Arc<dyn CronSchedulerPort>` 还原为具体实现 `Arc<T>`（类型不符返回原 `Arc`）。
    pub fn downcast_arc<T: CronSchedulerPort + 'static>(
        self: Arc<Self>,
    ) -> Result<Arc<T>, Arc<Self>> {
        let ptr = Arc::into_raw(self);
        unsafe {
            if (*ptr).type_id() == std::any::TypeId::of::<T>() {
                Ok(Arc::from_raw(ptr as *const T))
            } else {
                Err(Arc::from_raw(ptr))
            }
        }
    }
}
