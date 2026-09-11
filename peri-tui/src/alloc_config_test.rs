use super::*;

// libtest uses System; an ordinary Vec does not exercise jemalloc here.
#[cfg(not(target_os = "windows"))]
struct Allocation(*mut u8, std::alloc::Layout);

#[cfg(not(target_os = "windows"))]
impl Allocation {
    fn new(size: usize) -> Self {
        use std::alloc::GlobalAlloc;
        let layout = std::alloc::Layout::from_size_align(size, 8).unwrap();
        // SAFETY: layout is nonzero and valid; Drop uses the same allocator
        // and layout, and no references escape this fixture.
        let ptr = unsafe { tikv_jemallocator::Jemalloc.alloc_zeroed(layout) };
        assert!(!ptr.is_null());
        Self(ptr, layout)
    }
}

#[cfg(not(target_os = "windows"))]
impl Drop for Allocation {
    fn drop(&mut self) {
        use std::alloc::GlobalAlloc;
        // SAFETY: this pointer is owned by the matching allocator and layout.
        unsafe { tikv_jemallocator::Jemalloc.dealloc(self.0, self.1) };
    }
}

/// 测试 init_alloc_conf 不覆盖已存在的 MALLOC_CONF 环境变量。
#[test]
fn test_init_alloc_conf_does_not_overwrite() {
    const CHILD: &str = "PERI_ALLOC_CONF_TEST_CHILD";
    let sentinel = "dirty_decay_ms:9999";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "alloc_config::tests::test_init_alloc_conf_does_not_overwrite",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("MALLOC_CONF", sentinel)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "isolated allocator init test failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        return;
    }

    init_alloc_conf();

    // 预设值不应被覆盖
    assert_eq!(
        std::env::var("MALLOC_CONF").unwrap(),
        sentinel,
        "不应覆盖用户设置的 MALLOC_CONF"
    );
}

#[test]
fn test_alloc_collect_does_not_panic() {
    // alloc_collect 应可安全多次调用
    alloc_collect();
    alloc_collect();
    alloc_collect();
}

/// jemalloc stats 查询仅在非 Windows 平台有效（Windows stub 返回 None）
#[cfg(not(target_os = "windows"))]
#[test]
fn test_query_stats_returns_valid_data() {
    let stats = query_stats().expect("query_stats 应返回数据");
    assert!(stats.current_rss > 0, "RSS 应大于 0");
    assert!(stats.current_allocated > 0, "jemalloc allocated 应大于 0");
    // RSS excludes swapped/untouched pages; allocator allocated counts are not
    // a subset of resident bytes, and the two sources are sampled separately.
}

#[cfg(not(target_os = "windows"))]
#[test]
fn test_rss_unit_contract_preserves_sysinfo_bytes_and_converts_to_mib() {
    let rss_bytes = process_rss_bytes().expect("current process RSS");
    let stats = stats_with_rss(usize::try_from(rss_bytes).unwrap());
    assert_eq!(stats.current_rss as u64, rss_bytes);
    assert_eq!(bytes_to_mib(7 * 1024 * 1024 + 1023), 7);
    assert_eq!(bytes_to_mib(1024 * 1024 - 1), 0);
}

#[cfg(not(target_os = "windows"))]
#[test]
fn test_concurrent_epoch_refresh_preserves_breakdown_invariants() {
    use std::sync::{Arc, Barrier};

    if run_isolated_stats_test("test_concurrent_epoch_refresh_preserves_breakdown_invariants") {
        return;
    }
    let start = Arc::new(Barrier::new(4));
    let tasks: Vec<_> = (0..4)
        .map(|worker| {
            let start = start.clone();
            std::thread::spawn(move || {
                let mut snapshots = Vec::new();
                let mut rss_snapshots = Vec::new();
                for round in 0..48 {
                    let allocation = Allocation::new((worker + 1) * 128 * 1024);
                    // Epoch refresh serializes readers, not allocation/free. Keep
                    // the real allocations stable while all readers sample them.
                    start.wait();
                    match (worker, round % 12) {
                        (0, 0) => alloc_collect(),
                        (1, 0) => {
                            rss_snapshots.push(query_stats());
                        }
                        (2, 0) if round == 0 => dump_stats(),
                        _ => {}
                    }
                    snapshots.push(query_breakdown());
                    // No worker may free or replace an allocation before every
                    // snapshot in this round completes. Assert after all joins so
                    // a failed assertion cannot strand a peer at this barrier.
                    start.wait();
                    drop(allocation);
                }
                (snapshots, rss_snapshots)
            })
        })
        .collect();
    let outcomes: Vec<_> = tasks
        .into_iter()
        .map(std::thread::JoinHandle::join)
        .collect();
    for outcome in outcomes {
        let (snapshots, rss_snapshots) = outcome.unwrap();
        for rss in rss_snapshots {
            assert!(rss.expect("RSS and allocator snapshot").current_rss > 0);
        }
        assert_eq!(snapshots.len(), 48);
        for snapshot in snapshots {
            let bd = snapshot.expect("jemalloc snapshot");
            assert!(
                bd.allocated >= 10 * 128 * 1024,
                "all fixture allocations are live: {bd:?}"
            );
            assert!(bd.allocated <= bd.active, "{bd:?}");
            assert!(bd.active <= bd.resident, "{bd:?}");
        }
    }
}

