use super::*;

// A. 真实启动路径（生产 loader + 生产 transport + 生产 handler）
// ══════════════════════════════════════════════════════════════════════════════════

/// 生产启动路径：**四个**已实现 builtin 实例（web / artifact / cron / lsp）经真实 loader
/// （step 6.5 默认层）落地为 **Connected** 句柄，live `tools/list` 的工具清单等于注册表声明，
/// 且配置侧声明与注册表一致。
///
/// 函数名保留 H-02 期的历史命名（主 plan §6 的具名闸门命令按字面引用它），实例集合一律从
/// 注册表派生——注册表增删实例时本用例自动跟随，不硬编码「两个」。
#[tokio::test]
async fn production_startup_path_connects_both_builtin_instances() {
    let fixture = StartupFixture::start().await;
    let pool = Arc::clone(&fixture.pool);

    assert_eq!(
        pool.builtin_task_count(),
        BUILTIN_MCP_INSTANCES.len(),
        "每个 builtin 实例必须登记一条 server task（关闭时有归属）"
    );
    for instance in BUILTIN_MCP_INSTANCES {
        let handle = pool
            .get_client(instance.name)
            .unwrap_or_else(|| panic!("{} 必须完成连接", instance.name));
        assert!(
            matches!(handle.status, ClientStatus::Connected),
            "{} 必须经同一条处理链提交 Connected，实际: {:?}",
            instance.name,
            handle.status
        );
        let peer_info = handle
            .peer
            .as_ref()
            .and_then(|peer| peer.peer_info())
            .unwrap_or_else(|| panic!("{} 的 modern 握手必须留下 peer_info", instance.name));
        assert!(
            peer_info.server_info.is_some(),
            "{} 必须协商出 server_info（真实 handler 的 get_info）",
            instance.name
        );

        let declared: Vec<&str> = instance
            .tools
            .iter()
            .map(|tool| tool.original_name)
            .collect();
        let served: Vec<&str> = handle.tools.iter().map(|tool| tool.name.as_ref()).collect();
        assert_eq!(
            served, declared,
            "{} 的 live tools/list 必须按声明顺序返回注册表工具",
            instance.name
        );
        assert!(
            matches!(handle.source, Some(ConfigSource::Builtin { .. })),
            "{} 的句柄必须保留 builtin 身份",
            instance.name
        );
        assert!(handle.url.is_none(), "builtin 实例无 URL / 无凭据");

        let config = pool
            .configs
            .read()
            .get(instance.name)
            .cloned()
            .unwrap_or_else(|| panic!("{} 必须写入 pool.configs", instance.name));
        assert_eq!(
            config.system_mcp,
            Some(true),
            "{} 必须是 system 依赖（IF-D9）",
            instance.name
        );
        assert_eq!(
            config.system_mcp_tools.as_deref(),
            Some(declared_direct_original_names(instance).as_slice()),
            "{} 的 system_mcp_tools 必须等于声明 direct 集合（A5/A17）",
            instance.name
        );
        assert!(
            pool.discovery_evidence(instance.name)
                .is_some_and(|evidence| evidence.is_complete()),
            "{} 必须提交可核对的完整发现证据",
            instance.name
        );
    }
    assert!(
        matches!(
            *pool.init_status.read(),
            McpInitStatus::Ready { total } if total == BUILTIN_MCP_INSTANCES.len()
        ),
        "收口必须是 Ready{{total:{}}}（全部注册表实例都真的连上，不是「部分成功也 ready」），实际: {:?}",
        BUILTIN_MCP_INSTANCES.len(),
        pool.init_status.read()
    );

    fixture.shutdown().await;
}

