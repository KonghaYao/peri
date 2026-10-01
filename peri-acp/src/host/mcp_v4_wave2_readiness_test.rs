use super::*;

// ── 用例 4：ready 闸门矩阵（§8 第 2 行 / R19 / A33 / IF-M3）────────────────────

/// 宿主侧 ready 闸门矩阵：五个 builtin 实例在 **1R 之前**逐实例取证，故障侧则必须
/// 「不发布 ready + fatal 阻塞首个 LLM 请求」。
///
/// **成功侧**（A 段）：[`WAVE2_INSTANCES`] 的五个实例各自跑 [`assert_instance_ready`]
/// 的四段证据（transport / 协议初始化 / 能力协商 / live `tools/list`），全部发生在任何
/// prompt 之前；随后一次真实 prompt 证明这些 ready 事实在首个 LLM 请求时已被投影
/// （cron / lsp 的 effective name 落在搜索面、web / artifact 落在直连面）。
///
/// **故障侧**（B 段）：宿主可达的等价故障面是「实例装配失败」（上下文缺失 /
/// 实例输入缺失 ⇒ typed `BuiltinSpawnError`）。对 cron 与 lsp **分别**注入，断言：
/// ① 故障实例 ready 面五行全否（status / peer / version / tools / 面板行）；
/// ② 同 pool 的其它三个实例不受牵连（实例级隔离）；
/// ③ 首个 LLM 请求被 system 闸门阻塞 —— 模型 **0 次**调用、`failure.kind == Internal`、
/// 文案归属 `McpMiddleware` 且是该实例的固定模板。
///
/// **未做（报告登记为 UNVERIFIED）**：把「坏 handler」塞进 cron / lsp 实例以观察**协议
/// 失败 / `tools/list` 超时**这条路在宿主侧**不可达** —— `mcp::builtin::dispatch::
/// builtin_server_handler` 与 `builtin::runtime::spawn_builtin_transport_with_*` 都是
/// `pub(crate)`，仓库也没有 test-seam feature；peri-acp 只能装配**真实** handler。
/// 协议失败 / 超时注入由 crate 内用例（`mcp::builtin_runtime_tests`，owner V-02，
/// §8 第 2 行）与 stdio 夹具（`host::mcp_v4_startup::system_mcp_transport_failure_*` /
/// `system_mcp_timeout_is_fatal_not_cancelled`）覆盖，本用例不拿外部 stdio fatal 冒充
/// builtin 实例的协议失败。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn wave2_ready_gate_matrix() {
    // 注册表声明与本用例的内联期望必须一致（否则下面断言的是用例的臆想而非契约）。
    for (instance, expected) in WAVE2_INSTANCES {
        assert_eq!(
            declared_tool_names(instance).as_slice(),
            expected,
            "{instance} 的注册表工具声明变了：WAVE2_INSTANCES 必须同步（本用例按下标断言工具面）"
        );
    }

    // ── A：成功侧矩阵（1R 之前逐实例取证）────────────────────────────────────
    let dirs = FixtureDirs::new();
    let fixture = BuiltinHostFixture::start(dirs, &[lsp_server_config("wave2_ready")], false).await;
    for (instance, expected) in WAVE2_INSTANCES {
        assert_instance_ready(&fixture.pool, instance, expected);
    }

    // 1R 投影：句柄面之外再证「ready 事实已进入首个 LLM 请求的工具面」。
    let faces = probe_tool_faces(
        "ready 矩阵（五实例）",
        fixture.session_context("mcp-v4-wave2-ready").await,
        "cron lsp web fetch artifact",
    )
    .await;
    for name in declared_direct_effective_names("web")
        .into_iter()
        .chain(declared_direct_effective_names("artifact"))
    {
        assert!(
            faces.direct.iter().any(|tool| tool == name),
            "wave 1 的 direct effective name `{name}` 必须在首个请求的直连参数内: {:?}",
            faces.direct
        );
    }
    for name in declared_effective_names("cron")
        .into_iter()
        .chain(declared_effective_names("lsp"))
    {
        assert!(
            faces.searched.iter().any(|tool| tool == name),
            "cron / lsp 在 1R 前 ready ⇒ `{name}` 必须落在搜索面: {:?}",
            faces.searched
        );
        assert!(
            !faces.direct.iter().any(|tool| tool == name),
            "cron / lsp 工具一律 deferred（`direct: false`），不得进入直连面: `{name}`"
        );
    }
    println!(
        "[W2 ready] 1R 投影：直连面 builtin = {:?}；搜索面 = {:?}",
        faces
            .direct
            .iter()
            .filter(|name| name.starts_with("mcp__"))
            .collect::<Vec<_>>(),
        faces.searched
    );

    // ── B：故障侧（装配失败）⇒ 不发布 ready + fatal 阻塞 1R ────────────────────
    for fault in ["cron", "lsp"] {
        let dirs = FixtureDirs::new();
        let cwd = dirs.workspace_str();
        // 只给**健康那个**实例的输入：被观察实例的输入缺失 ⇒ 装配期 typed 失败。
        let (cron_tx, _cron_triggers) = tokio::sync::mpsc::unbounded_channel();
        let scheduler = Arc::new(parking_lot::Mutex::new(peri_mcp_cron::CronScheduler::new(
            cron_tx,
        )));
        let context = BuiltinInstanceContext::new(cwd.clone());
        let context = if fault == "cron" {
            context.with_lsp(LspInstanceInput {
                pool: peri_mcp_lsp::create_host_lsp_pool(
                    &cwd,
                    &[lsp_server_config("wave2_ready_fault")],
                ),
            })
        } else {
            context.with_cron(CronInstanceInput {
                scheduler,
                tick_enabled: false,
            })
        };
        let fixture = BuiltinHostFixture::start_with_context(dirs, context).await;

        // ① 故障实例：ready 面五行全否。
        assert_no_ready_evidence(&fixture.pool, fault, "缺少上下文输入");
        // ② 其它实例不受牵连（一个实例装配失败不污染同 pool 的其它实例）。
        for (instance, expected) in WAVE2_INSTANCES {
            if instance != fault {
                assert_instance_ready(&fixture.pool, instance, expected);
            }
        }
        // ③ fatal：system 闸门在**首个** LLM 请求之前阻塞。
        let sink = Arc::new(MockEventSink::new());
        let model = Arc::new(WireScriptedModel::new(Vec::new()));
        let result = run_wire_prompt(
            fixture
                .session_context(&format!("mcp-v4-wave2-ready-fault-{fault}"))
                .await,
            &sink,
            &model,
        )
        .await;
        assert!(
            !result.ok,
            "{fault} 实例未 ready 必须使首个 prompt 失败: {:?}",
            result.failure
        );
        assert_eq!(
            model.call_count(),
            0,
            "{fault} 实例未 ready 时模型调用次数必须为 0（闸门先于 Reason）"
        );
        let failure = result
            .failure
            .as_ref()
            .unwrap_or_else(|| panic!("{fault} 实例未 ready 必须产生 fatal failure"));
        assert_eq!(
            failure.kind,
            ExecutionFailureKind::Internal,
            "MCP 准入失败只能投影为 internal 类别: {failure:?}"
        );
        assert!(
            failure.public_message.contains("McpMiddleware"),
            "失败必须归属于 McpMiddleware: {}",
            failure.public_message
        );
        let expected_text = format!("System MCP \"{fault}\" 启动失败");
        assert!(
            failure.public_message.contains(expected_text.as_str()),
            "失败文案必须指名未 ready 的实例（`{expected_text}`）: {}",
            failure.public_message
        );
        assert!(
            failure
                .public_message
                .contains("transport 或协议初始化失败"),
            "实例装配失败的固定文案必须是 ConnectionFailed 模板: {}",
            failure.public_message
        );
        println!(
            "[W2 ready] fault={fault}：ready 未发布；首个 LLM 请求被阻塞（模型调用 = 0）；fatal = {}",
            failure.public_message
        );
    }
}

