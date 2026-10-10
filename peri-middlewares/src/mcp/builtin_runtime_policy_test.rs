use super::*;

// ─── 用例 1：策略关闭四维（Cron 开/关 = 2 格）────────────────────────────────────

/// sub-plan-v §5 真值表的**逐格**覆盖（主 plan §8 第 10 行）：策略关闭是**非物理**关闭，
/// 四维（工具可见性 / tick / readiness / 物理生命周期）必须逐格可观察，且与
/// 「MCP 配置 `disabled`」「`PERI_MCP_BUILTIN=off`」「物理 close」三类关闭
/// **不等价**（后三类由 `four_close_sources_are_distinct` 分列）。
///
/// 矩阵（每格断言后再打印）：
///
/// | Cron 键 | 工具可见性 | tick | readiness |
/// |---|---|---|---|
/// | 开 | cron=3，deferred | 1 driver（每格真触发一次） | 池 Ready{4} |
/// | 关 | cron=0 | 仍 1（**策略关闭不停 tick**，且有既有注册任务） | 不变 |
///
/// 物理生命周期维在每格断言：builtin 代监督者表仍 4 项、cron 句柄仍是**同一份** Arc、
/// 组合根 scheduler 里的既有任务未被销毁；另以「cron 工具（raw typed bridge）仍调用成功」
/// 证明 handler 未被物理销毁——**策略关闭只关本 turn 投影**。
#[tokio::test]
async fn policy_close_dimensions() {
    let mut fixture = Wave3Fixture::start(Wave3Spec {
        tick_enabled: true,
        ..Wave3Spec::default()
    })
    .await;

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
    let cron_task = fixture.register_cron_task("0 3 * * *", "mw-policy-close");
    let cron_handle_before = pool.get_client("cron").expect("cron 必须已连接");
    // 期望的可见性计数一律**从注册表派生**（不硬编码名字 / 条数）：注册表增删工具时本用例自动跟随。
    let cron_declared = mw_declared_tool_count("cron");
    let web_declared = mw_declared_tool_count("web");
    let artifact_declared = mw_declared_tool_count("artifact");
    assert_eq!(cron_declared, 3, "用例前提（真值表数字）：cron 三工具");

    let mut cells = 0usize;
    for cron_open in [true, false] {
        cells += 1;
        let mut disabled: HashSet<String> = HashSet::new();
        if !cron_open {
            disabled.insert("CronMiddleware".to_string());
        }
        let disabled_list: Vec<&str> = disabled.iter().map(String::as_str).collect();
        let closed = closed_instances(&disabled);
        let label = format!("cron_key={cron_open}");

        // ── 维①：工具可见性（每 turn 投影）────────────────────────────────────────
        let projection = fixture.projections(&disabled_list);
        let cron_tools = mw_instance_tools(&projection, "cron");
        assert_eq!(
            cron_tools.len(),
            if cron_open { cron_declared } else { 0 },
            "[{label}] cron 工具可见性必须随 CronMiddleware 键变化: {cron_tools:?}"
        );
        assert_eq!(
            mw_instance_tools(&projection, "web").len(),
            web_declared,
            "[{label}] 其它 builtin 实例不得受 Cron 策略键影响"
        );
        assert_eq!(
            mw_instance_tools(&projection, "artifact").len(),
            artifact_declared,
            "[{label}] artifact 不得受策略键影响"
        );
        // 注册表声明 direct: false 的 cron 工具一律 deferred。
        let direct_of = |name: &str| -> Option<bool> {
            projection
                .iter()
                .find(|candidate| candidate.name() == name)
                .map(|candidate| candidate.is_direct())
        };
        for tool in cron_tools.iter() {
            assert_eq!(
                direct_of(tool),
                Some(false),
                "[{label}] cron 工具必须 deferred（IF-D13）: {tool}"
            );
        }
        // 裸名不得回流（effective name 之外没有第二条可见路径）。
        for bare in ["cron_register", "cron_list", "cron_remove"] {
            assert!(
                !projection.iter().any(|tool| tool.name() == bare),
                "[{label}] 不得出现裸名 {bare}"
            );
        }

        // ── 维②：tick（1 driver，策略关闭不得停）──────────────────────────────────
        assert_eq!(
            pool.builtin_tick_is_finished("cron"),
            Some(false),
            "[{label}] 策略关闭不得让 cron 的 tick 结束（TickGuard 必须仍在运行）"
        );
        fixture
            .await_armed_trigger(&cron_task, "mw-policy-close")
            .await;

        // ── 维③：readiness（cron ready，不变）────────────────────────────────────
        assert!(
            matches!(
                *pool.init_status.read(),
                McpInitStatus::Ready { total } if total == BUILTIN_MCP_INSTANCES.len()
            ),
            "[{label}] 池的 readiness 收口不得因策略关闭变化，实际 {:?}",
            pool.init_status.read()
        );
        let handle = pool
            .get_client("cron")
            .unwrap_or_else(|| panic!("[{label}] cron 必须仍已连接"));
        assert!(
            matches!(handle.status, ClientStatus::Connected),
            "[{label}] cron 必须仍 Connected，实际 {:?}",
            handle.status
        );
        assert!(
            pool.discovery_evidence("cron")
                .is_some_and(|evidence| evidence.is_complete()),
            "[{label}] cron 的发现证据必须仍完整（不撤 ready）"
        );
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

        // ── 维④：物理生命周期（handler / tick / pool 保留）────────────────────────
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
        assert_eq!(
            fixture.cron_task_count(),
            1,
            "[{label}] 组合根 scheduler 里的既有注册任务不得被策略关闭销毁"
        );

        println!(
            "[MW policy-close] {label} | visible(cron={},web={web_declared},artifact={artifact_declared},deferred=true) \
             | tick(driver=1,trigger=1) | ready(connected=1/1,pool=Ready{{{}}},gate=Ok) \
             | phys(tasks={},handler=retained,pool=same)",
            cron_tools.len(),
            BUILTIN_MCP_INSTANCES.len(),
            BUILTIN_MCP_INSTANCES.len(),
        );
    }
    assert_eq!(cells, 2, "必须逐格覆盖 Cron 开/关 = 2 个策略组合");

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

    // ── 「cron 工具仍可见可调用」与「策略关闭不是物理销毁」───────────────────────
    // `CronMiddleware=false`（实例键）：**投影**归零，但物理 handler 仍在（raw typed bridge
    // 仍可调用成功）——策略关闭是投影关闭，不是物理销毁。
    let cron_off = ["CronMiddleware"];
    let projection = fixture.projections(&cron_off);
    assert!(
        mw_instance_tools(&projection, "cron").is_empty(),
        "实例键关闭后 cron 工具必须从投影中消失"
    );
    let invocation_fixture =
        invocation_fixture::InvocationFixture::new("mcp__cron__cron_list", &[json!({})]).await;
    let text = tokio::time::timeout(
        MW_BOUND,
        fixture
            .typed_bridge("mcp__cron__cron_list")
            .invoke(json!({}), invocation_fixture.context(0)),
    )
    .await
    .expect("策略关闭后 raw bridge 调用仍必须在有界等待内返回")
    .expect("策略关闭不得物理销毁 handler：raw typed bridge 仍必须成功");
    assert!(
        text.contains("mw-policy-close"),
        "策略关闭只关投影：handler → cron 工具 → 注入 scheduler 这条链必须仍然完好，实际: {text}"
    );
    println!(
        "[MW cron-instance-closed] visible=false | raw_bridge_call=ok | phys(handler=retained,tasks=4)"
    );

    fixture.shutdown().await;
}
