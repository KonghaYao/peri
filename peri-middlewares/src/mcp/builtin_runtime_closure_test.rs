use super::*;

// ─── 用例 2：四类关闭来源分列（各有一份独立可观察签名）──────────────────────────

/// 同一套观测面读出的「关闭签名」：四类关闭必须两两不同，且**只有策略关闭**允许
/// 保留 handler / tick / readiness。
#[derive(Debug, Clone, PartialEq, Eq)]
struct CloseSignature {
    /// 该实例在池里的连接记录形态：`connected` / `disabled_handle` / `absent`。
    connection: &'static str,
    /// 池的 builtin 代监督者表是否有该实例条目（handler / server task 是否存在）。
    task_registered: bool,
    /// `builtin_tick_is_finished`：`Some(false)` = tick 在跑；`Some(true)` = 无 tick 或已停；
    /// `None` = 无该实例条目（无条目与「无 tick」是两件事）。
    tick: Option<bool>,
    /// 该实例是否仍宣称 ready（发现证据完整）。
    ready: bool,
    /// 配置层是否仍保留该实例条目（builtin 身份）。
    config_entry: bool,
    /// 本 turn 投影里该实例的工具是否可见。
    projected: bool,
}

/// 主 plan §8 第 10 行的四类关闭**分列**（`disabled: true` / MetaHarness `policy_key=false` /
/// `PERI_MCP_BUILTIN=off` / 物理 `close_builtin_task`）。
///
/// 每一类都在真宿主装配形态的 pool 上读同一套观测面，得到一份**签名**；断言：
///
/// 1. 四份签名**两两不同**（互不等价）；
/// 2. `policy_key=false` 是唯一保留 handler / tick / readiness 的关闭来源（策略关闭专属），
///    且只作用在本 turn 投影上——投影归零时 raw typed bridge 仍能调用成功（cron）；
/// 3. `PERI_MCP_BUILTIN=off` 是**零注入**（配置层就不存在 builtin 条目），因此不得与
///    `disabled: true`（配置条目保留、注册为 `Disabled`）混为一谈；
/// 4. 非法关闭片段（`disabled + system_mcp`）在**加载期**即失败，且不留下任何连接/task；
/// 5. 物理 `close_builtin_task` 经 `supervisor.close(BUILTIN_CONVERGE_TIMEOUT)` 先 tick
///    cancel+join 再收敛 server task；B 实例不受影响、组合根 scheduler / pool 不因 Arc
///    释放而关闭。
///
/// 与 sub-plan-v §5 的一处**实测差异**（以代码为事实源，登记不掩盖）：纯
/// `close_builtin_task` **不**撤销该实例的 `ready` 证据，也不改句柄状态——"对应实例不再 ready"
/// 的形态属于宿主级路径（`remove_server` / `set_disabled` / `reconnect`，它们把物理收敛与
/// 句柄/证据失效组合在一起）。本用例按实测签名断言 ready 仍为真，并在报告里登记该差异。
#[tokio::test]
async fn four_close_sources_are_distinct() {
    let mut signatures: Vec<(&'static str, CloseSignature)> = Vec::new();

    // ── ① MCP 配置 `disabled: true`：跳过连接、无 handler/tick、不宣称 ready ──────
    let fixture = Wave3Fixture::start(Wave3Spec {
        tick_enabled: true,
        project_mcp_json: Some(r#"{"mcpServers":{"artifact":{"disabled":true}}}"#),
        builtin_env: None,
    })
    .await;
    {
        let pool = Arc::clone(&fixture.pool);
        let handle = pool
            .get_client("artifact")
            .expect("disabled 实例仍必须留下句柄记录（面板语义：注册为 Disabled，不是消失）");
        assert!(
            matches!(handle.status, ClientStatus::Disabled),
            "disabled 实例必须注册为 Disabled（不是 Failed / 不是 Connected），实际 {:?}",
            handle.status
        );
        assert!(
            handle.peer.is_none(),
            "disabled 实例不得建立连接（无 peer）"
        );
        assert!(
            handle.tools.is_empty(),
            "disabled 实例不得有工具面（跳过连接 ⇒ 没有 tools/list）"
        );
        assert_eq!(
            pool.builtin_task_count(),
            BUILTIN_MCP_INSTANCES.len() - 1,
            "disabled 实例不得构造 handler/server task（task 表少一项）"
        );
        assert_eq!(
            pool.builtin_tick_is_finished("artifact"),
            None,
            "无条目必须是 None（不是 false）：关闭的实例连 tick 归属都不存在"
        );
        assert!(
            pool.discovery_evidence("artifact").is_none(),
            "disabled 实例不得宣称 ready（无发现证据）"
        );
        let config = pool
            .configs
            .read()
            .get("artifact")
            .cloned()
            .expect("disabled 仍必须保留配置条目（overlay 规则 2 只填 source）");
        assert!(
            matches!(config.source, Some(ConfigSource::Builtin { .. })),
            "保留 builtin 传输身份（Disabled 不是「外部同名 server」）"
        );
        assert_eq!(config.system_mcp, None, "disabled 不得构成 system 依赖");
        assert_eq!(config.system_mcp_tools, None);
        let requirements = pool.system_requirements();
        assert_eq!(
            requirements.len(),
            BUILTIN_MCP_INSTANCES.len() - 1,
            "system 依赖必须排除 disabled 实例: {:?}",
            requirements.iter().map(|r| &r.server).collect::<Vec<_>>()
        );
        assert!(requirements
            .iter()
            .all(|required| required.server != "artifact"));
        assert!(
            matches!(
                *pool.init_status.read(),
                McpInitStatus::Ready { total } if total == BUILTIN_MCP_INSTANCES.len() - 1
            ),
            "connectable 必须排除 disabled 实例，实际 {:?}",
            pool.init_status.read()
        );

        // 不得把 disabled 当策略关闭：关闭集为空，artifact 缺席来自池（句柄无工具）。
        assert!(
            closed_instances(&HashSet::new()).is_empty(),
            "空 MetaHarness 关闭集必须映射到空关闭集"
        );
        let projection = fixture.projections(&[]);
        assert!(
            mw_instance_tools(&projection, "artifact").is_empty(),
            "disabled 实例的工具不得进入投影（原因在池，不在关闭集）"
        );
        assert_eq!(
            mw_instance_tools(&projection, "cron").len(),
            mw_declared_tool_count("cron"),
            "其余实例不受 disabled 影响"
        );
        assert_eq!(
            mw_instance_tools(&projection, "web").len(),
            mw_declared_tool_count("web"),
            "web 也不受影响（关闭只按被点名实例生效）"
        );
        assert_eq!(
            pool.builtin_tick_is_finished("cron"),
            Some(false),
            "其余实例的 tick 必须照常运行（disabled 不是「所有 builtin 停摆」）"
        );
        println!(
            "[MW close-source] disabled=true | connection=disabled_handle | task=false | tick=None \
             | ready=false | config=true | projected=false | others(cron_tools={},web_tools={},cron_tick=live)",
            mw_declared_tool_count("cron"),
            mw_declared_tool_count("web")
        );
        signatures.push((
            "mcp_config_disabled",
            CloseSignature {
                connection: "disabled_handle",
                task_registered: false,
                tick: None,
                ready: false,
                config_entry: true,
                projected: false,
            },
        ));
    }
    fixture.shutdown().await;

    // ── ①b 非法关闭片段（`disabled + system_mcp`）必须加载期报错，且不留任何连接 ────
    let invalid = Wave3Fixture::start(Wave3Spec {
        tick_enabled: true,
        project_mcp_json: Some(
            r#"{"mcpServers":{"artifact":{"disabled":true,"system_mcp":true}}}"#,
        ),
        builtin_env: None,
    })
    .await;
    {
        let pool = Arc::clone(&invalid.pool);
        let status = pool.init_status.read().clone();
        match status {
            McpInitStatus::Failed(message) => {
                assert!(
                    message.contains("artifact")
                        && message
                            .contains("disabled = true cannot be combined with system_mcp = true"),
                    "非法组合必须在加载期以固定文本报错（只含实例名），实际: {message}"
                );
            }
            other => panic!("非法关闭片段必须以 Failed 收口，实际: {other:?}"),
        }
        assert_eq!(
            pool.builtin_task_count(),
            0,
            "加载期失败不得留下任何 builtin server task"
        );
        assert!(
            pool.get_client("artifact").is_none() && pool.get_client("cron").is_none(),
            "加载期失败不得注册任何连接"
        );
        assert!(
            pool.configs.read().is_empty(),
            "加载期失败不得发布配置清单（不产生部分注入）"
        );
        println!(
            "[MW close-source] disabled+system_mcp=illegal | loader=Failed | tasks=0 | clients=0 | configs=0"
        );
    }
    invalid.shutdown().await;

    // ── ② 策略关闭 + ④ 物理关闭（同一夹具顺序执行）──────────────────────────────
    let mut fixture = Wave3Fixture::start(Wave3Spec {
        tick_enabled: true,
        ..Wave3Spec::default()
    })
    .await;
    let pool = Arc::clone(&fixture.pool);
    let cron_task = fixture.register_cron_task("0 3 * * *", "mw-four-sources");
    let policy_disabled = ["CronMiddleware"];
    {
        // ② 投影归零，但 handler / pool / tick / readiness 全部保留。
        let projection = fixture.projections(&policy_disabled);
        assert!(
            mw_instance_tools(&projection, "cron").is_empty(),
            "策略关闭必须让 cron 实例的工具从本 turn 投影中消失"
        );
        assert_eq!(
            pool.builtin_task_count(),
            BUILTIN_MCP_INSTANCES.len(),
            "策略关闭不得销毁任何 builtin 代监督者（handler 保留）"
        );
        assert_eq!(
            pool.builtin_tick_is_finished("cron"),
            Some(false),
            "策略关闭不得停 tick"
        );
        assert!(
            matches!(
                *pool.init_status.read(),
                McpInitStatus::Ready { total } if total == BUILTIN_MCP_INSTANCES.len()
            ),
            "策略关闭不得改变 readiness 收口，实际 {:?}",
            pool.init_status.read()
        );
        assert!(
            pool.discovery_evidence("cron")
                .is_some_and(|evidence| evidence.is_complete()),
            "策略关闭不得撤销 cron 的 ready 证据"
        );
        assert!(
            pool.configs.read().contains_key("cron"),
            "策略关闭不得移除 cron 的配置条目"
        );
        // 投影归零 ≠ 物理销毁：raw typed bridge 仍必须调用成功。
        fixture
            .await_armed_trigger(&cron_task, "mw-four-sources")
            .await;
        let invocation_fixture =
            invocation_fixture::InvocationFixture::new("mcp__cron__cron_list", &[json!({})]).await;
        let cron_text = tokio::time::timeout(
            MW_BOUND,
            fixture
                .typed_bridge("mcp__cron__cron_list")
                .invoke(json!({}), invocation_fixture.context(0)),
        )
        .await
        .expect("策略关闭后 cron 工具调用必须有界返回")
        .expect("策略关闭不得物理销毁 cron handler");
        assert!(
            cron_text.contains("mw-four-sources"),
            "cron handler 必须仍连着**同一份**组合根 scheduler（注册任务可见），实际: {cron_text}"
        );
        println!(
            "[MW close-source] meta_harness_policy_key=false | connection=connected | task=true \
             | tick=Some(false) | ready=true | config=true | projected=false \
             | raw_bridge(cron=ok) | 策略关闭专属：handler/pool/readiness 保留"
        );
        signatures.push((
            "meta_harness_policy_key",
            CloseSignature {
                connection: "connected",
                task_registered: true,
                tick: Some(false),
                ready: true,
                config_entry: true,
                projected: false,
            },
        ));
    }

    // ④ 物理 close（sub-plan-v §5 生命周期表的 `instance close`）：生产顺序 = 先关 client
    //    半边（pool 的 `services` 表，`close_with_timeout`）→ 再 `close_builtin_task`
    //    （supervisor：先 tick cancel+join，再收敛 server task）。两个生产调用点
    //    （`lifecycle.rs` 的 `set_disabled` / `remove_server`、`reconnect.rs`）都是这个顺序。
    let artifact_handle_before = pool.get_client("artifact").expect("artifact 必须仍已连接");
    let web_handle_before = pool.get_client("web").expect("web 必须仍已连接");
    let cron_service = pool.services.lock().remove("cron");
    assert!(
        cron_service.is_some(),
        "生产 pool 必须持有 cron 的 client service（关闭顺序的前提）"
    );
    if let Some(mut service) = cron_service {
        let _ = service.close_with_timeout(CLOSE_TIMEOUT).await;
    }
    let outcome = pool
        .close_builtin_task("cron")
        .await
        .expect("cron 必须有登记在册的代监督者可关闭");
    assert!(
        matches!(outcome.tick, TickCloseOutcome::Joined),
        "物理关闭必须先 cancel 并**有界 join** tick（abort 不是正常路径），实际: {:?}",
        outcome.tick
    );
    assert!(
        matches!(outcome.server, BuiltinServerExit::Quit(_)),
        "server task 必须靠 EOF 自然收敛（不是 abort），实际: {:?}",
        outcome.server
    );
    assert_eq!(
        pool.builtin_task_count(),
        BUILTIN_MCP_INSTANCES.len() - 1,
        "物理关闭必须让代监督者表少一项"
    );
    assert_eq!(
        pool.builtin_tick_is_finished("cron"),
        None,
        "关闭后条目不存在（None）：不再有任何 tick 归属"
    );
    // 已停的一代不再驱动 scheduler（窗口到期即证据）。
    fixture.assert_armed_trigger_absent(&cron_task).await;
    // B 实例（artifact / web）不受影响：句柄同一份、链路仍可服务。
    assert!(
        Arc::ptr_eq(
            &artifact_handle_before,
            &pool.get_client("artifact").expect("artifact 必须在池中")
        ),
        "物理关闭 cron 不得替换 artifact 句柄"
    );
    assert!(
        Arc::ptr_eq(
            &web_handle_before,
            &pool.get_client("web").expect("web 必须在池中")
        ),
        "物理关闭 cron 不得替换 web 句柄"
    );
    let projection = fixture.projections(&[]);
    assert_eq!(
        mw_instance_tools(&projection, "artifact").len(),
        mw_declared_tool_count("artifact"),
        "A 关闭后 B（artifact）的投影必须完整"
    );
    assert_eq!(
        mw_instance_tools(&projection, "web").len(),
        mw_declared_tool_count("web"),
        "A 关闭后 B（web）的投影必须完整"
    );
    // 组合根 scheduler / pool 不因 Arc 释放而关闭。
    let later_task = fixture.register_cron_task("0 4 * * *", "mw-after-physical-close");
    assert!(
        !later_task.is_empty(),
        "组合根 scheduler 必须仍可用（物理 close 不是组合根关闭）"
    );
    assert_eq!(
        fixture.cron_task_count(),
        2,
        "物理 close 不得销毁组合根 scheduler 里的任务数据"
    );
    assert!(
        matches!(*pool.init_status.read(), McpInitStatus::Ready { .. }),
        "单实例物理关闭不得让 pool 生命周期进入终态"
    );
    // 实测签名：纯物理 close 只收敛 task；句柄与 ready 证据**不动**（见用例文档的差异登记）。
    assert!(
        matches!(
            pool.get_client("cron").expect("cron 句柄记录仍在").status,
            ClientStatus::Connected
        ),
        "纯 close_builtin_task 不改句柄状态（宿主级 remove/disabled 另有 handle 失效步骤）"
    );
    assert!(
        pool.discovery_evidence("cron")
            .is_some_and(|evidence| evidence.is_complete()),
        "纯 close_builtin_task 不撤销发现证据（实测；与 sub-plan-v §5 期望的差异见报告）"
    );
    assert_eq!(
        mw_instance_tools(&fixture.projections(&[]), "cron").len(),
        mw_declared_tool_count("cron"),
        "纯物理 close 不动本 turn 投影（句柄与工具表未变）：投影关闭只由策略键驱动"
    );
    // 同一 API 的**边界形态**（登记不掩盖）：只关监督者表、**不**先关 client 半边时，
    // server task 读不到 EOF ⇒ 有界等待到期后 `abort`。上面两个生产调用点都先关 client
    // service，因此正常关闭恒落在 `Quit`；本断言记录「为什么必须先关 client 半」。
    let web_outcome = pool
        .close_builtin_task("web")
        .await
        .expect("web 必须有登记在册的代监督者可关闭");
    assert!(
        matches!(web_outcome.tick, TickCloseOutcome::NotSpawned),
        "web 实例没有 tick 驱动，实际: {:?}",
        web_outcome.tick
    );
    assert!(
        matches!(web_outcome.server, BuiltinServerExit::AbortedAfterTimeout),
        "边界形态：未先关 client 半边时 server task 必须在有界等待后 abort（正常路径恒 Quit），实际: {:?}",
        web_outcome.server
    );
    println!(
        "[MW close-source] physical_close_builtin_task | connection=connected | task=false \
         | tick=None | ready=true(实测:证据不撤) | config=true | projected=true \
         | tick_close=Joined | server_exit=Quit | quiet_window=>{MW_TICK_WINDOW:?}无触发 \
         | B(artifact)=ok | root_scheduler=alive | boundary(web,未先关client)=AbortedAfterTimeout"
    );
    signatures.push((
        "physical_close_builtin_task",
        CloseSignature {
            connection: "connected",
            task_registered: false,
            tick: None,
            ready: true,
            config_entry: true,
            projected: true,
        },
    ));
    fixture.shutdown().await;

    // ── ③ `PERI_MCP_BUILTIN=off`：零注入（配置层就不存在 builtin 条目）────────────
    let off = Wave3Fixture::start(Wave3Spec {
        tick_enabled: true,
        builtin_env: Some("off"),
        ..Wave3Spec::default()
    })
    .await;
    {
        let pool = Arc::clone(&off.pool);
        for instance in BUILTIN_MCP_INSTANCES {
            assert!(
                !pool.configs.read().contains_key(instance.name),
                "off 必须零注入：{} 不得出现在配置清单里",
                instance.name
            );
            assert!(
                pool.get_client(instance.name).is_none(),
                "off 必须零注入：{} 不得有连接记录（连 Disabled 记录都不该有）",
                instance.name
            );
        }
        assert_eq!(
            pool.builtin_task_count(),
            0,
            "off 必须零 builtin transport / handler / tick（task 表为空）"
        );
        assert_eq!(
            pool.builtin_tick_is_finished("cron"),
            None,
            "off 下连 tick 归属都不存在"
        );
        assert!(
            matches!(*pool.init_status.read(), McpInitStatus::Ready { total } if total == 0),
            "off 是「零个 server 的合法完整清单」，不是失败，实际 {:?}",
            pool.init_status.read()
        );
        assert!(
            pool.system_requirements().is_empty(),
            "off 下不得有 system 依赖（不宣称任何 builtin ready）"
        );
        let projection = off.projections(&[]);
        for instance in BUILTIN_MCP_INSTANCES {
            assert!(
                mw_instance_tools(&projection, instance.name).is_empty(),
                "off 下 {} 的工具不得进入投影",
                instance.name
            );
        }
        // 不得把 off 当策略关闭：关闭集为空，且组合根对象仍被注入（零注入 ≠ 对象不存在）。
        assert!(closed_instances(&HashSet::new()).is_empty());
        assert!(
            pool.builtin_instance_context().is_some(),
            "off 只抑制 config 层注入；宿主注入的实例上下文仍在（不得据此推断组合根对象不存在）"
        );
        println!(
            "[MW close-source] PERI_MCP_BUILTIN=off | connection=absent | task=false | tick=None \
             | ready=false | config=false | projected=false | context_injected=true | pool=Ready{{0}}"
        );
        signatures.push((
            "builtin_injection_off",
            CloseSignature {
                connection: "absent",
                task_registered: false,
                tick: None,
                ready: false,
                config_entry: false,
                projected: false,
            },
        ));
    }
    off.shutdown().await;

    // ── 四类签名两两不同（互不等价）─────────────────────────────────────────────
    assert_eq!(signatures.len(), 4, "必须四类关闭各读一份签名");
    for left in 0..signatures.len() {
        for right in (left + 1)..signatures.len() {
            assert_ne!(
                signatures[left].1, signatures[right].1,
                "{} 与 {} 的关闭签名不得等价（四类关闭必须各有独立签名）",
                signatures[left].0, signatures[right].0
            );
        }
    }
    println!(
        "[MW close-sources] distinct=true | {}",
        signatures
            .iter()
            .map(|(name, signature)| format!("{name}=({:?})", signature))
            .collect::<Vec<_>>()
            .join(" ")
    );
}
