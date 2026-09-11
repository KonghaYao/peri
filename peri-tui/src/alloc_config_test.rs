use super::*;

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
    use std::alloc::{GlobalAlloc, Layout};
    use std::sync::{Arc, Barrier};

    // Exercise jemalloc itself without changing the test binary's allocator.
    struct Allocation(*mut u8, Layout);
    impl Allocation {
        fn new(size: usize) -> Self {
            let layout = Layout::from_size_align(size, 8).unwrap();
            // SAFETY: layout is nonzero and valid; the matching allocator owns
            // the pointer until Drop and no references escape this fixture.
            let ptr = unsafe { tikv_jemallocator::Jemalloc.alloc_zeroed(layout) };
            assert!(!ptr.is_null());
            Self(ptr, layout)
        }
    }
    impl Drop for Allocation {
        fn drop(&mut self) {
            // SAFETY: same allocator and layout used in Allocation::new.
            unsafe { tikv_jemallocator::Jemalloc.dealloc(self.0, self.1) };
        }
    }

    let start = Arc::new(Barrier::new(4));
    let tasks: Vec<_> = (0..4)
        .map(|worker| {
            let start = start.clone();
            std::thread::spawn(move || {
                start.wait();
                for round in 0..48 {
                    let allocation = Allocation::new((worker + 1) * 128 * 1024);
                    match (worker, round % 12) {
                        (0, 0) => alloc_collect(),
                        (1, 0) => {
                            query_stats().expect("RSS and allocator snapshot");
                        }
                        (2, 0) if round == 0 => dump_stats(),
                        _ => {}
                    }
                    let bd = query_breakdown().expect("jemalloc snapshot");
                    drop(allocation);
                    assert!(bd.allocated <= bd.active, "{bd:?}");
                    assert!(bd.active <= bd.resident, "{bd:?}");
                }
            })
        })
        .collect();
    let outcomes: Vec<_> = tasks
        .into_iter()
        .map(std::thread::JoinHandle::join)
        .collect();
    for outcome in outcomes {
        outcome.unwrap();
    }
}

/// jemalloc breakdown 查询仅在非 Windows 平台有效（Windows stub 返回 None）
#[cfg(not(target_os = "windows"))]
#[test]
fn test_breakdown_shows_fragmentation() {
    let bd = query_breakdown().expect("query_breakdown 应返回数据");
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
    // 分配一些内存让 stats 有意义
    let _vec: Vec<usize> = (0..256 * 1024).collect();
    eprintln!("=== jemalloc full stats ===");
    dump_stats();
    eprintln!("=== end ===");
}
