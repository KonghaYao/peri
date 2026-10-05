use super::*;

// ── 用例 9：reconnect 单 tick 驱动（§8 第 7 行 / H05 / V 子计划 `[W2 tick]`）──────

/// 主 plan §8 第 7 行（H05 / A2 / A32 / R25 / C-03）在宿主侧的收口断言：
/// **reconnect 换新代后每一代都还有 tick 驱动**（计数观测，不是「未 panic」），
/// **旧代驱动已随代关闭收敛**，且组合根状态跨代保留。
///
/// 观测面：装配注入的**同一份** `CronScheduler`（端口 downcast 的还原点）+ 任务的
/// 1s 粒度 cron 表达式（croner 支持可选秒字段）⇒ 「强制到期后窗口内出现的触发条数」
/// 就是驱动是否在跑的直接投影（`force_due_rounds` 逐轮计数，0 条即失败）。
///
/// ## 可证伪性边界（实测结论，不得含糊）
///
/// 宿主侧**拿不到 tick 调用计数**：增量闭包在 `peri-middlewares`
/// （`mcp::builtin::runtime::TickGuard::spawn`），宿主只能经 `CronSchedulerPort`
/// 观测触发面。而触发面在结构上**无法**证伪「相位错开的第二个驱动」：
/// `CronScheduler::tick` 触发后立即把 `next_fire` 重算到未来（≥ 下一整秒），因此两个
/// 1s 驱动并存时，每周期仍至多 1 次触发（先到者触发、后到者看到未来的 `next_fire`）。
/// 本文件实测两次（报告的反例实验 E1/E2）：在 reconnect 之后、以及 `remove_server` 之后
/// 额外挂一个 1s 驱动（`scheduler.lock().tick()`）——gen2 的窗口计数**没有**翻倍
/// （E1：相位错开 ⇒ 触发面不动），而**关闭后零触发**在两次实验里都立刻变红
/// （E1：2.5s 内 3 条；E2：2.5s 内 2 条）。
///
/// 因此本用例把可证伪的判据放在这三处（其余计数只作观测打印）：
/// ① 每一代都能被「强制到期 → 有界窗口」逼出触发（驱动挂掉 / reconnect 漏挂新代 ⇒ 0 条）；
/// ② reconnect 换新代对象（旧句柄由用例持有 ⇒ `Arc::ptr_eq` 为假不是地址复用）+ 组合根
///    任务表不变；
/// ③ **实例关闭后 >2× interval 零触发** —— 与 reconnect 的第一步（`close_builtin_task`
///    → 代监督者 `close` → tick cancel + join）是同一条收敛路径，本判据能抓住「驱动
///    未 join」（反例实验 E2 实测：关闭后多留一个驱动 ⇒ 2.5s 内 2 条触发 ⇒ 断言红）。
///
/// 直接 tick 计数（`计数冻结` 形态）由 crate 内用例
/// `mcp::builtin::cron::tests::{tick_reconnect_has_single_driver_per_interval,
/// tick_shutdown_joins_task_and_stops_triggers}` 承担（那里能直接数 tick 调用）。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn reconnect_has_single_tick_driver() {
    /// 每代的强制到期轮数（逐轮都必须在窗口内等到触发）。
    const FORCE_ROUNDS: usize = 2;
    /// 单轮窗口：2 × interval（计数观测面；CPU 争用只会推迟 tick，不会提前 ⇒ 下界保守）。
    const FORCE_WINDOW_MS: u64 = 2_000;
    /// 关闭后的反控窗口：> 2 × interval。
    const CLOSED_MULTIPLE: u32 = 2;
    const TICK_PROBE_PROMPT: &str = "w2-tick-probe";

    let fixture = AssembledHostFixture::start(FixtureDirs::new(), true).await;
    fixture.assert_all_ready();
    let scheduler = fixture.cron_scheduler();
    let task_id = scheduler
        .lock()
        .register(W2_EVERY_SECOND_CRON, TICK_PROBE_PROMPT)
        .expect("1s 粒度 cron 表达式必须被接受（计数观测的前提）");
    let cron_tools = wave2_tools("cron").to_vec();
    let mut triggers = fixture.cron_port().subscribe();
    let force_window = std::time::Duration::from_millis(FORCE_WINDOW_MS);

    // ── ① gen1：驱动存在（逐轮计数）──────────────────────────────────────────
    let gen1_rounds = force_due_rounds(
        "gen1",
        &scheduler,
        &mut triggers,
        &task_id,
        FORCE_ROUNDS,
        force_window,
    )
    .await;

    // ── ② reconnect：换新代对象，且新代仍有驱动 ──────────────────────────────
    // 旧代句柄由本用例一直持有 ⇒ `Arc::ptr_eq` 为假不是地址复用的巧合。
    let generation1 = fixture
        .pool
        .get_client("cron")
        .unwrap_or_else(|| panic!("cron 必须已连接"));
    let outcome = fixture.pool.reconnect("cron", None).await;
    assert!(
        outcome.is_ok(),
        "builtin 实例 reconnect 必须成功: {outcome:?}"
    );
    let generation2 = fixture
        .pool
        .get_client("cron")
        .unwrap_or_else(|| panic!("reconnect 后 cron 必须仍有句柄"));
    assert!(
        !Arc::ptr_eq(&generation1, &generation2),
        "reconnect 必须产出新代对象（同一代复用会让旧 tick 与旧链路继续存活）"
    );
    assert_instance_ready(&fixture.pool, "cron", &cron_tools);
    let gen2_rounds = force_due_rounds(
        "gen2（reconnect 后）",
        &scheduler,
        &mut triggers,
        &task_id,
        FORCE_ROUNDS,
        force_window,
    )
    .await;
    assert_eq!(
        scheduler.lock().list_tasks().len(),
        1,
        "reconnect 不得清空组合根任务表（实例换代 ≠ 组合根状态重建）"
    );

    // ── ③ 旧代驱动的收敛面（可证伪）：关闭后 >2× interval 零触发 ─────────────
    fixture.pool.remove_server("cron").await;
    let closed_window = W2_TICK_INTERVAL * CLOSED_MULTIPLE + std::time::Duration::from_millis(500);
    let closed = force_due_and_collect(&scheduler, &mut triggers, &task_id, closed_window).await;
    assert!(
        closed.is_empty(),
        "实例关闭后 {closed_window:?}（>2× interval）内不得有任何触发：到期任务若有驱动在跑，
        必在 1 个 interval 内被触发 ⇒ 该断言是「旧代 tick 已 cancel + join」的证伪面: {closed:?}"
    );
    assert_eq!(
        scheduler.lock().list_tasks().len(),
        1,
        "instance close 不得销毁组合根 scheduler 的任务表"
    );

    println!(
        "[W2 tick] interval={W2_TICK_INTERVAL:?} force_window={force_window:?}：gen1 轮计数={gen1_rounds:?} gen2 轮计数={gen2_rounds:?}（每轮都必须 ≥1；≈1/interval。相位错开的双驱动在此触发面上结构性不可见，见用例文档的可证伪性边界）；句柄 {}→{}（新代）；关闭后窗口 {closed_window:?} trigger={}（旧代驱动已 cancel+join）；组合根任务表 = {} 条",
        Arc::as_ptr(&generation1) as usize,
        Arc::as_ptr(&generation2) as usize,
        closed.len(),
        scheduler.lock().list_tasks().len()
    );
}
// ── 用例 10：生命周期矩阵（§8 第 15 行 / H09 / V 子计划 `[W2 ... lifecycle]`）─────

