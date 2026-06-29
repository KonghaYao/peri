use std::{sync::Arc, time::Duration};

use parking_lot::Mutex;
use peri_middlewares::cron::CronScheduler;

/// Cron 状态（App 子结构体）
pub struct CronState {
    pub scheduler: Arc<Mutex<CronScheduler>>,
}

impl CronState {
    pub fn new() -> (Self, Arc<Mutex<CronScheduler>>) {
        let scheduler = CronScheduler::new(tokio::sync::mpsc::unbounded_channel().0);
        let scheduler = Arc::new(Mutex::new(scheduler));

        let state = Self {
            scheduler: scheduler.clone(),
        };
        (state, scheduler)
    }

    /// Spawn CronManager tick task
    pub fn spawn_tick_task(scheduler: Arc<Mutex<CronScheduler>>) {
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