/// [回归测试] 查询必须反映真实 jemalloc 分配/释放，不能重复返回旧 epoch 缓存。
#[cfg(not(target_os = "windows"))]
#[test]
fn test_breakdown_refreshes_after_allocation_and_release() {
    if run_isolated_stats_test("test_breakdown_refreshes_after_allocation_and_release") {
        return;
    }
    // Warm up jemalloc's thread state before recording the baseline. A large
    // allocation bypasses the default small-object thread cache on both targets.
    const BYTES: usize = 4 * 1024 * 1024;
    drop(Allocation::new(BYTES));
    let before = query_breakdown().expect("初始统计");
    let allocation = Allocation::new(BYTES);
    let live = query_breakdown().expect("分配后统计");
    assert!(
        live.allocated >= before.allocated + BYTES,
        "新分配必须出现在统计中: before={before:?}, live={live:?}"
    );
    drop(allocation);
    let released = query_breakdown().expect("释放后统计");
    assert!(
        released.allocated + BYTES <= live.allocated,
        "释放必须出现在统计中: live={live:?}, released={released:?}"
    );
}

// Stats inequalities apply to a quiescent allocator. Other libtest cases must
// not allocate in jemalloc while this test asserts those relationships.
#[cfg(not(target_os = "windows"))]
fn run_isolated_stats_test(name: &str) -> bool {
    const CHILD: &str = "PERI_ALLOC_STATS_TEST_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(name) {
        return false;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("alloc_config::tests::{name}"),
            "--nocapture",
        ])
        .env(CHILD, name)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed;"),
        "isolated stats test {name} must execute successfully:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

/// jemalloc breakdown 查询仅在非 Windows 平台有效（Windows stub 返回 None）
#[cfg(not(target_os = "windows"))]
#[test]
fn test_breakdown_shows_fragmentation() {
    if run_isolated_stats_test("test_breakdown_shows_fragmentation") {
        return;
    }
    let _allocation = Allocation::new(129 * 1024);
    let bd = query_breakdown().expect("query_breakdown 应返回数据");
    assert!(bd.allocated >= 129 * 1024, "必须包含真实分配: {bd:?}");
    eprintln!("jemalloc breakdown:");
    eprintln!("  allocated: {} bytes", bd.allocated);
    eprintln!(
        "  active:    {} bytes (frag: {})",
        bd.active,
        bd.active.saturating_sub(bd.allocated)
    );
    eprintln!(
        "  resident:  {} bytes (waste: {})",
        bd.resident,
        bd.resident.saturating_sub(bd.active)
    );
    eprintln!("  metadata:  {} bytes", bd.metadata);
    eprintln!("  mapped:    {} bytes", bd.mapped);
    eprintln!("  retained:  {} bytes", bd.retained);
    // 层级关系：allocated <= active <= resident
    assert!(
        bd.allocated <= bd.active,
        "allocated({}) 应 <= active({})",
        bd.allocated,
        bd.active
    );
    assert!(
        bd.active <= bd.resident,
        "active({}) 应 <= resident({})",
        bd.active,
        bd.resident
    );
}

#[cfg(not(target_os = "windows"))]
#[test]
fn test_dump_stats() {
    // libtest 默认的 System 分配器不会出现在 jemalloc 统计里。
    let _allocation = Allocation::new(2 * 1024 * 1024);
    eprintln!("=== jemalloc full stats ===");
    dump_stats();
    eprintln!("=== end ===");
}
