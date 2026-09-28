use super::*;

// ─── 用例 3：builtin LSP 的调用与取消收敛（R29）───────────────────────

/// 真 builtin LSP handler 的链路夹具：**生产** handler（`LspMcpServer`）+ **生产**
/// transport 装配（`spawn_builtin_transport_with_tap`）+ **生产** client 握手
/// （`serve_client_auto`）+ **生产** typed bridge（`build_typed_tool_bridges`）。
///
/// client 半边的归属：service 由夹具持有（`service`），server task 经
/// `into_parts()` 登记进 pool 的代监督者表——关闭因此走 pool 的生产路径
/// （`close_builtin_task`），而不是夹具自己拼第二条关闭路径。
struct MwLspLink {
    pool: Arc<McpClientPool>,
    lsp_pool: Arc<LspServerPool>,
    service: Option<McpServiceWrapper>,
    wire: Arc<BuiltinWireLog>,
    lsp: MwFakeLsp,
    dir: tempfile::TempDir,
}

impl MwLspLink {
    async fn connect() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let lsp = MwFakeLsp::new(dir.path());
        let cwd = dir.path().to_string_lossy().to_string();
        let lsp_pool = lsp.pool(&cwd);
        let pool = Arc::new(McpClientPool::new_empty());
        let instance = find("lsp").expect("lsp 已实现");

        let (transport, wire) = spawn_builtin_transport_with_tap(
            instance.name,
            LspMcpServer::new(Arc::clone(&lsp_pool)),
        );
        // 生产拆分：client 侧 io + 本代关闭所有权（tick 归属随监督者；lsp 无 tick）。
        let (io, supervisor) = transport.into_parts();
        let service =
            serve_client_auto(io, None, None, &pool.capability_profile, HANDSHAKE_TIMEOUT)
                .await
                .expect("builtin 握手不得超时（同进程链路）")
                .expect("builtin 握手不得失败");

        // 配置侧：与默认层同形的 builtin 条目（`system_mcp = true` ⇒ live round-trip）。
        let config = builtin_entry(instance);
        pool.configs
            .write()
            .insert(instance.name.to_string(), config.clone());
        let peer = service.peer().clone();
        let discovered = list_discovered_tools(&pool, instance.name, &peer, &config)
            .await
            .expect("live tools/list 必须成功");
        pool.clients.write().insert(
            instance.name.to_string(),
            Arc::new(McpClientHandle {
                name: instance.name.to_string(),
                version: None,
                cache_version: None,
                peer: Some(peer),
                tools: discovered,
                resources: vec![],
                status: ClientStatus::Connected,
                oauth_status: Default::default(),
                source: Some(ConfigSource::Builtin {
                    instance: instance.instance.to_string(),
                }),
                url: None,
                skills_capable: false,
                channel_capable: false,
            }),
        );
        pool.register_builtin_task(instance.name.to_string(), supervisor);

        Self {
            pool,
            lsp_pool,
            service: Some(service),
            wire,
            lsp,
            dir,
        }
    }

    fn dir(&self) -> &Path {
        self.dir.path()
    }

    /// 生产 typed bridge（`mcp__lsp__LSP`）。
    fn bridge(&self) -> McpToolBridge {
        build_typed_tool_bridges(&self.pool)
            .into_iter()
            .find(|bridge| bridge.name() == "mcp__lsp__LSP")
            .expect("lsp 实例必须有 mcp__lsp__LSP 的 typed bridge")
    }

    /// 线路上 server 侧读到的 `tools/call` 条数（重放判据）。
    fn wire_call_tool_count(&self) -> usize {
        self.wire
            .methods()
            .iter()
            .filter(|method| method.as_str() == "tools/call")
            .count()
    }
}

/// `documentSymbol` 调用输入（真实存在、按扩展名路由到假服务器的文件）。
fn mw_document_symbol_call(file_path: &Path) -> Value {
    json!({
        "operation": "documentSymbol",
        "file_path": file_path.to_string_lossy(),
    })
}

