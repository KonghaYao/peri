use super::*;

#[tokio::test]
async fn legacy_private_zero_ttl_cache_survives_cache_recreation() {
    let (client_io, server_io) = tokio::io::duplex(8192);
    let request_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    tokio::spawn(private_zero_ttl_skill_server(
        server_io,
        Arc::clone(&request_count),
    ));
    let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let cache_dir = tempfile::tempdir().unwrap();
    let origin = "legacy-private-origin";
    let version = "sha256:test-cache-version";
    let first_cache =
        crate::mcp::resource_cache::McpResourceCache::at(cache_dir.path().to_path_buf());
    first_cache.set_cache_version(origin, Some(version));
    let resources = vec![resource("skill://srv/cached/SKILL.md")];
    let (_, first_entries) = collect_skill_entries(
        running.peer().clone(),
        "srv",
        resources.clone(),
        AgentCancellationToken::new(),
        Some((first_cache.clone(), origin.to_string())),
    )
    .await;
    assert_eq!(first_entries.len(), 1);
    assert_eq!(
        request_count.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "首次 legacy read 必须访问远程 server"
    );

    drop(first_cache);

    // 丢弃第一个 cache 实例后，用同一磁盘根目录重建，模拟重启/新 pool。
    let second_cache =
        crate::mcp::resource_cache::McpResourceCache::at(cache_dir.path().to_path_buf());
    second_cache.set_cache_version(origin, Some(version));
    let (_, second_entries) = collect_skill_entries(
        running.peer().clone(),
        "srv",
        resources,
        AgentCancellationToken::new(),
        Some((second_cache.clone(), origin.to_string())),
    )
    .await;
    assert_eq!(second_entries.len(), 1);
    assert_eq!(
        request_count.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "匹配 cacheVersion 的 private + ttlMs:0 条目应直接从新 cache 实例读取"
    );
    assert_eq!(
        second_cache.recent_status(origin),
        Some(crate::mcp::resource_cache::CacheLoadStatus::VersionHit),
        "第二次读取必须记录 VersionHit，而不是 LiveFetch"
    );

    // 版本变化必须拒绝旧的零 TTL 内容，恢复远程读取。
    let third_cache =
        crate::mcp::resource_cache::McpResourceCache::at(cache_dir.path().to_path_buf());
    third_cache.set_cache_version(origin, Some("sha256:changed-cache-version"));
    let (_, third_entries) = collect_skill_entries(
        running.peer().clone(),
        "srv",
        vec![resource("skill://srv/cached/SKILL.md")],
        AgentCancellationToken::new(),
        Some((third_cache, origin.to_string())),
    )
    .await;
    assert_eq!(third_entries.len(), 1);
    assert_eq!(
        request_count.load(std::sync::atomic::Ordering::Relaxed),
        2,
        "cacheVersion 改变后必须重新访问远程 server，不能复用旧零 TTL 内容"
    );
}

#[tokio::test]
async fn collect_skill_entries_sorts_by_name_despite_completion_order() {
    let (client_io, server_io) = tokio::io::duplex(8192);
    // 服务端每请求独立 spawn：zebra 立即返回、alpha 延迟 200ms → 完成序
    // 确定性为 [zebra, alpha]（与 name 排序相反）；若 collect 不排序，
    // entries 将保持完成序。
    let completion_log: Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    tokio::spawn(raw_skill_server(
        server_io,
        vec![RespondRule {
            segment: "alpha",
            delay: std::time::Duration::from_millis(200),
            error: false,
        }],
        None,
        Some(Arc::clone(&completion_log)),
    ));

    let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let peer = running.peer().clone();

    let resources = vec![
        resource("skill://srv/zebra/SKILL.md"),
        resource("skill://srv/alpha/SKILL.md"),
    ];
    let cancel = AgentCancellationToken::new();
    let (_, entries) = collect_skill_entries(peer, "srv", resources, cancel, None).await;

    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    // frontmatter name = uri 最终段（zebra/alpha）→ 注册名
    assert_eq!(
        names,
        vec!["mcp__srv__alpha", "mcp__srv__zebra"],
        "JoinSet 完成序非确定，输出应按 name 排序"
    );
    // 完成序确定性：zebra（无延迟）先于 alpha（200ms 延迟）写出——与排序序
    // 相反，证明排序断言确实覆盖了乱序输入。
    assert_eq!(
        *completion_log.lock().unwrap(),
        vec![
            "skill://srv/zebra/SKILL.md".to_string(),
            "skill://srv/alpha/SKILL.md".to_string()
        ],
        "服务端完成序应为 [zebra, alpha]，与排序序相反"
    );
}

