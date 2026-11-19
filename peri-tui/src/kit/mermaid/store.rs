//! 进程级预览调度：单后台线程、每图至多一个待执行任务。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::kit::markdown::{MermaidFence, MermaidFenceKey};

use super::frame;
use super::preview::{PreviewEngine, PreviewView};

struct PreviewHub {
    engine: Mutex<PreviewEngine>,
    wake: Condvar,
    revision: AtomicU64,
}

static HUB: OnceLock<Arc<PreviewHub>> = OnceLock::new();
static WORKER_STARTED: AtomicBool = AtomicBool::new(false);

fn hub() -> &'static Arc<PreviewHub> {
    HUB.get_or_init(|| {
        Arc::new(PreviewHub {
            engine: Mutex::new(PreviewEngine::new()),
            wake: Condvar::new(),
            revision: AtomicU64::new(0),
        })
    })
}

fn lock_engine(hub: &PreviewHub) -> std::sync::MutexGuard<'_, PreviewEngine> {
    hub.engine
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 全进程的预览版本。渲染缓存据此只重算含 Mermaid 的可见消息。
#[must_use]
pub(crate) fn revision() -> u64 {
    hub().revision.load(Ordering::Relaxed)
}

/// 观察一个围栏并返回当前可展示的帧。没有启动后台线程时仍会记录状态，
/// 只是不会自动发布新帧（测试与无 TUI 的调用方保持确定性）。
pub(crate) fn observe(
    key: MermaidFenceKey,
    fence: &MermaidFence,
    width: usize,
    frozen: bool,
) -> PreviewView {
    let hub = hub();
    let view = {
        let mut engine = lock_engine(hub);
        engine.observe(key, fence, width, frozen, Instant::now())
    };
    hub.wake.notify_one();
    view
}

/// 后台排版线程是否已启动。无 TUI 的调用方据此走同步历史重建。
#[must_use]
pub(crate) fn worker_running() -> bool {
    WORKER_STARTED.load(Ordering::Relaxed)
}

/// 启动唯一的排版线程。重复调用无副作用。
pub(crate) fn init_preview_worker() {
    if WORKER_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let hub = Arc::clone(hub());
    let spawned = std::thread::Builder::new()
        .name("peri-mermaid-preview".to_string())
        .spawn(move || run_worker(hub));
    if let Err(error) = spawned {
        WORKER_STARTED.store(false, Ordering::SeqCst);
        tracing::warn!(target: "mermaid", %error, "failed to start mermaid preview worker");
    }
}

fn run_worker(hub: Arc<PreviewHub>) {
    let mut engine = lock_engine(&hub);
    loop {
        let now = Instant::now();
        if let Some(job) = engine.take_due(now) {
            drop(engine);
            let result = match job.kind {
                super::preview::JobKind::Preview => {
                    frame::render_source(job.source.as_str(), job.width)
                }
                super::preview::JobKind::Final => {
                    frame::render_final(job.source.as_str(), job.mode, job.width)
                }
            };
            engine = lock_engine(&hub);
            if engine.complete(job, result, Instant::now()) {
                drop(engine);
                publish(&hub);
                engine = lock_engine(&hub);
            }
            continue;
        }
        let wait = engine
            .wait_duration(now)
            .map_or(Duration::from_secs(1), |wait| {
                wait.max(Duration::from_millis(1))
            });
        let (next, _) = hub
            .wake
            .wait_timeout(engine, wait)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        engine = next;
    }
}

/// 发布一次可见变化：递增布局版本并唤醒渲染循环。
fn publish(hub: &PreviewHub) {
    hub.revision.fetch_add(1, Ordering::Relaxed);
    crate::kit::atoms::RENDER_HEARTBEAT
        .set(crate::kit::atoms::RENDER_HEARTBEAT.get().wrapping_add(1));
}

/// 测试专用：同步跑完所有待执行任务，绕过 80ms 节流。
#[cfg(test)]
pub(crate) fn drain_preview_jobs_now() {
    let hub = hub();
    let far_future = Instant::now() + Duration::from_secs(3600);
    loop {
        let job = {
            let mut engine = lock_engine(hub);
            engine.take_due(far_future)
        };
        let Some(job) = job else {
            break;
        };
        let result = match job.kind {
            super::preview::JobKind::Preview => {
                frame::render_source(job.source.as_str(), job.width)
            }
            super::preview::JobKind::Final => {
                frame::render_final(job.source.as_str(), job.mode, job.width)
            }
        };
        let mut engine = lock_engine(hub);
        if engine.complete(job, result, Instant::now()) {
            drop(engine);
            publish(hub);
        }
    }
}

/// 测试专用：清空全局状态，避免用例之间共享图实例与版本。
#[cfg(test)]
pub(crate) fn reset_for_test() {
    let hub = hub();
    *lock_engine(hub) = PreviewEngine::new();
    hub.revision.store(0, Ordering::Relaxed);
}
