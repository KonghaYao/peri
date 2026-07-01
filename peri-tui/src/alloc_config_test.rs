use crate::alloc_config::*;

/// 测试 init_alloc_conf 不覆盖已存在的 MALLOC_CONF 环境变量。
#[test]
fn test_init_alloc_conf_does_not_overwrite() {
    let sentinel = "dirty_decay_ms:9999";
    unsafe { std::env::set_var("MALLOC_CONF", sentinel); }

    init_alloc_conf();

    // 预设值不应被覆盖
    assert_eq!(
        std::env::var("MALLOC_CONF").unwrap(),
        sentinel,
        "不应覆盖用户设置的 MALLOC_CONF"
    );

    // Cleanup
    unsafe { std::env::remove_var("MALLOC_CONF"); }
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
    // RSS >= allocated（RSS 含碎片和元数据）
    assert!(
        stats.current_rss >= stats.current_allocated,
        "RSS({}) 应 >= allocated({})",
        stats.current_rss,
        stats.current_allocated,
    );
    eprintln!(
        "stats: rss={} allocated={} gap={}",
        stats.current_rss,
        stats.current_allocated,
        stats.current_rss - stats.current_allocated,
    );
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
