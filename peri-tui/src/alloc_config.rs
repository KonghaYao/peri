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
//! - `os_rss_mb()` — OS-level RSS via sysinfo (MB)

/// Allocator stats (RSS from sysinfo + jemalloc allocated).
#[derive(Debug, Clone, Copy)]
pub struct AllocStats {
    /// OS 级 RSS（sysinfo 报告，含所有内存，字节）
    pub current_rss: usize,
    /// jemalloc stats.allocated（应用实际分配字节数，不含碎片/元数据）
    pub current_allocated: usize,
}

/// jemalloc 详细统计（需要 advance epoch 才准确）。
#[derive(Debug, Clone, Copy)]
pub struct JemallocBreakdown {
    /// 应用实际分配的字节
    pub allocated: usize,
    /// 活跃页中的字节（页对齐，>= allocated）
    pub active: usize,
    /// 物理驻留字节（含脏页、元数据，>= active）
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
    advance_epoch();
    use sysinfo::{ProcessesToUpdate, System};
    let mut sys = System::new();
    let pid = sysinfo::get_current_pid().ok()?;
    sys.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    let proc = sys.process(pid)?;
    let current_rss = proc.memory() as usize; // sysinfo returns bytes
    let current_allocated = tikv_jemalloc_ctl::stats::allocated::read().unwrap_or(current_rss);
    Some(AllocStats {
        current_rss,
        current_allocated,
    })
}

/// Query jemalloc detailed breakdown.
#[cfg(not(target_os = "windows"))]
pub fn query_breakdown() -> Option<JemallocBreakdown> {
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
    let _ = tikv_jemalloc_ctl::stats_print::stats_print(&mut buf, Default::default());
    if let Ok(s) = String::from_utf8(buf) {
        for line in s.lines() {
            tracing::info!("{line}");
        }
    }
}

/// macOS task ledger physical footprint in bytes.
#[cfg(target_os = "macos")]
pub fn physical_footprint_bytes() -> Option<u64> {
    const TASK_VM_INFO: libc::task_flavor_t = 22;

    // TASK_VM_INFO rev1 prefix from the installed macOS SDK. Asking only for this
    // prefix is ABI-stable and includes phys_footprint without guessing later fields.
    #[repr(C)]
    #[derive(Default)]
    struct TaskVmInfoRev1 {
        virtual_size: u64,
        region_count: i32,
        page_size: i32,
        resident_size: u64,
        resident_size_peak: u64,
        device: u64,
        device_peak: u64,
        internal: u64,
        internal_peak: u64,
        external: u64,
        external_peak: u64,
        reusable: u64,
        reusable_peak: u64,
        purgeable_volatile_pmap: u64,
        purgeable_volatile_resident: u64,
        purgeable_volatile_virtual: u64,
        compressed: u64,
        compressed_peak: u64,
        compressed_lifetime: u64,
        phys_footprint: u64,
    }

    let mut info = TaskVmInfoRev1::default();
    let mut count = (std::mem::size_of::<TaskVmInfoRev1>() / std::mem::size_of::<libc::natural_t>())
        as libc::mach_msg_type_number_t;
    // SAFETY: `info` is a writable C-layout TASK_VM_INFO rev1 prefix, `count`
    // exactly describes its natural_t capacity, and mach_task_self_ is the current task.
    let result = unsafe {
        #[allow(deprecated)]
        libc::task_info(
            libc::mach_task_self_,
            TASK_VM_INFO,
            (&mut info as *mut TaskVmInfoRev1).cast::<libc::integer_t>(),
            &mut count,
        )
    };
    let required_count = (std::mem::offset_of!(TaskVmInfoRev1, phys_footprint)
        + std::mem::size_of::<u64>())
    .div_ceil(std::mem::size_of::<libc::natural_t>())
        as libc::mach_msg_type_number_t;
    physical_footprint_from_task_info(result, count, required_count, info.phys_footprint)
}

#[cfg(target_os = "macos")]
pub(crate) fn physical_footprint_from_task_info(
    result: libc::kern_return_t,
    returned_count: libc::mach_msg_type_number_t,
    required_count: libc::mach_msg_type_number_t,
    physical_footprint: u64,
) -> Option<u64> {
    (result == 0 && returned_count >= required_count).then_some(physical_footprint)
}

#[cfg(not(target_os = "macos"))]
pub fn physical_footprint_bytes() -> Option<u64> {
    None
}

/// 通过 sysinfo 获取 OS 级 RSS（MB）。
/// 公共函数，供 gc.rs 和 thread_ops.rs 复用。
#[cfg(not(target_os = "windows"))]
pub fn os_rss_mb() -> Option<u64> {
    use sysinfo::{ProcessesToUpdate, System};
    let mut sys = System::new();
    let pid = sysinfo::get_current_pid().ok()?;
    sys.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    sys.process(pid).map(|p| p.memory() / 1024 / 1024) // bytes → MB
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
