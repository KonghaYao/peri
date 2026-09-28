use super::*;

// ── 用例 6：`PERI_MCP_BUILTIN=off` 零注入（§8 第 10 / 15 行 / A2 / A11 / R28）────

/// `PERI_MCP_BUILTIN=off` ⇒ **零 builtin 注入**：五个实例都不出现，无 handler、无 tick、
/// 模型面看不到任何 builtin 工具（与 wave 1 的 `off` 语义连续）。
///
/// 四个面逐一取证，每个「不出现」都配正控制（否则可能是断言面空洞）：
/// ① **配置/实例层**：`all_server_infos()` 为空（含 config-only 行）⇒ loader 没注入任何
///    builtin 条目，pool 里也不存在其它 server（夹具 HOME / workspace 均已隔离）；
/// ② **handler 层**：五个实例 `get_client` 全 `None`、`get_tools` 全空；
/// ③ **tick 层**：夹具用**自己持有**的 scheduler 注册一个已到期任务（`force_next_fire_to_past`），
///    off 侧 >2× `BUILTIN_TICK_INTERVAL` 内零触发；B 段在同一观测法上跑 ON 正控制，
///    必须看到触发 —— 否则 ③ 的「零触发」不可证伪；
/// ④ **模型直连面 + 搜索面**：五个实例的 effective name 与裸名都不得出现（A2：off 的
///    退回态**没有**该能力；middleware 提供面已删，不存在旧实现回退），同时直连面仍含
///    核心工具、搜索面仍含非 builtin 的 deferred 工具（正控制：面本身工作正常）。
///
/// 与 wave 1 的连续性：`host::mcp_v4_builtin::builtin_injection_off_removes_capabilities_without_fallback`
/// 已证 web / artifact 在 off 下不可见；本用例把同一语义扩到五个实例，并补上 cron / lsp
/// 的 handler 与 tick 面。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn off_has_zero_builtin_injection() {
    // ── A：off 生效期间（夹具同款上下文，tick_enabled = true：若注入发生，③ 必然看到触发）──
    {
        let off = BuiltinInjectionOff::set();
        let dirs = FixtureDirs::new();
        let cwd = dirs.workspace_str();
        let (cron_tx, mut triggers) = tokio::sync::mpsc::unbounded_channel();
        let scheduler = Arc::new(parking_lot::Mutex::new(
            peri_middlewares::cron::CronScheduler::new(cron_tx),
        ));
        let task_id = scheduler
            .lock()
            .register("* * * * *", "off 观测用任务（必须永不被触发）")
            .expect("夹具 cron 任务注册");
        assert!(
            scheduler.lock().force_next_fire_to_past(&task_id),
            "夹具必须让任务立即到期（否则 tick 观测面空洞）"
        );
        let fixture = BuiltinHostFixture::start_with_context(
            dirs,
            BuiltinInstanceContext::new(cwd.clone())
                .with_cron(CronInstanceInput {
                    scheduler: Arc::clone(&scheduler),
                    tick_enabled: true,
                })
                .with_lsp(LspInstanceInput {
                    pool: create_host_lsp_pool(&cwd, &[lsp_server_config("off_probe")]),
                }),
        )
        .await;

        // ① 配置/实例层：零 server（builtin 条目都没进 pool）。
        assert!(
            fixture.pool.all_server_infos().is_empty(),
            "off 时 pool 不得有任何 server（含 config-only 行）: {:?}",
            fixture
                .pool
                .all_server_infos()
                .iter()
                .map(|info| info.name.clone())
                .collect::<Vec<_>>()
        );
        assert!(
            fixture.pool.get_all_clients().is_empty(),
            "off 时不得有任何 Connected client（`get_all_clients` 只回 Connected 句柄）"
        );
        // ② handler 层：五个实例都不出现。
        for (instance, _) in WAVE2_INSTANCES {
            assert!(
                fixture.pool.get_client(instance).is_none(),
                "off 时 `{instance}` 实例不得被注册（零注入）"
            );
            assert!(
                fixture.pool.get_tools(instance).is_empty(),
                "off 时 `{instance}` 不得有工具面"
            );
        }
        // ③ tick 层：已到期任务在 2.4s（>2× 1s interval）内不得被触发。
        let observed =
            collect_triggers(&mut triggers, std::time::Duration::from_millis(2_400)).await;
        assert!(
            observed.is_empty(),
            "off 时不得有任何 tick 驱动者（已到期任务在 2.4s 内零触发）: {observed:?}"
        );
        // ④ 模型面：五个实例的 effective name / 裸名都不得出现。
        let faces = probe_tool_faces(
            "off（零注入）",
            fixture.session_context("mcp-v4-wave2-off").await,
            "web fetch artifact cron lsp",
        )
        .await;
        for (instance, _) in WAVE2_INSTANCES {
            for name in declared_effective_names(instance) {
                assert!(
                    !faces.direct.iter().any(|tool| tool == name),
                    "off 时 `{name}` 不得进入首个 LLM 请求的直连参数: {:?}",
                    faces.direct
                );
                assert!(
                    !faces.searched.iter().any(|tool| tool == name),
                    "off 时 `{name}` 不得出现在搜索/执行面: {:?}",
                    faces.searched
                );
            }
            for bare in declared_tool_names(instance) {
                assert!(
                    !faces.direct.iter().any(|tool| tool == bare)
                        && !faces.searched.iter().any(|tool| tool == bare),
                    "off 的退回态**没有**内建能力（middleware 提供面已删，不存在旧实现回退）：裸名 `{bare}` 不得复活"
                );
            }
        }
        // 正控制：面本身可用（直连面的核心工具 + 搜索面的非 builtin deferred 工具）。
        //
        // 正控制只能取**迁移后仍在链上直供**的内建工具：`Read` / `Bash` 等文件与 shell
        // 工具的 middleware 直供面已随 C1 删除（其模型面身份改为 builtin effective name），
        // 继续拿裸名当正控制会让本用例永远红，也失去「面非空洞」的证明力。名字取
        // `core_tools` 常量而非字面量，避免新增裸名硬编码。
        for control in [
            SEARCH_EXTRA_TOOLS_NAME,
            core_tools::TOOL_TODO,
            core_tools::TOOL_AGENT,
            core_tools::TOOL_ASK_USER,
            core_tools::TOOL_SKILL,
            core_tools::TOOL_DISCOVER_SKILLS,
        ] {
            assert!(
                faces.direct.iter().any(|tool| tool == control),
                "正控制：`{control}` 必须仍在直连面（否则本用例的「不含」只是因为工具面空洞）: {:?}",
                faces.direct
            );
        }
        assert!(
            !faces.searched.is_empty(),
            "正控制：搜索/执行面必须仍可用（off 只关 builtin 注入，不移除搜索面）: {:?}",
            faces.searched
        );
        println!(
            "[W2 isolation/off] PERI_MCP_BUILTIN=off：servers=0；handler=0；tick 触发 = 0（2.4s 窗口）；直连面 = {:?}；搜索面 = {:?}",
            faces.direct, faces.searched
        );
        drop(off);
    }

    // ── B：ON 正控制（同一 tick 观测法必须看到触发；否则 A 段的「零触发」不可证伪）──
    let dirs = FixtureDirs::new();
    let cwd = dirs.workspace_str();
    let (cron_tx, mut triggers) = tokio::sync::mpsc::unbounded_channel();
    let scheduler = Arc::new(parking_lot::Mutex::new(
        peri_middlewares::cron::CronScheduler::new(cron_tx),
    ));
    let task_id = scheduler
        .lock()
        .register("* * * * *", "on 控制任务")
        .expect("夹具 cron 任务注册");
    assert!(
        scheduler.lock().force_next_fire_to_past(&task_id),
        "夹具必须让任务立即到期"
    );
    let fixture = BuiltinHostFixture::start_with_context(
        dirs,
        BuiltinInstanceContext::new(cwd.clone())
            .with_cron(CronInstanceInput {
                scheduler: Arc::clone(&scheduler),
                tick_enabled: true,
            })
            .with_lsp(LspInstanceInput {
                pool: create_host_lsp_pool(&cwd, &[]),
            }),
    )
    .await;
    assert_instance_ready(
        &fixture.pool,
        "cron",
        &["cron_register", "cron_list", "cron_remove"],
    );
    let observed = collect_triggers(&mut triggers, std::time::Duration::from_secs(3)).await;
    assert!(
        !observed.is_empty(),
        "ON 控制：`cron` 实例的 tick 必须驱动 scheduler（否则 A 段的「零触发」不可证伪）"
    );
    assert!(
        observed.iter().all(|id| *id == task_id),
        "ON 控制：触发的必须是夹具注册的唯一到期任务: {observed:?}"
    );
    println!(
        "[W2 isolation/off] ON 控制：tick 触发 = {} 次（任务 {task_id}）；同一观测法在 off 侧为 0 次",
        observed.len()
    );
}