/// A32 的**单 spawn 点**：`tick_enabled = true` 的 cron 上下文经
/// [`McpClientPool::spawn_builtin_transport`] 装配后，tick 真的在驱动同一个 scheduler
/// （到期任务被送出 `CronTrigger`），且本代关闭后**不再有驱动**（两个 tick 周期内无新触发）。
///
/// 与 `mcp::builtin::cron::tests::tick_shutdown_joins_task_and_stops_triggers` 的分工：那条
/// 直接消费 `TickGuard` API（驱动本体与 join 顺序），本用例证明 **pool 的唯一 spawn 点真的
/// 挂上了它**（`cron.rs` 的 handler 内没有 tick，若 pool 不挂，本用例的正控必然收不到触发）。
///
/// 证据形态是事件/超时驱动：正控以 `timeout(5s, recv())` 等到 `CronTrigger`；反控把任务
/// 再次置为到期后等一个 `> 2× tick 周期` 的窗口（`timeout` 到期即证据），不用 `sleep` 当结论。
/// 注册表达式取每日一次（`0 3 * * *`）：触发后重算的下次时间远离观测窗口，正控不会串到反控。
#[tokio::test]
async fn pool_spawn_point_drives_cron_tick_and_stops_on_generation_close() {
    let (trigger_tx, mut triggers) = tokio::sync::mpsc::unbounded_channel();
    let scheduler = Arc::new(parking_lot::Mutex::new(CronScheduler::new(trigger_tx)));
    let prompt = "tick 驱动证据";
    let task_id = scheduler
        .lock()
        .register("0 3 * * *", prompt)
        .expect("夹具注册必须成功");
    assert!(
        scheduler.lock().force_next_fire_to_past(&task_id),
        "正控前置：任务必须存在"
    );

    let pool = Arc::new(McpClientPool::new_empty());
    pool.set_builtin_instance_context(Arc::new(BuiltinInstanceContext::new(".").with_cron(
        CronInstanceInput {
            scheduler: Arc::clone(&scheduler),
            tick_enabled: true,
        },
    )))
    .expect("首次注入上下文必须成功");

    let transport = pool
        .spawn_builtin_transport("cron")
        .expect("cron 必须已接线（dispatch 覆盖四个已实现实例）");
    let (io, supervisor) = transport.into_parts();
    // client 半边走生产握手（同进程链路），保证本代真的进入服务而不是空转。
    let mut service = serve_client_auto(io, None, &pool.capability_profile, HANDSHAKE_TIMEOUT)
        .await
        .expect("builtin 握手不得超时（同进程链路）")
        .expect("builtin 握手不得失败");

    // ① 正控：pool 挂上的 tick 把到期任务带过来（handler 内没有 tick，触发只能来自 pool）。
    let fired = tokio::time::timeout(Duration::from_secs(5), triggers.recv())
        .await
        .expect("正控：pool 装配的 tick 必须在一个 interval 内送出 CronTrigger（否则本用例空转）")
        .expect("观测通道必须存活");
    assert_eq!(fired.task_id, task_id, "触发必须来自夹具注册的任务");
    assert_eq!(fired.prompt, prompt, "触发必须携带注册时的 prompt");

    // ② 关闭本代：先停 tick、再收敛 server task（冻结顺序）。
    let _ = service.close_with_timeout(CLOSE_TIMEOUT).await;
    let outcome = supervisor.close(BUILTIN_CONVERGE_TIMEOUT).await;
    assert!(
        matches!(outcome.tick, TickCloseOutcome::Joined),
        "tick 必须在有界等待内 join 完成（abort 不是正常路径），实际: {:?}",
        outcome.tick
    );
    assert!(
        matches!(outcome.server, BuiltinServerExit::Quit(_)),
        "server task 必须靠 EOF 自然收敛，实际: {:?}",
        outcome.server
    );

    // ③ 反控：已关闭的一代不再驱动 scheduler —— 再次把任务置为到期，> 2× tick 周期内
    //    不得有任何新触发（窗口到期即证据；不用 sleep 作结论）。
    while triggers.try_recv().is_ok() {} // 排空关闭前的在途触发（不属于反控窗口）
    assert!(
        scheduler.lock().force_next_fire_to_past(&task_id),
        "反控前置：任务必须仍存在"
    );
    let quiet_window = BUILTIN_TICK_INTERVAL * 2 + Duration::from_millis(250);
    assert!(
        quiet_window > BUILTIN_TICK_INTERVAL * 2,
        "反控窗口必须 > 2× tick 周期，否则本用例的结论不成立"
    );
    let unexpected = tokio::time::timeout(quiet_window, triggers.recv()).await;
    assert!(
        unexpected.is_err(),
        "已关闭的一代不得再驱动 scheduler（到期任务在无驱动时不得自行触发），实际收到: {:?}",
        unexpected.map(|received| received.map(|trigger| trigger.task_id))
    );
}