/// 主 plan §8 第 15 行（H09 / V-04 / R23 / R25 / A11 / A22 / A24）的收口断言：
/// 五实例 × {reconnect, close} + host shutdown 的**对象与计数矩阵**，逐格记录。
///
/// 每格判据（互不依赖，缺一即红）：
/// - **reconnect**：句柄换成新代对象（旧对象在比较时仍被本用例持有 ⇒ `Arc::ptr_eq` 为假
///   不是地址复用）、实例重新 ready、组合根状态不变（cron 任务表条数、宿主 LSP pool 的
///   `shutdown` 调用计数仍为 0）；
/// - **close**（`remove_server`）：该实例句柄消失、工具面空、面板不再有该行，而**组合根
///   状态保留**（cron 任务表仍在、共享 LSP pool 未被关闭、尚未处理的同 pool 实例仍 ready）；
///   cron 行额外取「到期任务在 >2× interval 内零触发」（代监督者已 cancel + join）；
/// - **host shutdown**：`shutdown_host` 收敛为 `Complete`，且**只有它**关共享 LSP pool
///   （观察点恰一次 + 终态），session 表清空。
///
/// 边界（报告登记）：`cfg.lsp_pool` 的观察点是**替身**（与用例 1 同款「只换被观察对象，
/// 不换消费方代码路径」）。真实 `LspServerPool` 在「从未有 client 启动」时没有可读的关闭
/// 状态（`has_servers()` 由配置派生；`ready_for` 对未启动的 server 恒假），因此
/// 「instance close 不关真实 host pool」以端口调用计数取证，不以真实池内部状态取证。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn lifecycle_state_matrix() {
    /// cron close 行的收敛窗口：> 2 × interval。
    const CONVERGE_WINDOW: std::time::Duration = std::time::Duration::from_millis(2_500);
    const LIFECYCLE_PROBE_PROMPT: &str = "w2-lifecycle-probe";

    let fixture = AssembledHostFixture::start(FixtureDirs::new(), true).await;
    fixture.assert_all_ready();
    let scheduler = fixture.cron_scheduler();
    let task_id = scheduler
        .lock()
        .register(W2_EVERY_SECOND_CRON, LIFECYCLE_PROBE_PROMPT)
        .expect("夹具 cron 任务注册");
    let mut triggers = fixture.cron_port().subscribe();
    let AssembledHostFixture {
        dirs,
        mut cfg,
        pool,
        _owner: mut mcp_owner,
        session_tasks: _session_tasks,
    } = fixture;
    // 观察点替换：宿主唯一 LSP pool 换成 recording 替身（`shutdown_host` 的消费路径不变）。
    let recorder = Arc::new(RecordingHostPool::new());
    cfg.lsp_pool = Some(Arc::clone(&recorder) as Arc<dyn LspPoolPort>);

    let transport = idle_transport();
    let mut sessions = HashMap::new();
    let created = crate::host::requests::handle_request(
        "session/new",
        &json!({ "cwd": dirs.workspace_str() }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/new 必须成功");
    let session_id = created["sessionId"]
        .as_str()
        .expect("sessionId")
        .to_string();
    // 前置：矩阵开始前投影面完整（五个实例都 ready、都投影宿主唯一 LSP pool）。
    assert!(
        sessions
            .get(&session_id)
            .expect("session state")
            .lsp_pool
            .as_ref()
            .is_some_and(|pool| Arc::ptr_eq(
                pool,
                &(Arc::clone(&recorder) as Arc<dyn LspPoolPort>)
            )),
        "session 必须投影宿主唯一 pool（矩阵的 lsp 行前提）"
    );

    for (index, (instance, expected)) in WAVE2_INSTANCES.iter().enumerate() {
        // ── 列 1：reconnect（新代对象；组合根状态与其他实例不变）──────────────
        let before = pool
            .get_client(instance)
            .unwrap_or_else(|| panic!("{instance} 必须已连接"));
        let outcome = pool.reconnect(instance, None).await;
        assert!(
            outcome.is_ok(),
            "{instance} reconnect 必须成功: {outcome:?}"
        );
        let after = pool
            .get_client(instance)
            .unwrap_or_else(|| panic!("{instance} reconnect 后必须仍有句柄"));
        assert!(
            !Arc::ptr_eq(&before, &after),
            "{instance} reconnect 必须换新代对象（旧对象仍被本用例持有 ⇒ 不是地址复用）"
        );
        assert_instance_ready(&pool, instance, expected);
        assert_eq!(
            recorder.shutdown_calls(),
            0,
            "reconnect（{instance}）不得关闭宿主共享 LSP pool"
        );
        assert_eq!(
            scheduler.lock().list_tasks().len(),
            1,
            "reconnect（{instance}）不得清空组合根 cron 任务表"
        );
        let generation = format!(
            "{}→{}",
            Arc::as_ptr(&before) as usize,
            Arc::as_ptr(&after) as usize
        );

        // ── 列 2：close（收敛所属任务；组合根状态保留）─────────────────────────
        pool.remove_server(instance).await;
        assert!(
            pool.get_client(instance).is_none(),
            "{instance} close 后不得留下句柄"
        );
        assert!(
            pool.get_tools(instance).is_empty(),
            "{instance} close 后不得留下工具面"
        );
        assert!(
            !pool
                .all_server_infos()
                .iter()
                .any(|row| row.name == *instance),
            "{instance} close 后面板不得再有该行（含 config-only 行）"
        );
        assert_eq!(
            recorder.shutdown_calls(),
            0,
            "instance close（{instance}）不得关闭宿主共享 LSP pool（共享池只在 host shutdown 关闭）"
        );
        assert_eq!(
            scheduler.lock().list_tasks().len(),
            1,
            "instance close（{instance}）不得清空组合根 cron 任务表（实例生命周期 ≠ 组合根状态）"
        );
        let siblings_ready = WAVE2_INSTANCES
            .iter()
            .skip(index + 1)
            .filter(|(other, _)| {
                pool.get_client(other)
                    .map(|handle| matches!(handle.status, ClientStatus::Connected))
                    .unwrap_or(false)
            })
            .map(|(other, _)| *other)
            .collect::<Vec<_>>();
        let pending: Vec<&str> = WAVE2_INSTANCES
            .iter()
            .skip(index + 1)
            .map(|(other, _)| *other)
            .collect();
        assert_eq!(
            siblings_ready, pending,
            "close（{instance}）不得牵连同 pool 的其它实例"
        );

        // cron 行附加：关闭后驱动已 join（到期任务零触发）。
        let task_cell = if *instance == "cron" {
            assert!(
                scheduler.lock().force_next_fire_to_past(&task_id),
                "反控前置：任务必须仍在组合根任务表内"
            );
            let observed = count_triggers_in_window(&mut triggers, CONVERGE_WINDOW).await;
            assert!(
                observed.is_empty(),
                "cron close 必须收敛代监督者：到期任务在 {CONVERGE_WINDOW:?}（>2× interval）内不得触发: {observed:?}"
            );
            format!("converged(>{CONVERGE_WINDOW:?} 零触发)")
        } else {
            "n/a(无 tick)".to_string()
        };

        println!(
            "[W2 isolation/lifecycle/off] instance={instance} link=connected→absent generation={generation} task={task_cell} state=kept(scheduler_tasks={},lsp_pool_shutdown={}) count=同 pool 未处理实例仍 ready={siblings_ready:?}",
            scheduler.lock().list_tasks().len(),
            recorder.shutdown_calls()
        );
    }

    // ── 列 3：host shutdown（唯一关共享 LSP pool 的点）────────────────────────
    let mut task_owner = cfg.host_task_owner.take().expect("宿主 task owner");
    let shared: SharedSessions = Arc::new(tokio::sync::Mutex::new(sessions));
    let prompt_locks: PromptLocks = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let connection = Arc::new(tokio::sync::Mutex::new(ConnectionContext::new(false)));
    let connection_cancellation = tokio_util::sync::CancellationToken::new();
    let mut cont_tx = None;
    let mut closing_sessions = std::collections::BTreeMap::new();
    let report = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        crate::host::shutdown::shutdown_host(
            &mut task_owner,
            mcp_owner.as_mut(),
            &cfg,
            &shared,
            &prompt_locks,
            &mut cont_tx,
            &connection,
            &connection_cancellation,
            &mut closing_sessions,
        ),
    )
    .await
    .expect("host shutdown 必须有界收敛（不得挂起）");
    assert!(
        matches!(
            report,
            crate::host::task_scope::HostTerminalShutdownReport::Complete { .. }
        ),
        "host shutdown 必须收敛（session/pool/task 全部结算）: {report:?}"
    );
    assert_eq!(
        recorder.shutdown_calls(),
        1,
        "host shutdown 必须对唯一 host pool 恰调用一次 shutdown（实例 close/reconnect 各列均为 0）"
    );
    assert!(
        recorder.is_closed(),
        "host shutdown 后共享 LSP pool 必须处于终态（language server 全部关闭）"
    );
    assert!(
        !recorder.ready_for(Path::new("w2-lifecycle.w2probe")),
        "关闭后的 pool 不得再报 ready"
    );
    assert!(
        shared.lock().await.is_empty(),
        "host shutdown 后 session 表必须清空"
    );
    println!(
        "[W2 isolation/lifecycle/off] host_shutdown={report:?} lsp_pool_shutdown={} closed={} sessions=0；五实例 close 后组合根 cron 任务表 = {} 条（实例生命周期不销毁组合根状态）",
        recorder.shutdown_calls(),
        recorder.is_closed(),
        scheduler.lock().list_tasks().len()
    );
}