// ── 用例 7：wave 1 兼容（§8 第 19 行 / R10 / R19）─────────────────────────────

/// wave 1 的 `web` / `artifact` 在 wave 2（cron / lsp 实迁）落地后**行为不变**，
/// 且不因 cron / lsp 的加入而改变。
///
/// 判据全部来自**注册表（冻结字面量）与同一次真实运行**，不用「wave 1 曾录过」当断言：
/// ① 两个实例的 transport（peer + 面板 `builtin` 身份）、协议初始化（`server_info`）、
///    能力协商（tools）、live `tools/list`（逐字逐序）不变；
/// ② 首个 LLM 请求直连参数里的 `mcp__*` 子集**恰为**「各实例注册表声明为 `direct` 的
///    effective name 全集」（由 `WAVE2_INSTANCES` + `declared_direct_effective_names`
///    派生，不写死名单）—— 集合**相等**（不是包含）：cron / lsp 的 deferred 加入贡献 0，
///    workspace 的 7 项 direct 加入逐项对齐，没有任何实例多带或被挤出；
/// ③ 裸名（`WebSearch` / `WebFetch` / `artifact` / 7 个文件工具名）在直连面与搜索面
///    都不得复活（wave 1 的 XOR 判据，A20；wave 3 将同一判据扩到 workspace 的 7 项）。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn wave1_compatibility_with_wave2() {
    let dirs = FixtureDirs::new();
    let fixture =
        BuiltinHostFixture::start(dirs, &[lsp_server_config("wave2_wave1_compat")], false).await;
    for (instance, expected) in WAVE2_INSTANCES {
        assert_instance_ready(&fixture.pool, instance, expected);
    }

    let faces = probe_tool_faces(
        "wave 1 兼容（五实例共存）",
        fixture.session_context("mcp-v4-wave2-wave1-compat").await,
        "web fetch artifact",
    )
    .await;

    // ② 直连面的 builtin 子集 == 全部实例**声明为 direct** 的模型可见名集合。
    //    wave 1 只有 web / artifact 声明 direct；wave 2 的 cron / lsp 声明 deferred（贡献 0）；
    //    wave 3 的 workspace 声明 7 项 direct（AW3-03：迁移前这 7 项就在直连表内，只是名字
    //    是裸名）。集合**相等**（不是包含）：后加入的实例既没有多带 direct 工具，也没有把
    //    既有实例挤出去。（wave 1 三名仍在表内由 `peri-acp-types` 注册表用例锁定。）
    let mut expected_builtin_direct: Vec<String> = WAVE2_INSTANCES
        .iter()
        .flat_map(|(instance, _)| declared_direct_effective_names(instance))
        .map(str::to_string)
        .collect();
    expected_builtin_direct.sort();
    let mut observed_builtin_direct: Vec<String> = faces
        .direct
        .iter()
        .filter(|name| {
            expected_builtin_direct
                .iter()
                .any(|expected| expected == *name)
        })
        .cloned()
        .collect();
    observed_builtin_direct.sort();
    assert_eq!(
        observed_builtin_direct, expected_builtin_direct,
        "1R 直连面的 builtin 子集必须恰为各实例声明为 direct 的集合（cron / lsp 的 deferred 加入与 workspace 的 direct 加入都必须逐项对齐）"
    );

    // ③ 当前模型可见名只出现于应有的面，不生成历史前缀执行别名。
    for (instance, _) in WAVE2_INSTANCES {
        for current_name in declared_effective_names(instance) {
            assert!(
                if declared_direct_effective_names(instance).contains(&current_name) {
                    faces.direct.iter().any(|tool| tool == current_name)
                } else {
                    !faces.direct.iter().any(|tool| tool == current_name)
                },
                "工具 `{current_name}` 的 direct/deferred 分类必须符合注册表: {:?}",
                faces.direct
            );
        }
    }
    println!(
        "[W2 wave1] 五实例共存下 wave 1 面不变：直连 builtin 子集 = {observed_builtin_direct:?}；web/artifact 面板 transport = builtin；cron/lsp 直连 = 0"
    );
}