/// `transport_type` 三分类（IF-D11 / sub-plan H §6.4）：两个 builtin 实例在面板投影里
/// 都必须报 `"builtin"`（不是 stdio，也不是 http）。
#[tokio::test]
async fn builtin_instances_report_builtin_transport_type() {
    let fixture = StartupFixture::start().await;
    let infos = fixture.pool.all_server_infos();

    for instance in BUILTIN_MCP_INSTANCES {
        let info = infos
            .iter()
            .find(|info| info.name == instance.name)
            .unwrap_or_else(|| panic!("面板投影必须含 {}", instance.name));
        assert_eq!(
            info.transport_type, "builtin",
            "{} 的 transport_type 必须是 builtin 分类，实际: {}",
            instance.name, info.transport_type
        );
    }

    fixture.shutdown().await;
}

/// 关闭矩阵**面①**（启动提交的必需工具选择）：真实 ready 的 pool 上，闸门候选的
/// required 与 direct 集合都必须等于注册表声明（逐实例逐工具，含双层身份）。
#[tokio::test]
async fn startup_gate_stages_declared_direct_tools_and_required_set() {
    let fixture = StartupFixture::start().await;
    let middleware = McpMiddleware::new(Arc::clone(&fixture.pool));

    let mut probe = StartupProbe::default();
    Middleware::before_react_start(&middleware, &mut probe)
        .await
        .expect("两个 builtin 实例 ready 后闸门必须放行");
    assert_eq!(probe.stage_calls, 1, "一次准入只提交一个候选");
    let update = probe.staged.expect("System 依赖就绪必须提交候选");

    let expected_required: Vec<(String, String, String)> = BUILTIN_MCP_INSTANCES
        .iter()
        .flat_map(|instance| {
            instance
                .tools
                .iter()
                .filter(|tool| tool.direct)
                .map(move |tool| {
                    (
                        instance.name.to_string(),
                        tool.original_name.to_string(),
                        tool.effective_name.to_string(),
                    )
                })
        })
        .collect();
    let mut actual_required: Vec<(String, String, String)> = update
        .required
        .iter()
        .map(|required: &StartupRequiredTool| {
            (
                required.server_name.clone(),
                required.original_tool_name.clone(),
                required.effective_tool_name.clone(),
            )
        })
        .collect();
    actual_required.sort();
    let mut expected_required_sorted = expected_required;
    expected_required_sorted.sort();
    assert_eq!(
        actual_required, expected_required_sorted,
        "候选的必需工具身份必须逐项等于注册表声明（原始名与 effective name 双层）"
    );
    assert_eq!(
        direct_names_of(update.tools.iter().map(|tool| tool.as_ref())),
        all_direct_effective_names(),
        "候选内 direct 集合必须等于注册表声明的 direct 工具集合"
    );

    fixture.shutdown().await;
}

