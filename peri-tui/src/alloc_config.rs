//! Allocator tuning for high-churn workloads.
//!
//! Using jemalloc with aggressive decay for better fragmentation handling on macOS.
//!
//! Public API:
//! - `init_alloc_conf()` — set env vars before allocator init
//! - `alloc_collect()` — force aggressive memory reclamation
//! - `query_stats()` — get allocator stats (RSS + jemalloc allocated)
//! - `query_breakdown()` — jemalloc allocated/active/resident/metadata/mapped/retained
//! - `dump_stats()` — print detailed allocator stats to stderr
//! - `os_rss_mb()` — OS-level RSS via sysinfo (MiB)

// jemalloc caches global counters at each epoch. Every refresh, including the
// implicit one in stats_print, must share the snapshot reader's lock.
#[cfg(not(target_os = "windows"))]
static STATS_SNAPSHOT: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// Allocator stats (RSS from sysinfo + jemalloc allocated).
/// The sources are sampled separately. Allocated bytes may be nonresident, so
/// RSS and allocated do not have a guaranteed ordering.
#[derive(Debug, Clone, Copy)]
pub struct AllocStats {
    /// OS 级 RSS（sysinfo 报告，含所有内存，字节）
    pub current_rss: usize,
    /// jemalloc stats.allocated（应用实际分配字节数，不含碎片/元数据）
    pub current_allocated: usize,
}

/// jemalloc 缓存统计（advance epoch 刷新）。
/// 并发分配/释放时，各内部计数不构成同一时刻的原子快照，不能断言字段间的大小关系。
#[derive(Debug, Clone, Copy)]
pub struct JemallocBreakdown {
    /// 应用实际分配的字节
    pub allocated: usize,
    /// 活跃页中的字节（页对齐；静止状态下 >= allocated）
    pub active: usize,
    /// 物理驻留字节（含脏页、元数据；静止状态下 >= active）
    pub resident: usize,
    /// jemalloc 元数据开销
    pub metadata: usize,
    /// 映射的字节
    pub mapped: usize,
    /// 保留未归还 OS 的字节
    pub retained: usize,
}

/// Set allocator environment variables before initialization.
#[cfg(not(target_os = "windows"))]
pub fn init_alloc_conf() {
    if std::env::var("MALLOC_CONF").is_err() {
        unsafe {
            std::env::set_var(
                "MALLOC_CONF",
                "dirty_decay_ms:0,muzzy_decay_ms:0,background_thread:true",
            );
        }
    }
}

#[cfg(target_os = "windows")]
pub fn init_alloc_conf() {}

/// Force jemalloc to aggressively reclaim freed memory.
#[cfg(not(target_os = "windows"))]
pub fn alloc_collect() {
    let _snapshot = STATS_SNAPSHOT.lock();
    let _ = tikv_jemalloc_ctl::epoch::advance();
    // Purge each arena
    if let Ok(n) = tikv_jemalloc_ctl::arenas::narenas::read() {
        for i in 0..n {
            let key = format!("arena.{}.purge\0", i);
            // Safety: key is null-terminated, jemalloc handles arena.purge
            unsafe {
                tikv_jemalloc_sys::mallctl(
                    key.as_ptr() as *const _,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    0usize,
                );
            }
        }
    }
    std::thread::yield_now();
    let _ = tikv_jemalloc_ctl::epoch::advance();
}

#[cfg(target_os = "windows")]
pub fn alloc_collect() {}

/// Advance jemalloc epoch to refresh cached stats.
#[cfg(not(target_os = "windows"))]
fn advance_epoch() {
    let _ = tikv_jemalloc_ctl::epoch::advance();
}

/// Query RSS + jemalloc allocated bytes.
#[cfg(not(target_os = "windows"))]
pub fn query_stats() -> Option<AllocStats> {
    let current_rss = usize::try_from(process_rss_bytes()?).ok()?;
    Some(stats_with_rss(current_rss))
}

#[cfg(not(target_os = "windows"))]
fn stats_with_rss(current_rss: usize) -> AllocStats {
    let _snapshot = STATS_SNAPSHOT.lock();
    advance_epoch();
    let current_allocated = tikv_jemalloc_ctl::stats::allocated::read().unwrap_or(current_rss);
    AllocStats {
        current_rss,
        current_allocated,
    }
}

/// Query jemalloc detailed breakdown.
#[cfg(not(target_os = "windows"))]
pub fn query_breakdown() -> Option<JemallocBreakdown> {
    let _snapshot = STATS_SNAPSHOT.lock();
    advance_epoch();
    Some(JemallocBreakdown {
        allocated: tikv_jemalloc_ctl::stats::allocated::read().ok()?,
        active: tikv_jemalloc_ctl::stats::active::read().ok()?,
        resident: tikv_jemalloc_ctl::stats::resident::read().ok()?,
        metadata: tikv_jemalloc_ctl::stats::metadata::read().ok()?,
        mapped: tikv_jemalloc_ctl::stats::mapped::read().ok()?,
        retained: tikv_jemalloc_ctl::stats::retained::read().ok()?,
    })
}

/// Print jemalloc full stats to stderr via tracing.
#[cfg(not(target_os = "windows"))]
pub fn dump_stats() {
    let mut buf = Vec::new();
    {
        let _snapshot = STATS_SNAPSHOT.lock();
        let _ = tikv_jemalloc_ctl::stats_print::stats_print(&mut buf, Default::default());
    }
    if let Ok(s) = String::from_utf8(buf) {
        for line in s.lines() {
            tracing::info!("{line}");
        }
    }
}

/// 通过 sysinfo 获取 OS 级 RSS（MiB）。
/// 公共函数，供 gc.rs 和 thread_ops.rs 复用。
#[cfg(not(target_os = "windows"))]
pub fn os_rss_mb() -> Option<u64> {
    process_rss_bytes().map(bytes_to_mib)
}

#[cfg(not(target_os = "windows"))]
fn bytes_to_mib(bytes: u64) -> u64 {
    bytes / 1024 / 1024
}

#[cfg(not(target_os = "windows"))]
fn process_rss_bytes() -> Option<u64> {
    use sysinfo::{ProcessesToUpdate, System};
    let mut sys = System::new();
    let pid = sysinfo::get_current_pid().ok()?;
    sys.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    sys.process(pid).map(sysinfo::Process::memory)
}

// ── Windows stubs ──────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
pub fn query_stats() -> Option<AllocStats> {
    None
}
#[cfg(target_os = "windows")]
pub fn query_breakdown() -> Option<JemallocBreakdown> {
    None
}
#[cfg(target_os = "windows")]
pub fn dump_stats() {}
#[cfg(target_os = "windows")]
pub fn os_rss_mb() -> Option<u64> {
    None
}

#[cfg(test)]
#[path = "alloc_config_test.rs"]
mod tests;
