use super::*;

// ─── 用例 1：策略关闭五维（LSP 两键 4 格 × Cron 开/关 = 8 格）────────────────────

/// sub-plan-v §5 真值表的**逐格**覆盖（主 plan §8 第 10 行）：策略关闭是**非物理**关闭，
/// 五维（工具可见性 / 同步 read·change·save / tick / readiness / 物理生命周期）必须逐格
/// 可观察，且与「MCP 配置 `disabled`」「`PERI_MCP_BUILTIN=off`」「物理 close」三类关闭
/// **不等价**（后三类由 `four_close_sources_are_distinct` 分列）。
///
/// 矩阵（每格断言后再打印）：
///
/// | Cron 键 | LSP 工具键 | LSP 同步键 | 工具可见性 | 同步 ready/read/change/save | tick | readiness |
/// |---|---|---|---|---|---|---|
/// | 开 | 开 | 开 | cron=3、lsp=1，均 deferred | 1/1/1（文本来自磁盘） | 1 driver（每格真触发一次） | 两实例 ready，池 Ready{4} |
/// | 开 | 开 | 关 | cron=3、lsp=1 | 0/0/0（不装同步槽位） | 仍 1 | 不变 |
/// | 开 | 关 | 开 | cron=3、lsp=0 | 0/0/0（不读文件） | 仍 1 | 不变 |
/// | 开 | 关 | 关 | cron=3、lsp=0 | 0/0/0 | 仍 1 | 不变 |
/// | 关 | … | … | cron=0、lsp 随 LSP 键 | 同上逐格 | 仍 1（**策略关闭不停 tick**，且有既有注册任务） | 不变 |
///
/// 物理生命周期维在每格断言：builtin 代监督者表仍 4 项、两实例句柄仍是**同一份** Arc、
/// 组合根 scheduler 里的既有任务未被销毁；另以「LSP 工具（raw typed bridge）仍调用成功」
/// 证明 handler 未被物理销毁——**策略关闭只关本 turn 投影**。
#[tokio::test]
async fn policy_close_five_dimensions() {
    let mut fixture = Wave3Fixture::start(Wave3Spec {
        tick_enabled: true,
        ..Wave3Spec::default()
    })
    .await;
    // 本用例的 LSP 调用走「立即应答」形态（放行文件预先存在）。
    fixture.open_lsp_release();

    let pool = Arc::clone(&fixture.pool);
    assert_eq!(
        pool.builtin_task_count(),
        BUILTIN_MCP_INSTANCES.len(),
        "前置：四个实例都必须登记代监督者（否则物理维断言无意义）"
    );
    assert!(
        matches!(
            *pool.init_status.read(),
            McpInitStatus::Ready { total } if total == BUILTIN_MCP_INSTANCES.len()
        ),
        "前置：启动必须收口成 Ready{{{}}}，实际 {:?}",
        BUILTIN_MCP_INSTANCES.len(),
        pool.init_status.read()
    );
    assert_eq!(
        pool.builtin_tick_is_finished("cron"),
        Some(false),
        "前置：tick_enabled=true ⇒ cron 代必须有**运行中**的 tick（TickGuard 未结束）"
    );
    assert_eq!(
        pool.builtin_tick_is_finished("lsp"),
        Some(true),
        "前置：lsp 实例没有 tick 驱动（A32：tick 不放在 handler）"
    );
    let cron_task = fixture.register_cron_task("0 3 * * *", "mw-policy-close");
    let cron_handle_before = pool.get_client("cron").expect("cron 必须已连接");
    let lsp_handle_before = pool.get_client("lsp").expect("lsp 必须已连接");
    // 期望的可见性计数一律**从注册表派生**（不硬编码名字 / 条数）：注册表增删工具时本用例自动跟随。
    let cron_declared = mw_declared_tool_count("cron");
    let lsp_declared = mw_declared_tool_count("lsp");
    let web_declared = mw_declared_tool_count("web");
    let artifact_declared = mw_declared_tool_count("artifact");
    assert_eq!(
        (cron_declared, lsp_declared),
        (3, 1),
        "用例前提（真值表数字）：cron 三工具、lsp 单工具"
    );

    let sync_body = "fn mw_sync() {}\n";
    let mut cells = 0usize;
    for cron_open in [true, false] {
        // LSP 两键 4 格（工具键 × 同步键），与 sub-plan-v §5 的四行逐一对应。
        for (lsp_tool_open, lsp_sync_open) in
            [(true, true), (true, false), (false, true), (false, false)]
        {
            cells += 1;
            let mut disabled: HashSet<String> = HashSet::new();
            if !cron_open {
                disabled.insert("CronMiddleware".to_string());
            }
            if !lsp_tool_open {
                disabled.insert("LspMiddleware".to_string());
            }
            if !lsp_sync_open {
                disabled.insert("LspSyncMiddleware".to_string());
            }
            let disabled_list: Vec<&str> = disabled.iter().map(String::as_str).collect();
            let closed = closed_instances(&disabled);
            let label =
                format!("cron_key={cron_open} lsp_key={lsp_tool_open} sync_key={lsp_sync_open}");

            // ── 维①：工具可见性（每 turn 投影）────────────────────────────────────
            let projection = fixture.projections(&disabled_list);
            let cron_tools = mw_instance_tools(&projection, "cron");
            let lsp_tools = mw_instance_tools(&projection, "lsp");
            assert_eq!(
                cron_tools.len(),
                if cron_open { cron_declared } else { 0 },
                "[{label}] cron 工具可见性必须随 CronMiddleware 键变化: {cron_tools:?}"
            );
            assert_eq!(
                lsp_tools.len(),
                if lsp_tool_open { lsp_declared } else { 0 },
                "[{label}] lsp 工具可见性必须随 LspMiddleware 键变化: {lsp_tools:?}"
            );
            assert_eq!(
                mw_instance_tools(&projection, "web").len(),
                web_declared,
                "[{label}] 其它 builtin 实例不得受 LSP/Cron 策略键影响"
            );
            assert_eq!(
                mw_instance_tools(&projection, "artifact").len(),
                artifact_declared,
                "[{label}] artifact 不得受策略键影响"
            );
            // 注册表声明 direct 的两个实例（cron / lsp）一律 deferred。
            let direct_of = |name: &str| -> Option<bool> {
                projection
                    .iter()
                    .find(|candidate| candidate.name() == name)
                    .map(|candidate| candidate.is_direct())
            };
            for tool in cron_tools.iter().chain(lsp_tools.iter()) {
                assert_eq!(
                    direct_of(tool),
                    Some(false),
                    "[{label}] cron / lsp 工具必须 deferred（IF-D13）: {tool}"
                );
            }
            // 裸名不得回流（effective name 之外没有第二条可见路径）。
            for bare in ["LSP", "cron_register", "cron_list", "cron_remove"] {
                assert!(
                    !projection.iter().any(|tool| tool.name() == bare),
                    "[{label}] 不得出现裸名 {bare}"
                );
            }

            // ── 维②：同步 read / change / save ────────────────────────────────────
            let sync_dir = tempfile::tempdir().expect("同步观测目录");
            let sync_file = sync_dir.path().join("mw_sync.rs");
            std::fs::write(&sync_file, sync_body).expect("同步观测文件可写");
            let port = Arc::new(MwSyncPort::new(true));
            let mut hook = MwHookState::new(&sync_dir.path().to_string_lossy());
            let mounted = mw_sync_slot_mounted(&disabled);
            if mounted {
                mw_run_write_sync(&pool, &port, &mut hook, &sync_file)
                    .await
                    .unwrap_or_else(|error| panic!("[{label}] 同步不得改变工具结果: {error}"));
            }
            let expected_sync = if mounted { (1, 1, 1) } else { (0, 0, 0) };
            assert_eq!(
                port.counts(),
                expected_sync,
                "[{label}] 同步端口计数（ready_for, did_change, did_save）必须与槽位装/不装一致，\
                 轨迹: {:?}",
                port.steps()
            );
            if mounted {
                assert_eq!(
                    port.synced_text().as_deref(),
                    Some(sync_body),
                    "[{label}] 「读文件发生过」的判据：did_change 收到的文本必须逐字等于磁盘内容"
                );
                assert_eq!(
                    port.steps(),
                    vec![
                        MwSyncStep::Ready(sync_file.clone()),
                        MwSyncStep::Change {
                            path: sync_file.clone(),
                            text: sync_body.to_string(),
                        },
                        MwSyncStep::Save(sync_file.clone()),
                    ],
                    "[{label}] 顺序必须是 ready_for → did_change → did_save（ready_for 是读文件之前的唯一前置门）"
                );
            } else {
                assert!(
                    port.steps().is_empty(),
                    "[{label}] 不装同步槽位时端口不得被触达（不读文件、不发通知）: {:?}",
                    port.steps()
                );
            }

            // ── 维③：tick（1 driver，策略关闭不得停）──────────────────────────────
            assert_eq!(
                pool.builtin_tick_is_finished("cron"),
                Some(false),
                "[{label}] 策略关闭不得让 cron 的 tick 结束（TickGuard 必须仍在运行）"
            );
            fixture
                .await_armed_trigger(&cron_task, "mw-policy-close")
                .await;

            // ── 维④：readiness（两实例 ready，不变）──────────────────────────────
            assert!(
                matches!(
                    *pool.init_status.read(),
                    McpInitStatus::Ready { total } if total == BUILTIN_MCP_INSTANCES.len()
                ),
                "[{label}] 池的 readiness 收口不得因策略关闭变化，实际 {:?}",
                pool.init_status.read()
            );
            for instance_name in ["cron", "lsp"] {
                let handle = pool
                    .get_client(instance_name)
                    .unwrap_or_else(|| panic!("[{label}] {instance_name} 必须仍已连接"));
                assert!(
                    matches!(handle.status, ClientStatus::Connected),
                    "[{label}] {instance_name} 必须仍 Connected，实际 {:?}",
                    handle.status
                );
                assert!(
                    pool.discovery_evidence(instance_name)
                        .is_some_and(|evidence| evidence.is_complete()),
                    "[{label}] {instance_name} 的发现证据必须仍完整（不撤 ready）"
                );
            }
            // 闸门仍放行：策略关闭不产生 fatal，也不撤 ready 候选。
            let middleware = McpMiddleware::new(Arc::clone(&pool))
                .with_tool_pool(Arc::clone(&pool))
                .with_builtin_closures(closed.clone());
            let mut probe = StartupProbe::default();
            Middleware::before_react_start(&middleware, &mut probe)
                .await
                .unwrap_or_else(|error| panic!("[{label}] 策略关闭后闸门仍必须放行: {error}"));
            assert!(
                probe.staged.is_some(),
                "[{label}] 策略关闭不得阻止启动候选提交"
            );

            // ── 维⑤：物理生命周期（handler / tick / pool 保留）────────────────────
            assert_eq!(
                pool.builtin_task_count(),
                BUILTIN_MCP_INSTANCES.len(),
                "[{label}] 策略关闭不得销毁任何 builtin 代监督者"
            );
            assert!(
                Arc::ptr_eq(
                    &cron_handle_before,
                    &pool.get_client("cron").expect("cron 必须仍在池中")
                ),
                "[{label}] cron 句柄必须仍是同一份 Arc（不重连、不销毁）"
            );
            assert!(
                Arc::ptr_eq(
                    &lsp_handle_before,
                    &pool.get_client("lsp").expect("lsp 必须仍在池中")
                ),
                "[{label}] lsp 句柄必须仍是同一份 Arc（不重连、不销毁）"
            );
            assert_eq!(
                fixture.cron_task_count(),
                1,
                "[{label}] 组合根 scheduler 里的既有注册任务不得被策略关闭销毁"
            );

            // 打印口径：`read` 由上面的「文本逐字等于磁盘内容」断言支撑（端口不读文件，
            // 内容只可能来自中间件的读盘），不是独立计数。
            let expect_read = if mounted { 1 } else { 0 };
            println!(
                "[MW policy-close] {label} | visible(cron={},lsp={},web=2,artifact=1,deferred=true) \
                 | sync(ready_for={},read={},change={},save={}) | tick(driver=1,trigger=1) \
                 | ready(connected=2/2,pool=Ready{{4}},gate=Ok) | phys(tasks=4,handler=retained,pool=same)",
                cron_tools.len(),
                lsp_tools.len(),
                expected_sync.0,
                expect_read,
                expected_sync.1,
                expected_sync.2,
            );
        }
    }
    assert_eq!(
        cells, 8,
        "必须逐格覆盖 LSP 两键 4 格 × Cron 开/关 = 8 个策略组合"
    );

    // ── tick 维度的**代级读法**（`into_parts` → `BuiltinInstanceSupervisor` / `TickGuard`）──
    // 再走一次生产 spawn 点，直接读监督者的 tick 状态，并按冻结顺序关闭这一代。
    // 这一代是夹具**自持**的（不登记进 pool 表），因此不影响上面矩阵的「池内 1 driver」读法，
    // 也不改变池的 task 计数；它在本块内被完整收敛。
    {
        let extra = pool
            .spawn_builtin_transport("cron")
            .expect("cron 必须已接线（dispatch 覆盖四个已实现实例）");
        let (io, supervisor) = extra.into_parts();
        assert_eq!(supervisor.instance(), "cron", "监督者必须归属被装配的实例");
        assert!(
            !supervisor.tick_is_finished(),
            "生产 spawn 点必须为 tick_enabled=true 的 cron 代挂上**运行中**的 tick（TickGuard 未结束）"
        );
        let mut extra_service = serve_client_auto(io, &pool.capability_profile, HANDSHAKE_TIMEOUT)
            .await
            .expect("附加代的 builtin 握手不得超时")
            .expect("附加代的 builtin 握手不得失败");
        let _ = extra_service.close_with_timeout(CLOSE_TIMEOUT).await;
        let outcome = supervisor.close(BUILTIN_CONVERGE_TIMEOUT).await;
        assert!(
            matches!(outcome.tick, TickCloseOutcome::Joined),
            "冻结顺序：先 cancel 并有界 join tick，实际: {:?}",
            outcome.tick
        );
        assert!(
            matches!(outcome.server, BuiltinServerExit::Quit(_)),
            "server task 必须靠 EOF 自然收敛，实际: {:?}",
            outcome.server
        );
        println!(
            "[MW tick-generation] instance=cron | tick_is_finished=false(装配后) | close(tick=Joined,server=Quit) | pool_tasks=4(夹具自持,不登记)"
        );
    }

    // ── 同步前（ready_for）的可证伪形态 ──────────────────────────────────────────
    // ready_for=false ⇒ 不得读文件、不得发通知（这就把「ready_for 先于读文件」变成可失败断言：
    // 任何先读文件 / 先发通知的实现都会让 (1,0,0) 变成 (1,1,1)）。
    let gate_dir = tempfile::tempdir().expect("门控观测目录");
    let gate_file = gate_dir.path().join("mw_gate.rs");
    std::fs::write(&gate_file, sync_body).expect("门控观测文件可写");
    let not_ready = Arc::new(MwSyncPort::new(false));
    let mut hook = MwHookState::new(&gate_dir.path().to_string_lossy());
    mw_run_write_sync(&pool, &not_ready, &mut hook, &gate_file)
        .await
        .expect("不就绪也只是跳过，不得改变工具结果");
    assert_eq!(
        not_ready.counts(),
        (1, 0, 0),
        "ready_for 是读文件之前的唯一前置门：不就绪 ⇒ 不读文件、不发通知，轨迹: {:?}",
        not_ready.steps()
    );
    assert_eq!(
        not_ready.steps(),
        vec![MwSyncStep::Ready(gate_file.clone())],
        "不就绪时端口只允许被 ready_for 触达一次"
    );
    assert!(
        not_ready.synced_text().is_none(),
        "不就绪时不得有任何内容进入端口（即「读文件」未发生）"
    );
    println!(
        "[MW gate] ready_for=false ⇒ counts(ready_for=1,read=0,change=0,save=0) | steps=1 | file_read=false"
    );

    // ── 「LSP 工具仍可见可调用」与「策略关闭不是物理销毁」───────────────────────
    // ① `LspSyncMiddleware=false`（只关同步）：工具**可见**，且经生产 typed bridge 调用**成功**。
    let lsp_sync_off = ["LspSyncMiddleware"];
    let projection = fixture.projections(&lsp_sync_off);
    assert_eq!(
        mw_instance_tools(&projection, "lsp"),
        vec!["mcp__lsp__LSP".to_string()],
        "只关同步中间件时 LSP 工具必须仍可见"
    );
    let source_file = fixture.project.join("mw_callable.rs");
    std::fs::write(&source_file, "fn mw_callable() {}\n").expect("调用目标文件可写");
    let call_input = json!({
        "operation": "documentSymbol",
        "file_path": source_file.to_string_lossy(),
    });
    let call_cwd = fixture.project.to_string_lossy().to_string();
    let text = tokio::time::timeout(
        MW_BOUND,
        fixture
            .typed_bridge("mcp__lsp__LSP")
            .invoke(call_input.clone(), ToolContext::new(&[], &call_cwd)),
    )
    .await
    .expect("LSP 工具调用必须在有界等待内返回（真 builtin 链路 + 真语言服务器）")
    .expect("关闭同步中间件不得影响 LSP 工具的调用");
    assert_eq!(
        text, MW_EMPTY_SYMBOLS_TEXT,
        "调用必须真的走完 handler → LspTool → 语言服务器（空符号表的固定文本）"
    );
    assert!(
        fixture.lsp_spawns() >= 1,
        "调用成功必须伴随语言服务器真的被拉起"
    );
    println!(
        "[MW lsp-callable] visible=true | call=ok(text={MW_EMPTY_SYMBOLS_TEXT:?}) | lsp_spawns>=1 \
         | sync(counts=0/0/0) | phys(handler=retained)"
    );

    // ② `LspMiddleware=false`（实例键）：**投影**归零，但物理 handler 仍在（raw typed bridge
    //    仍可调用成功）——策略关闭是投影关闭，不是物理销毁。
    let lsp_tool_off = ["LspMiddleware"];
    let projection = fixture.projections(&lsp_tool_off);
    assert!(
        mw_instance_tools(&projection, "lsp").is_empty(),
        "实例键关闭后 LSP 工具必须从投影中消失"
    );
    let raw_text = tokio::time::timeout(
        MW_BOUND,
        fixture
            .typed_bridge("mcp__lsp__LSP")
            .invoke(call_input, ToolContext::new(&[], &call_cwd)),
    )
    .await
    .expect("策略关闭后 raw bridge 调用仍必须在有界等待内返回")
    .expect("策略关闭不得物理销毁 handler：raw typed bridge 仍必须成功");
    assert_eq!(
        raw_text, MW_EMPTY_SYMBOLS_TEXT,
        "策略关闭只关投影：handler → LspTool → 语言服务器这条链必须仍然完好"
    );
    println!(
        "[MW lsp-instance-closed] visible=false | raw_bridge_call=ok | phys(handler=retained,tasks=4)"
    );

    fixture.shutdown().await;
}