/// 关闭矩阵（IF-D10 面①/②/③/④）：`WebMiddleware` / `ArtifactMiddleware` / 两者 /
/// `McpMiddleware` 四种输入下，四个可观察面同时变化。
///
/// 面① = 闸门候选的 required 与 direct 集合；面② = `McpMiddleware::collect_tools`；
/// 面③ = `open_builtin_bridges`（`assembly/preparation.rs` 的 `parent_tools` 与
/// `assembly/workflow.rs` 的 `builtin_tools` 调用的是**同一个** helper）；面④ =
/// 生产 workflow 工厂（`default_workflow_middleware_factory_with_pool`）的 `build_tools`。
///
/// `McpMiddleware=false` 时主链不构造 MCP 槽位：该输入的面①②③属链级，由
/// `assembly::tests` 的同一矩阵覆盖（主 plan §8 第 11 行的命令清单）；本文件断言它
/// 在**本层**可见的两件事——关闭键不进入 builtin 关闭集、workflow 面彻底没有 MCP 工具。
#[tokio::test]
async fn closure_matrix_four_faces_on_real_builtin_pool() {
    let fixture = StartupFixture::start().await;
    let pool = Arc::clone(&fixture.pool);
    let cwd = fixture.project.to_string_lossy().into_owned();

    let bare_names: Vec<&'static str> = BUILTIN_MCP_INSTANCES
        .iter()
        .flat_map(|instance| {
            instance
                .tools
                .iter()
                .filter(|tool| !tool.direct)
                .map(|tool| tool.original_name)
        })
        .collect();

    let cases: Vec<Vec<String>> = vec![
        vec!["WebMiddleware".to_string()],
        vec!["ArtifactMiddleware".to_string()],
        vec![
            "WebMiddleware".to_string(),
            "ArtifactMiddleware".to_string(),
        ],
    ];
    for disabled_list in cases {
        let disabled: HashSet<String> = disabled_list.iter().cloned().collect();
        let closed = closed_instances(&disabled);
        assert!(
            !closed.is_empty(),
            "合法策略键必须映射到至少一个关闭实例: {disabled_list:?}"
        );

        let open_instances: Vec<&BuiltinMcpInstance> = BUILTIN_MCP_INSTANCES
            .iter()
            .filter(|instance| !closed.contains(instance.name))
            .collect();
        // 面①/③的 direct 面 = 未关闭实例**声明为 direct** 的工具（cron / lsp 声明
        // `direct: false`，因此不在其中——`direct_names_of` 与 `open_builtin_bridges`
        // 都只保留 direct）。
        let expected_open_direct: Vec<String> = sorted(
            open_instances
                .iter()
                .flat_map(|instance| declared_direct_effective_names(instance))
                .collect(),
        );
        // 面①的 required 只由「有必需工具的实例」产出（`required` 来自
        // `SystemMcpConfig::system_mcp_tools`，空集合 = 只要求 ready、不做工具校验，见
        // `mcp/system_tools.rs` 的 `prepare_system_tools` 文档）：cron / lsp 的
        // `system_mcp_tools` 是空集，因此不出现在 required 里。
        let mut expected_servers: Vec<String> = open_instances
            .iter()
            .filter(|instance| !declared_direct_effective_names(instance).is_empty())
            .map(|instance| instance.name.to_string())
            .collect();
        expected_servers.sort();

        // 面①：闸门候选。
        let middleware = McpMiddleware::new(Arc::clone(&pool))
            .with_tool_pool(Arc::clone(&pool))
            .with_builtin_closures(closed.clone());
        let mut probe = StartupProbe::default();
        Middleware::before_react_start(&middleware, &mut probe)
            .await
            .unwrap_or_else(|error| {
                panic!("[{disabled_list:?}] 关闭实例后闸门仍必须放行: {error}")
            });
        let update = probe.staged.expect("仍有 System 依赖时必须提交候选");
        let mut staged_servers: Vec<String> = update
            .required
            .iter()
            .map(|required| required.server_name.clone())
            .collect();
        staged_servers.sort();
        staged_servers.dedup();
        assert_eq!(
            staged_servers, expected_servers,
            "[{disabled_list:?}] 面① required 只允许未关闭且有必需工具的实例"
        );
        assert_eq!(
            direct_names_of(update.tools.iter().map(|tool| tool.as_ref())),
            expected_open_direct,
            "[{disabled_list:?}] 面① direct 集合必须随关闭集收缩"
        );

        // 面②：deferred 目录（链工具集合）。
        let mut collected = tool_names(&middleware.collect_tools(&cwd));
        collected.retain(|name| !name.starts_with("mcp_read_resource") && name != "DiscoverMCP");
        for instance in BUILTIN_MCP_INSTANCES {
            for tool in declared_effective_names(instance) {
                assert_eq!(
                    collected.contains(&tool.to_string()),
                    !closed.contains(instance.name),
                    "[{disabled_list:?}] 面② {tool} 的出现必须与关闭集一致: {collected:?}"
                );
            }
        }
        for bare in &bare_names {
            assert!(
                !collected.contains(&bare.to_string()),
                "[{disabled_list:?}] 面② 不得出现裸名 {bare}: {collected:?}"
            );
        }

        // 面③：`open_builtin_bridges`（parent_tools 与 workflow builtin_tools 的共同 helper）。
        let parent_face = tool_names(&open_builtin_bridges(&pool, &disabled));
        assert_eq!(
            sorted(parent_face.clone()),
            expected_open_direct,
            "[{disabled_list:?}] 面③ parent_tools 提供面必须与 direct 集合一致"
        );

        // 面④：生产 workflow 工厂。
        let workflow = default_workflow_middleware_factory_with_pool(Some(Arc::clone(&pool)))
            .build_tools(&cwd, &disabled, None, None);
        let workflow_names = tool_names(&workflow);
        for name in &expected_open_direct {
            assert!(
                workflow_names.contains(name),
                "[{disabled_list:?}] 面④ workflow agent 工具列表必须含 {name}: {workflow_names:?}"
            );
        }
        for instance in BUILTIN_MCP_INSTANCES
            .iter()
            .filter(|i| closed.contains(i.name))
        {
            for tool in declared_effective_names(instance) {
                assert!(
                    !workflow_names.contains(&tool.to_string()),
                    "[{disabled_list:?}] 面④ 关闭实例的 {tool} 不得出现在 workflow: {workflow_names:?}"
                );
            }
        }
        for bare in &bare_names {
            assert!(
                !workflow_names.contains(&bare.to_string()),
                "[{disabled_list:?}] 面④ 不得出现裸名 {bare}: {workflow_names:?}"
            );
        }
    }

    // `McpMiddleware=false`：链槽位键不是 builtin 策略键。
    let chain_off: HashSet<String> = ["McpMiddleware".to_string()].into_iter().collect();
    assert!(
        closed_instances(&chain_off).is_empty(),
        "McpMiddleware 是链槽位键，不是 builtin 实例策略键（两表语义不重叠，A7）"
    );
    let workflow_off = default_workflow_middleware_factory_with_pool(Some(Arc::clone(&pool)))
        .build_tools(&cwd, &chain_off, None, None);
    let workflow_off_names = tool_names(&workflow_off);
    assert!(
        !workflow_off_names
            .iter()
            .any(|name| name.starts_with("mcp__")),
        "McpMiddleware 关闭后 workflow 面不得含任何 MCP 工具: {workflow_off_names:?}"
    );

    fixture.shutdown().await;
}

