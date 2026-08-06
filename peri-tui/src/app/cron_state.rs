use std::{sync::Arc, time::Duration};

use parking_lot::Mutex;

/// Cron 状态（App 子结构体）。
///
/// 调度器句柄为 peri-middlewares 具体实现——装配面直读（C 类豁免至 M-TUI；
/// 「面板数据全部经 ACP」需 cron/list 命令面，见批 3 tui-deps 未做项）。
pub struct CronState {
    pub scheduler: Arc<Mutex<peri_middlewares::cron::CronScheduler>>,
}

impl CronState {
    pub fn new() -> (Self, Arc<Mutex<peri_middlewares::cron::CronScheduler>>) {
        let scheduler =
            peri_middlewares::cron::CronScheduler::new(tokio::sync::mpsc::unbounded_channel().0);
        let scheduler = Arc::new(Mutex::new(scheduler));

        let state = Self {
            scheduler: scheduler.clone(),
        };
        (state, scheduler)
    }

    /// Spawn CronManager tick task
    pub fn spawn_tick_task(scheduler: Arc<Mutex<peri_middlewares::cron::CronScheduler>>) {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop {
                interval.tick().await;
                scheduler.lock().tick();
            }
        });
    }
}

impl Default for CronState {
    fn default() -> Self {
        let (state, _scheduler) = Self::new();
        state
    }
}