/// 真实 builtin LSP：正常调用、调用方取消后无重放、同链继续服务和关闭收敛。
/// MCP 取消已传到 handler；这里不声称内部 LSP JSON-RPC 请求也被取消。
/// 语言服务器请求在释放 fixture barrier 后自行收敛，最终核对零在途和零孤儿。
/// 120 秒边界由 workspace_recovery_test 的真实 Bash 生命周期回归覆盖。
#[tokio::test]
async fn builtin_lsp_timeout_and_cancellation_converge() {
    let mut link = MwLspLink::connect().await;
    let source = link.dir().join("mw_timeout.rs");
    std::fs::write(&source, "fn mw_timeout() {}\n").expect("调用目标文件可写");
    let input = mw_document_symbol_call(&source);
    let cwd = link.dir().to_string_lossy().to_string();

    // ── ① 正常返回 ──────────────────────────────────────────────────────────────
    link.lsp.open_release();
    let text = tokio::time::timeout(
        MW_BOUND,
        link.bridge()
            .invoke(input.clone(), ToolContext::new(&[], &cwd)),
    )
    .await
    .expect("正常返回必须在有界等待内完成（同进程链路 + 真语言服务器）")
    .expect("正常路径必须成功");
    assert_eq!(
        text, MW_EMPTY_SYMBOLS_TEXT,
        "正常返回必须走完 handler → LspTool → 语言服务器"
    );
    assert_eq!(
        link.wire_call_tool_count(),
        1,
        "一次调用只允许一次 tools/call: {:?}",
        link.wire.methods()
    );
    assert_eq!(link.lsp.spawns(), 1, "只允许拉起一个语言服务器进程");
    link.lsp.await_accounting("normal-return", (1, 1)).await;
    println!(
        "[MW timeout] normal | call=ok | wire(tools/call=1) | lsp(enter=1,exit=1,in_flight=0,spawns=1)"
    );

    // ── ② 调用方取消（在飞）────────────────────────────────────────────────────
    // 服务器侧停住应答：`documentSymbol` 的 enter 行会写下，exit 行不会。
    link.lsp.close_release();
    let cancel = AgentCancellationToken::new();
    let canceller = {
        let requests = link.lsp.requests_path();
        let cancel = cancel.clone();
        async move {
            // barrier：第 2 次调用已到达语言服务器（在途的定义点），取消**之后**才可能
            // 出现在飞状态——cancel 与「已进入 handler」之间不存在竞态窗口。
            mw_await_lsp_accounting(&requests, "in-flight-enter", (2, 1)).await;
            cancel.cancel();
        }
    };
    let racer = {
        let bridge = link.bridge();
        let cancel = cancel.clone();
        let input = input.clone();
        let cwd = cwd.clone();
        async move {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => Err(EffectiveToolError::new(
                    EffectiveToolErrorCode::Cancelled,
                    "interrupted by user",
                )),
                result = bridge.invoke(input, ToolContext::new(&[], &cwd)) => result.map_err(|error| {
                    EffectiveToolError::new(EffectiveToolErrorCode::ToolFailed, error.to_string())
                }),
            }
        }
    };
    let ((), outcome) = tokio::join!(canceller, racer);
    let error = outcome.expect_err("取消必须命中 race 的取消分支（不得让调用跑完）");
    assert_eq!(
        error.code,
        EffectiveToolErrorCode::Cancelled,
        "取消分支必须是 Cancelled（interrupted）: {error:?}"
    );
    assert!(
        error.message.contains("interrupted by user"),
        "取消文本必须与 agent loop 同文案: {error:?}"
    );
    assert_eq!(
        link.wire_call_tool_count(),
        2,
        "取消不得重放：线路只多了一次 tools/call: {:?}",
        link.wire.methods()
    );
    assert_eq!(
        link.lsp.accounting(),
        (2, 1),
        "LSP 协议边界的请求尚在飞（enter=2/exit=1）；本用例不声称语言服务器已取消请求"
    );
    println!(
        "[MW timeout] cancel | returned=Cancelled(interrupted by user) | wire(tools/call=2,无重放) \
         | lsp(enter=2,exit=1,in_flight=1) ← LSP 协议边界尚在飞，释放 barrier 后核对收敛"
    );

    // 放行在飞请求 ⇒ 语言服务器边界在途归零（有界等待 barrier，非 sleep 结论）。
    link.lsp.open_release();
    link.lsp.await_accounting("cancel-converge", (2, 2)).await;
    println!("[MW timeout] cancel-converge | lsp(enter=2,exit=2,in_flight=0)");

    // 取消后同一条链路仍可服务（无重放、无残留状态）。
    let after_cancel = tokio::time::timeout(
        MW_BOUND,
        link.bridge()
            .invoke(input.clone(), ToolContext::new(&[], &cwd)),
    )
    .await
    .expect("取消后的调用必须在有界等待内完成")
    .expect("取消后同一条 client service / 同一条 wire 必须仍可服务");
    assert_eq!(after_cancel, MW_EMPTY_SYMBOLS_TEXT);
    assert_eq!(
        link.wire_call_tool_count(),
        3,
        "取消后的调用是第三次 tools/call（取消本身不产生额外调用）: {:?}",
        link.wire.methods()
    );

    // ── ③ 显式 close：有界收敛、无 orphan ──────────────────────────────────────
    let mut service = link
        .service
        .take()
        .expect("client 半边只允许移交一次（本用例的关闭顺序）");
    let _ = service.close_with_timeout(CLOSE_TIMEOUT).await;
    let started = std::time::Instant::now();
    let outcome = link
        .pool
        .close_builtin_task("lsp")
        .await
        .expect("lsp 必须有登记在册的代监督者可关闭");
    let elapsed = started.elapsed();
    assert!(
        matches!(outcome.tick, TickCloseOutcome::NotSpawned),
        "lsp 实例没有 tick 驱动（A32：tick 不放在 handler），实际: {:?}",
        outcome.tick
    );
    assert!(
        matches!(outcome.server, BuiltinServerExit::Quit(_)),
        "server task 必须在有界等待内靠 EOF 自然收敛（不是 abort），实际: {:?}",
        outcome.server
    );
    assert!(
        elapsed < BUILTIN_CONVERGE_TIMEOUT,
        "关闭必须在 `BUILTIN_CONVERGE_TIMEOUT`({BUILTIN_CONVERGE_TIMEOUT:?}) 内正常收敛，实际 {elapsed:?}"
    );
    assert_eq!(
        link.pool.builtin_task_count(),
        0,
        "关闭后代监督者表必须排空（orphan=0）"
    );
    assert_eq!(
        link.pool.builtin_tick_is_finished("lsp"),
        None,
        "关闭后条目不存在（不再有任何 tick 归属）"
    );
    assert_eq!(
        link.lsp.accounting(),
        (3, 3),
        "收敛后语言服务器边界不得有在途请求"
    );
    assert_eq!(
        link.lsp.spawns(),
        1,
        "全程只允许一个语言服务器进程（取消 / 关闭都不重启、不重放）"
    );
    println!(
        "[MW timeout] converge | explicit_close(tick=NotSpawned,server=Quit,{elapsed:?}) \
         | tasks=0(orphan=0) | lsp(enter=3,exit=3,in_flight=0,spawns=1) | pool_tick_entry=None"
    );

    // 收尾：LSP pool 有界关闭，不留 language server 孤儿进程。
    tokio::time::timeout(MW_BOUND, link.lsp_pool.shutdown())
        .await
        .expect("LSP pool 必须在有界等待内关闭（不留 language server 孤儿进程）");
}