/// 无 orphan（pool 归属面）：关闭后 builtin task 表排空、pool 关闭报告完整。
///
/// 「server task 靠 EOF 自然收敛（`Quit`，不是 abort）」这条更强的证据由两条路径覆盖：
/// [`TappedLink::shutdown`]（每条线路用例的收尾断言）与 `mcp::builtin::runtime` 的
/// 收敛/超时用例。本用例只断言 pool 级的排空（生产归属下的关闭语义）。
#[tokio::test]
async fn shutdown_drains_builtin_tasks_without_orphan() {
    let fixture = StartupFixture::start().await;
    assert_eq!(
        fixture.pool.builtin_task_count(),
        BUILTIN_MCP_INSTANCES.len()
    );

    fixture.shutdown().await;
}

/// A13 ①②：两个实例是**不同的**连接对象（句柄非同源、连接键不同、对端 server 实例
/// 不同），且重连 `web` 只推进 `web` 的代际，`artifact` 的句柄与代际不变。
#[tokio::test]
async fn builtin_instances_are_distinct_and_reconnect_touches_one_generation() {
    let fixture = StartupFixture::start().await;
    let pool = Arc::clone(&fixture.pool);

    let web_before = pool.get_client("web").expect("web 必须已连接");
    let artifact_before = pool.get_client("artifact").expect("artifact 必须已连接");
    assert!(
        !Arc::ptr_eq(&web_before, &artifact_before),
        "两个实例的 Arc<McpClientHandle> 不得是同一份"
    );
    assert_ne!(
        McpConnectionKey::static_server("web"),
        McpConnectionKey::static_server("artifact"),
        "scoped connection identity 不得相同（pool 按 server name 派生，oauth.rs 同源）"
    );
    let server_info_name = |handle: &Arc<McpClientHandle>| {
        handle
            .peer
            .as_ref()
            .and_then(|peer| peer.peer_info())
            .and_then(|info| {
                info.server_info
                    .as_ref()
                    .map(|server| server.name.to_string())
            })
    };
    assert_ne!(
        server_info_name(&web_before),
        server_info_name(&artifact_before),
        "两条链路必须握到不同的 server 实例（各自的 rmcp ServerInfo 不同）"
    );

    let web_generation_before = pool.handle_generation(&web_before);
    let artifact_generation_before = pool.handle_generation(&artifact_before);
    pool.reconnect("web", None)
        .await
        .expect("builtin 实例必须能重连");

    let web_after = pool.get_client("web").expect("重连后必须留下新句柄");
    let artifact_after = pool.get_client("artifact").expect("artifact 必须仍在");
    assert!(
        !Arc::ptr_eq(&web_before, &web_after),
        "重连必须换新句柄（旧代证据不得继续有效）"
    );
    assert!(
        pool.handle_generation(&web_after) > web_generation_before,
        "重连后 web 的代际必须递增: {web_generation_before} -> {}",
        pool.handle_generation(&web_after)
    );
    assert!(
        Arc::ptr_eq(&artifact_before, &artifact_after),
        "重连 web 不得触碰 artifact 的句柄"
    );
    assert_eq!(
        pool.handle_generation(&artifact_after),
        artifact_generation_before,
        "重连 web 不得推进 artifact 的代际"
    );
    assert!(
        matches!(web_after.status, ClientStatus::Connected),
        "重连后 web 必须重新 Connected，实际: {:?}",
        web_after.status
    );
    assert_eq!(
        pool.builtin_task_count(),
        BUILTIN_MCP_INSTANCES.len(),
        "重连只替换本实例的 task（不新增、不残留）"
    );

    fixture.shutdown().await;
}