/// candidates 非空 + 全部 read 失败 → 汇总 warn（回写空条目 Discovered）。
#[test]
fn run_discovery_all_reads_fail_emits_warn() {
    let warns = Arc::new(std::sync::Mutex::new(Vec::new()));
    tracing::subscriber::with_default(
        WarnCaptureSubscriber {
            warns: Arc::clone(&warns),
        },
        || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let reg = Arc::new(McpSkillRegistry::new());
                let token: HandleToken = Arc::new(3u32);
                let (client_io, server_io) = tokio::io::duplex(8192);
                // 空段规则匹配所有 uri → 全部返回 JSON-RPC error（read 失败）
                tokio::spawn(raw_skill_server(
                    server_io,
                    vec![RespondRule {
                        segment: "",
                        delay: std::time::Duration::ZERO,
                        error: true,
                    }],
                    None,
                    None,
                ));
                let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
                    (),
                    client_io,
                    None::<rmcp::model::ServerPeerInfo>,
                );
                let handle = make_discovery_handle(
                    &running,
                    vec![
                        resource("skill://srv/a/SKILL.md"),
                        resource("skill://srv/b/SKILL.md"),
                    ],
                );
                reg.mark_discovery_started("srv", token.clone());
                let cancel = AgentCancellationToken::new();
                run_discovery(reg.clone(), None, handle, token.clone(), cancel).await;
                assert!(
                    matches!(
                        reg.discovery_state("srv"),
                        Some(ServerDiscoveryState::Discovered { entries, .. })
                            if entries.is_empty()
                    ),
                    "全部 read 失败应回写空条目 Discovered"
                );
            });
        },
    );
    let warns = warns.lock().unwrap();
    assert!(
        warns.iter().any(|m| m.contains("无可用条目")),
        "candidates 非空且全部 read 失败应发汇总 warn，实际: {warns:?}"
    );
}

/// cancel 提前退出 → clear_discovery_started 回退：首条响应后触发 cancel，
/// run_discovery 结束断言 discovery_state 为 None（Started 已清除）。
#[tokio::test]
async fn run_discovery_cancel_after_first_response_clears_started() {
    let reg = Arc::new(McpSkillRegistry::new());
    let token: HandleToken = Arc::new(4u32);
    let (client_io, server_io) = tokio::io::duplex(8192);
    let first_done = Arc::new(tokio::sync::Notify::new());
    tokio::spawn(raw_skill_server(
        server_io,
        vec![RespondRule {
            segment: "alpha",
            delay: std::time::Duration::from_secs(2),
            error: false,
        }],
        Some(Arc::clone(&first_done)),
        None,
    ));
    let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
        (),
        client_io,
        None::<rmcp::model::ServerPeerInfo>,
    );
    let handle = make_discovery_handle(
        &running,
        vec![
            resource("skill://srv/zebra/SKILL.md"),
            resource("skill://srv/alpha/SKILL.md"),
        ],
    );
    reg.mark_discovery_started("srv", token.clone());
    let cancel = AgentCancellationToken::new();
    let discovery = tokio::spawn(run_discovery(
        reg.clone(),
        None,
        handle,
        token.clone(),
        cancel.clone(),
    ));

    // zebra 立即响应、alpha 延迟 2s → 首条响应后触发 cancel
    first_done.notified().await;
    cancel.cancel();
    discovery.await.unwrap();

    assert!(
        reg.discovery_state("srv").is_none(),
        "cancel 后应 clear_discovery_started 回退（discovery_state None）"
    );
}

/// cancel 提前退出不得误报汇总 warn：首条响应为 read 失败（entries 保持空），
/// 随后 cancel → 走 clear 路径，不经过 entries.is_empty() 的 warn 分支。
#[test]
fn run_discovery_cancel_before_warn_does_not_emit_warn() {
    let warns = Arc::new(std::sync::Mutex::new(Vec::new()));
    tracing::subscriber::with_default(
        WarnCaptureSubscriber {
            warns: Arc::clone(&warns),
        },
        || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let reg = Arc::new(McpSkillRegistry::new());
                let token: HandleToken = Arc::new(5u32);
                let (client_io, server_io) = tokio::io::duplex(8192);
                let first_done = Arc::new(tokio::sync::Notify::new());
                tokio::spawn(raw_skill_server(
                    server_io,
                    vec![
                        // zebra：立即返回 error（entries 保持空）
                        RespondRule {
                            segment: "zebra",
                            delay: std::time::Duration::ZERO,
                            error: true,
                        },
                        // alpha：延迟 2s（cancel 在其完成前触发）
                        RespondRule {
                            segment: "alpha",
                            delay: std::time::Duration::from_secs(2),
                            error: false,
                        },
                    ],
                    Some(Arc::clone(&first_done)),
                    None,
                ));
                let running = rmcp::service::serve_directly::<RoleClient, _, _, _, _>(
                    (),
                    client_io,
                    None::<rmcp::model::ServerPeerInfo>,
                );
                let handle = make_discovery_handle(
                    &running,
                    vec![
                        resource("skill://srv/zebra/SKILL.md"),
                        resource("skill://srv/alpha/SKILL.md"),
                    ],
                );
                reg.mark_discovery_started("srv", token.clone());
                let cancel = AgentCancellationToken::new();
                let discovery = tokio::spawn(run_discovery(
                    reg.clone(),
                    None,
                    handle,
                    token.clone(),
                    cancel.clone(),
                ));
                first_done.notified().await;
                cancel.cancel();
                discovery.await.unwrap();
                assert!(
                    reg.discovery_state("srv").is_none(),
                    "cancel 后应 clear_discovery_started 回退"
                );
            });
        },
    );
    let warns = warns.lock().unwrap();
    assert!(
        !warns.iter().any(|m| m.contains("无可用条目")),
        "cancel 提前退出不得误报汇总 warn，实际: {warns:?}"
    );
}