// ── 用例 5：A33 注入序 + 重复注入判定（§8 第 2 行 / H-02 / V 子计划 `[W2 context]`）──

/// A33：`BuiltinInstanceContext` 必须在 `run_initialize` **之前**完成注入；**重复注入**
/// 返回 typed error 而不是静默覆盖。
///
/// 两段互相独立、各自可证伪：
///
/// **A 段（同一 pool 的完整序列）**：注入 → 重复注入（**封口前**）→ `run_initialize` →
/// 封口后再注入。判据不是「按构造顺序自证」，而是三条互相独立的可观察事实：
/// ① 封口前的重复注入返回 `AlreadyInjected`，且**首个上下文继续生效** —— 第二份上下文
///    故意既无 cron 输入也无 LSP pool，若它被静默接受，下一段 `run_initialize` 里
///    cron / lsp 会以 `InstanceInputMissing` 失败（不是 5/5 ready）；
/// ② `run_initialize` 收口为 `Ready { total: 5 }` 且五个实例 ready、工具面与注册表逐字
///    相等 —— 这同时是「注入早于 initialize」的证据（注入晚于封口只会被 typed 拒绝，
///    实例只能以 `ContextMissing` 收口，见 B 段实测）；
/// ③ 封口**之后**的再注入换分支返回 `InitializationStarted`（`initialize_started` 先判，
///    封口优先，见 `McpClientPool::set_builtin_instance_context` 的判定顺序），且被拒的
///    注入**不改变**任何事实（五个实例仍 ready）。
///
/// **B 段（未注入 + 先 initialize）**：五个实例全部 typed `ContextMissing` 失败、pool 收口
/// 为 `Failed`（**不是**「ready 空配置」，也不 fallback 到别的传输形态）；此后的**首次**
/// 注入同样被 `InitializationStarted` 拒绝，且不救活任何实例。
///
/// 本用例自己起 pool（而不是复用 [`BuiltinHostFixture`]）：夹具的「一次注入 + 一次
/// initialize」形状无法表达「封口前的重复注入」与「未注入状态下 initialize」，而这两条
/// 正是 A33 的可证伪面。装配步骤与生产逐位相同（pool → 绑定 cwd → 注入 → initialize），
/// 夹具不复制一行。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn context_injected_before_initialize_and_rejects_duplicate() {
    // ── A：注入（封口前）→ 重复注入 → initialize → 封口后再注入 ──────────────
    let dirs = FixtureDirs::new();
    let cwd = dirs.workspace_str();
    let (cron_tx, _cron_triggers) = tokio::sync::mpsc::unbounded_channel();
    let scheduler = Arc::new(parking_lot::Mutex::new(peri_mcp_cron::CronScheduler::new(
        cron_tx,
    )));
    // 宿主组合根的**唯一**一份上下文：cron 输入齐备 + 生效 LSP 配置非空。
    let first = BuiltinInstanceContext::new(cwd.clone())
        .with_cron(CronInstanceInput {
            scheduler,
            tick_enabled: false,
        })
        .with_lsp(LspInstanceInput {
            pool: peri_mcp_lsp::create_host_lsp_pool(&cwd, &[lsp_server_config("wave2_inject")]),
        });
    let (_owner, spawner) = McpTaskOwner::new();
    let pool = Arc::new(McpClientPool::new_pending_with_spawner(spawner));
    pool.bind_execution_cwd(&dirs.workspace)
        .expect("夹具绑定 execution cwd");

    // ① 注入（封口前，`run_initialize` 尚未开始）。
    pool.set_builtin_instance_context(Arc::new(first))
        .expect("首次注入必须成功（A33：注入早于 initialize）");
    // ② 重复注入（**仍在封口前**）⇒ typed `AlreadyInjected`（两个字段都在窗口内，因此第二
    //    条分支可见）：第二份上下文缺 cron / lsp 输入，若被静默接受，③ 必然失败。
    let duplicate_err = pool
        .set_builtin_instance_context(Arc::new(BuiltinInstanceContext::new(
            dirs.second_workspace().to_string_lossy().into_owned(),
        )))
        .err();
    assert_eq!(
        duplicate_err,
        Some(BuiltinContextError::AlreadyInjected),
        "封口前的重复注入必须是 typed 拒绝（含同一 `Arc` 再注入），不得静默覆盖"
    );

    // ③ initialize：只有「首个上下文生效」才可能 5/5 ready。
    let (status_tx, status_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);
    tokio::time::timeout(
        std::time::Duration::from_secs(30),
        McpClientPool::run_initialize(
            Arc::clone(&pool),
            &dirs.workspace,
            &dirs.claude_home,
            status_tx,
            None,
        ),
    )
    .await
    .expect("真实初始化必须有界收敛（不得挂起）");
    let status = status_rx.borrow().clone();
    assert!(
        matches!(status, McpInitStatus::Ready { total: 5 }),
        "首个上下文生效 ⇒ 五个实例必须全部 ready（第二份上下文若被静默接受，cron / lsp 会以 InstanceInputMissing 失败）: {status:?}"
    );
    for (instance, expected) in WAVE2_INSTANCES {
        assert_instance_ready(&pool, instance, expected);
    }

    // ④ 封口**之后**的再注入：typed `InitializationStarted`（封口优先于「已有上下文」）。
    let bare_cwd = dirs.second_workspace().to_string_lossy().into_owned();
    let late_err = pool
        .set_builtin_instance_context(Arc::new(BuiltinInstanceContext::new(bare_cwd)))
        .err();
    assert_eq!(
        late_err,
        Some(BuiltinContextError::InitializationStarted),
        "封口后的注入（即使已有上下文）必须先报封口分支，不得改报 AlreadyInjected"
    );
    // 被拒的注入不改变任何事实：五个实例仍 ready、工具面不变。
    for (instance, expected) in WAVE2_INSTANCES {
        assert_instance_ready(&pool, instance, expected);
    }
    println!(
        "[W2 context] inject_seq=1（封口前）→ duplicate_err={duplicate_err:?} → initialize_seq=2（Ready 5/5）→ 封口后再注入={late_err:?}；ready 面不受被拒注入影响"
    );

    // ── B：未注入 + 先 initialize ⇒ 全实例 ContextMissing，且首次晚注入被拒 ────
    let late_dirs = FixtureDirs::new();
    let (_owner, spawner) = McpTaskOwner::new();
    let pool = Arc::new(McpClientPool::new_pending_with_spawner(spawner));
    pool.bind_execution_cwd(&late_dirs.workspace)
        .expect("夹具绑定 execution cwd");
    let (status_tx, status_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);
    let workspace = late_dirs.workspace.clone();
    let claude_home = late_dirs.claude_home.clone();
    tokio::time::timeout(
        std::time::Duration::from_secs(30),
        McpClientPool::run_initialize(Arc::clone(&pool), &workspace, &claude_home, status_tx, None),
    )
    .await
    .expect("未注入上下文的真实初始化必须有界收敛（不得挂起）");
    let status = status_rx.borrow().clone();
    assert!(
        matches!(status, McpInitStatus::Failed(_)),
        "五个 builtin 实例全部失败 ⇒ pool 必须收口为 Failed，而不是「ready 空配置」: {status:?}"
    );
    let failed_text = match &status {
        McpInitStatus::Failed(text) => text.clone(),
        other => panic!("初始化必须收口为 Failed，实际: {other:?}"),
    };
    // 失败计数从**注册表**派生，不写死字面量：实例数随波次增长（wave 1 两个 → wave 3
    // 五个），写死会让本断言在「少盯一个实例」时继续绿。生产文本由 connectable 计数
    // 生成（`peri-middlewares/src/mcp/initialize.rs:542`）。
    let expected_failed = format!(
        "{} 个服务器连接失败",
        peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES.len()
    );
    assert!(
        failed_text.contains(&expected_failed),
        "五个实例都必须计入失败（不得因缺上下文就静默跳过）: {failed_text}"
    );
    assert!(
        failed_text.contains("上下文未注入"),
        "失败原因必须是 typed `ContextMissing` 文本（不 fallback 到别的传输形态）: {failed_text}"
    );
    for (instance, _) in WAVE2_INSTANCES {
        assert_no_ready_evidence(&pool, instance, "上下文未注入");
    }
    // 晚注入（**首次**注入，但封口已置真）⇒ typed 拒绝，且不救活任何实例。
    let late_cwd = late_dirs.workspace_str();
    let (cron_tx, _cron_triggers) = tokio::sync::mpsc::unbounded_channel();
    let scheduler = Arc::new(parking_lot::Mutex::new(peri_mcp_cron::CronScheduler::new(
        cron_tx,
    )));
    let first_late_err = pool
        .set_builtin_instance_context(Arc::new(
            BuiltinInstanceContext::new(late_cwd.clone())
                .with_cron(CronInstanceInput {
                    scheduler,
                    tick_enabled: false,
                })
                .with_lsp(LspInstanceInput {
                    pool: peri_mcp_lsp::create_host_lsp_pool(
                        &late_cwd,
                        &[lsp_server_config("wave2_inject_late")],
                    ),
                }),
        ))
        .err();
    assert_eq!(
        first_late_err,
        Some(BuiltinContextError::InitializationStarted),
        "晚于 `run_initialize` 的**首次**注入必须是 typed 拒绝（否则会出现「上下文在，但实例按旧上下文建」的假象）"
    );
    for (instance, _) in WAVE2_INSTANCES {
        assert_no_ready_evidence(&pool, instance, "上下文未注入");
    }
    println!(
        "[W2 context] 未注入场景：五个实例全部 Failed（ready = 0，pool = {status:?}）；首次晚注入={first_late_err:?}（被拒后仍无实例 ready）"
    );
}