/// A13 ③：关闭 `web` 后 `artifact` 仍完成一次**真实** `tools/call`（经生产 handler），
/// 且能力面按关闭集收缩。
///
/// 该往返用真实 `ArtifactMcpServer`（`ArtifactTool::new(cwd)`）：不存在的文件在**网络之前**
/// 失败，因此结果是 IF-D14 的固定规则文本（错误形态、无路径 / 无凭据泄漏），而**往返本身**
/// 是完整的（请求真的上了链路、结果真的回来了）。成功上载形态由 `mcp::builtin::artifact`
/// 用注入客户端覆盖（需要网络或本地桩，不在本文件重复）。
#[tokio::test]
async fn closing_web_keeps_artifact_capability_and_real_call() {
    let fixture = StartupFixture::start().await;
    let pool = Arc::clone(&fixture.pool);
    let cwd = fixture.project.to_string_lossy().into_owned();
    let disabled: HashSet<String> = ["WebMiddleware".to_string()].into_iter().collect();
    let closed = closed_instances(&disabled);
    assert_eq!(
        closed,
        std::collections::BTreeSet::from(["web".to_string()])
    );

    // 能力面：目录与 parent_tools 提供面都只剩 artifact。
    let middleware = McpMiddleware::new(Arc::clone(&pool))
        .with_tool_pool(Arc::clone(&pool))
        .with_builtin_closures(closed.clone());
    let collected = tool_names(&middleware.collect_tools(&cwd));
    let web_tools = declared_effective_names(find("web").expect("web 已实现"));
    let artifact_tools = declared_effective_names(find("artifact").expect("artifact 已实现"));
    for tool in &web_tools {
        assert!(
            !collected.contains(&tool.to_string()),
            "关闭 WebMiddleware 后能力面不得含 {tool}: {collected:?}"
        );
    }
    for tool in &artifact_tools {
        assert!(
            collected.contains(&tool.to_string()),
            "另一个实例必须不受影响: {tool} 不在 {collected:?}"
        );
    }

    // 真实往返：经 parent_tools / workflow 面的同一 helper 取 bridge。
    //
    // 前提变更（wave 3）：dispatch 的 `workspace` arm 已接线，`StartupFixture` 的真实
    // `run_initialize` 链路里 workspace 桥**真的建立**（接线前该实例走 `HandlerNotWired`
    // ⇒ warn + `insert_failed` + `continue`，因此这里曾恰为 artifact 一项）。workspace 的
    // handler 由 dispatch **无条件**装配、7 项一律 direct，`BuiltinInstanceContext::workspace`
    // 为 `None` 只是「可见但退化」（AW3-11），不改变工具面——所以期望集合是「未关闭实例
    // 声明的全部 direct」，由注册表派生（与 `closure_matrix_four_faces_on_real_builtin_pool`
    // 面③同口径），不在此处第二份硬编码实例名。
    let bridges = open_builtin_bridges(&pool, &disabled);
    let bridge_names = tool_names(&bridges);
    // 关闭集**只**作用于被点名实例：web 的工具一个不留（对**实际**产出断言，不只看期望）。
    for tool in &web_tools {
        assert!(
            !bridge_names.contains(tool),
            "关闭 WebMiddleware 后 web 的 direct 不得进入提供面: {tool} 在 {bridge_names:?}"
        );
    }
    let expected_open_direct = sorted(
        BUILTIN_MCP_INSTANCES
            .iter()
            .filter(|instance| !closed.contains(instance.name))
            .flat_map(declared_direct_effective_names)
            .collect(),
    );
    assert_eq!(
        sorted(bridge_names),
        expected_open_direct,
        "关闭 web 后 direct 提供面必须恰为未关闭实例的全部 direct（artifact + workspace）"
    );
    let bridge = bridges
        .into_iter()
        .find(|bridge| bridge.name() == artifact_tools[0])
        .expect("artifact 的 direct bridge 必须存在");
    assert!(
        bridge.is_direct(),
        "artifact 声明的 direct 必须生效（IF-D13）"
    );

    let error = bridge
        .invoke(
            json!({ "file_path": "missing-artifact-fixture.html" }),
            ToolContext::new(&[], &cwd),
        )
        .await
        .expect_err("不存在的文件必须在建立网络请求之前失败");
    let message = error.to_string();
    assert!(
        message.contains("withheld by policy"),
        "结果必须是 IF-D14 的固定规则文本，实际: {message}"
    );
    assert!(
        !message.contains(&cwd),
        "IF-D14 文本不得泄漏路径，实际: {message}"
    );

    fixture.shutdown().await;
}

// ══════════════════════════════════════════════════════════════════════════════════
