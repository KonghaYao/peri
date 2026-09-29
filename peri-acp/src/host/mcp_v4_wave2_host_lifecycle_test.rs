use super::*;

// ── 用例 1：多 cwd 共享 host pool + host shutdown 收敛 ────────────────────────

/// 主 plan §8 第 13 行（A11/A21/A22）的终态断言。
///
/// 三段落互相独立、各自可证伪：
/// ① 配置合并前移（settings.json 的 lspServers 在装配期生效，且 pool 构造时即带该配置）；
/// ② 多 cwd 共享（两个 session 的 `lsp_pool` 与宿主句柄 `Arc::ptr_eq`）；
/// ③ host shutdown 收敛（唯一 pool 被 `shutdown` 恰一次并进入终态）。
///
/// ②是 A22 登记的**功能退化**本体：迁移前每个 session 各自建池（多 cwd ⇒ 多个
/// root_uri），迁移后共享宿主 root_uri；本用例不把退化写成缺陷，而是把它钉成可观测事实。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn multi_cwd_degradation_and_host_shutdown() {
    let dirs = FixtureDirs::new();
    dirs.write_lsp_settings("wave2_host");
    let mut cfg = assemble_host(&dirs).await;

    // ── ① 配置合并前移（A21）：合并结果必须在装配期可见，且已在 pool 构造时生效 ──
    let merged: Vec<&str> = cfg
        .plugin_lsp_servers
        .iter()
        .map(|server| server.name.as_str())
        .collect();
    assert_eq!(
        merged,
        vec!["wave2_host"],
        "宿主装配必须把 settings.json 的 lspServers 合并进生效配置（A21 前移）: {merged:?}"
    );
    let host_pool = cfg
        .lsp_pool
        .clone()
        .expect("生产装配必须构造 host pool（空配置也在，A6）");
    let concrete = Arc::clone(&host_pool)
        .downcast_arc::<LspServerPool>()
        .unwrap_or_else(|_| panic!("host pool 必须是 LspServerPool"));
    assert!(
        concrete.has_servers(),
        "生效配置非空 ⇒ pool 必须已登记 server（handler 按 has_servers() 快照工具面）"
    );
    println!(
        "[W2-V03 终态] host 装配：生效 LSP 配置 = {merged:?}；host pool 已登记 server = {}（配置合并在 pool 构造之前生效）",
        concrete.has_servers()
    );

    // 观察点替换：宿主的唯一 pool 换成 recording 替身，session 投影与 host shutdown
    // 的代码路径不变（只换被观察对象）。
    let recorder = Arc::new(RecordingHostPool::new());
    cfg.lsp_pool = Some(Arc::clone(&recorder) as Arc<dyn LspPoolPort>);

    // ── ② 多 cwd：两个不同 cwd 的 session 都投影同一 host pool ──
    let first_cwd = dirs.workspace.clone();
    let second_cwd = dirs.second_workspace();
    let mut sessions = HashMap::new();
    let transport = idle_transport();
    let mut created = Vec::new();
    for cwd in [&first_cwd, &second_cwd] {
        let response = crate::host::requests::handle_request(
            "session/new",
            &json!({ "cwd": cwd }),
            &cfg,
            &mut sessions,
            &transport,
        )
        .await
        .expect("session/new 必须成功");
        created.push(
            response["sessionId"]
                .as_str()
                .expect("sessionId")
                .to_string(),
        );
    }
    assert_eq!(created.len(), 2);
    let projected: Vec<Arc<dyn LspPoolPort>> = created
        .iter()
        .map(|session_id| {
            sessions
                .get(session_id)
                .unwrap_or_else(|| panic!("{session_id} 必须已注册"))
                .lsp_pool
                .clone()
                .unwrap_or_else(|| panic!("{session_id} 必须投影 host pool"))
        })
        .collect();
    assert!(
        Arc::ptr_eq(&projected[0], &projected[1]),
        "多 cwd 的 session 必须共享**同一** host pool（A11/A22 单 pool）"
    );
    assert!(
        Arc::ptr_eq(
            &projected[0],
            &(Arc::clone(&recorder) as Arc<dyn LspPoolPort>)
        ),
        "session 投影的必须是宿主句柄本身，而不是第二份 pool"
    );
    println!(
        "[W2-V03 终态] 多 cwd：session = {created:?}；投影同一 host pool = {}（两 session `Arc::ptr_eq` = {}）",
        Arc::ptr_eq(&projected[0], &(Arc::clone(&recorder) as Arc<dyn LspPoolPort>)),
        Arc::ptr_eq(&projected[0], &projected[1])
    );

    // ── ③ host shutdown：唯一 pool 恰关闭一次并收敛 ──
    let mut task_owner = cfg.host_task_owner.take().expect("宿主 task owner");
    let mut mcp_task_owner = cfg.mcp_task_owner.take().expect("MCP task owner");
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
            mcp_task_owner.as_mut(),
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
        "host shutdown 必须对唯一 host pool 恰调用一次 shutdown（不去重集合、不按 session 重复）"
    );
    assert!(
        recorder.is_closed(),
        "host shutdown 后 pool 必须处于终态（language server 全部关闭、不可再路由文件）"
    );
    assert!(
        !recorder.ready_for(Path::new("/tmp/whatever.rs")),
        "关闭后的 pool 对任何文件都不得再报 ready"
    );
    println!(
        "[W2-V03 终态] host shutdown：报告 = {report:?}；唯一 host pool 的 shutdown 调用 = {}；终态（language server 全关）= {}",
        recorder.shutdown_calls(),
        recorder.is_closed()
    );
    assert!(
        shared.lock().await.is_empty(),
        "host shutdown 后 session 表必须清空"
    );
}

// ── 用例 2：1:N root 形态下不存在 per-session workspace 注入（U5 补验）────────────

/// wave 3 遗留任务 U5 的补验。原判定「第二个 session 的注入会得 `AlreadyInjected`」
/// 在 **1:N 形态**（`session_resources = true` 的会话环境装配面，可作 server root）下
/// 不成立：该形态下**任何** session 调 [`crate::host::workspace::SessionEnvironment::assemble`]
/// 都返回 `Ok(None)`（首句 `host.workspace_assembly.as_ref()` 取不到 `Some` 即早返回），
/// 因此不存在「第二个 session 的注入尝试」——池级重复注入面的 `AlreadyInjected` 只由
/// 同一 pool 被重复 `set_builtin_instance_context` 触发，那一面由
/// [`context_injected_before_initialize_and_rejects_duplicate`] 覆盖，与本形态无关。
///
/// 真正的退化是：**root 的那一份 `WorkspaceInstanceInput` 被全体 session 冒充使用**，
/// 而每个 session 记录里认领的 manager 是会话工厂各造一份、与实例里那份无关；两者
/// 不等这件事**全程静默**（不产生任何面向客户端的错误）。
///
/// 四条断言各自独立、各自可证伪：
/// ① **形态前提**：`workspace_assembly == None`（1:N = 部署单元唯一 pool 的形态本身）；
/// ② **无第二次注入尝试**：两个不同 cwd 的会话环境装配都返回 `Ok(None)`（断言的是
///    这个**返回值**，不是某个错误码）；
/// ③ **跨 session 单一部署单元**：两个 cwd 不同的 session 都成功建立、都不报错，且投影
///    **同一** host pool（`Arc::ptr_eq`，与 [`multi_cwd_degradation_and_host_shutdown`]
///    的 LSP 投影同一范式）；
/// ④ **静默串线**：root 送进 builtin 上下文的那份 manager（句柄由本用例自持）与两个
///    session 各自登记的 `AcpSession::task_manager` **都不是同一份**（`!Arc::ptr_eq`），
///    两个 session 之间也不是同一份。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn wave3_one_to_n_root_never_injects_per_session_workspace_input() {
    let dirs = FixtureDirs::new();

    // root 装配面送进 builtin 上下文的那份输入。manager 句柄由本用例自持：它是
    // 「builtin `Bash` 实际会用到的 manager」在本 crate 内**唯一**可观察的锚点
    // （池内 `BuiltinInstanceContext` 的读取面是 peri-middlewares 的 `pub(crate)`，
    // 宿主不可见——A33）。
    let root_manager: Arc<dyn peri_acp_types::tasks::TaskManager> =
        Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    let cfg = assemble_host_with_workspace_input(
        &dirs,
        false,
        Some(peri_mcp_workspace::WorkspaceInstanceInput {
            task_manager: Some(Arc::clone(&root_manager)),
            on_bg_complete: None,
        }),
    )
    .await;

    // ① 形态前提：1:N = 部署单元唯一 pool（`session_resources = true`）。
    assert!(
        cfg.workspace_assembly.is_none(),
        "本用例只在 1:N 形态成立（`session_resources = true` ⇒ `workspace_assembly` 为 None）；\
         为 Some 则每个 session 各自分裂出会话环境，断言②「恒 Ok(None)」即不成立"
    );
    assert!(
        cfg.mcp_pool.is_some(),
        "1:N 形态是唯一构造 builtin 上下文的形态 ⇒ 宿主必须持有 MCP 池（root 输入才落进实例）"
    );

    // ② 无第二次注入尝试：两个不同 cwd 的会话环境装配都返回 Ok(None)。
    let first_cwd = dirs.workspace.clone();
    let second_cwd = dirs.second_workspace();
    let cwds = [&first_cwd, &second_cwd];
    for (index, cwd) in cwds.iter().enumerate() {
        let cwd = cwd.to_string_lossy().into_owned();
        let assembled = crate::host::workspace::SessionEnvironment::assemble(
            &cfg,
            &cwd,
            &format!("wave3-u5-root-{index}"),
        )
        .await;
        let outcome = match &assembled {
            Ok(None) => "Ok(None)",
            Ok(Some(_)) => "Ok(Some(_))",
            Err(_) => "Err(_)",
        };
        assert!(
            matches!(assembled, Ok(None)),
            "1:N root 形态下任何 session 的会话环境装配都必须返回 Ok(None)\
             （⇒ 不存在第二个 session 的注入尝试）: cwd={cwd} outcome={outcome}"
        );
    }

    // ③ 跨 session 单一部署单元：两个不同 cwd 的 session 都成功、都不报错。
    let mut sessions = HashMap::new();
    let transport = idle_transport();
    let mut created = Vec::new();
    for cwd in cwds {
        let response = crate::host::requests::handle_request(
            "session/new",
            &json!({ "cwd": cwd }),
            &cfg,
            &mut sessions,
            &transport,
        )
        .await
        .expect("1:N 形态下 session/new 必须成功（root 输入的串线是静默的，不产生任何错误）");
        created.push(
            response["sessionId"]
                .as_str()
                .expect("session/new 必须回 sessionId（无错误响应）")
                .to_string(),
        );
    }
    assert_eq!(created.len(), 2);
    // ③ 的机制面（② 在**会话记录**上的投影）：两个 session 都没有「会话环境」⇒
    //    `ensure_session_with_task_manager` 的第三参恒为 `None` ⇒ ④ 里的两份 manager
    //    只能来自宿主装配注入的工厂。
    for session_id in &created {
        let state = sessions
            .get(session_id)
            .unwrap_or_else(|| panic!("{session_id} 必须已注册"));
        assert!(
            state.environment.is_none(),
            "1:N root 形态下 session 不得持有会话环境（有环境即会经第三参认领该环境的 manager）: {session_id}"
        );
    }
    let projected: Vec<Arc<dyn LspPoolPort>> = created
        .iter()
        .map(|session_id| {
            sessions
                .get(session_id)
                .unwrap_or_else(|| panic!("{session_id} 必须已注册"))
                .lsp_pool
                .clone()
                .unwrap_or_else(|| panic!("{session_id} 必须投影 host pool"))
        })
        .collect();
    assert!(
        Arc::ptr_eq(&projected[0], &projected[1]),
        "1:N 形态下两个 session 必须投影**同一** host 资源（部署单元唯一 pool）"
    );

    // ④ 静默串线（核心正向断言）：会话记录里认领 manager 的那处（会话工厂各造一份）
    //    与「builtin `Bash` 实际会用到的 manager」（root 输入那一份）是两份不同对象。
    let stored: Vec<(String, Arc<dyn peri_acp_types::tasks::TaskManager>)> = created
        .iter()
        .map(|session_id| {
            let session = cfg
                .session_manager
                .get_session(session_id)
                .unwrap_or_else(|| panic!("{session_id} 必须已登记 AcpSession 记录"));
            (session_id.clone(), Arc::clone(&session.task_manager))
        })
        .collect();
    for (session_id, manager) in &stored {
        assert!(
            !manager
                .as_any()
                .is::<peri_acp_types::tasks::NoopTaskManager>(),
            "session {session_id} 的 manager 必须来自宿主装配注入的工厂（真实 manager）——\
             两侧都是真实对象却互不相同，串线才是事实，而不是 fallback 退化产物"
        );
        assert!(
            !Arc::ptr_eq(manager, &root_manager),
            "session {session_id} 登记的 manager 不得是 root 送进 builtin 上下文的那一份：\
             相等即「会话记录认领的 manager」== 「builtin `Bash` 实际会用到的 manager」，\
             而 1:N 形态下没有把 root 输入交给 session 的路径（出现即真串线）"
        );
    }
    assert!(
        !Arc::ptr_eq(&stored[0].1, &stored[1].1),
        "1:N 形态下每个 session 由工厂各造一份 manager；两份互不相同进一步说明它们都不是\
         实例里那一份"
    );
    assert!(
        !root_manager
            .as_any()
            .is::<peri_acp_types::tasks::NoopTaskManager>(),
        "对照物自身不得退化：root 输入必须是真实 manager，否则上面的「不相等」无意义"
    );
    println!(
        "[W3-U5 补验] 1:N root：workspace_assembly=None、mcp_pool=Some；两个 cwd 的会话环境装配 = Ok(None)；\
         session = {created:?}（共享同一 host pool = {}）；root 输入 manager 与两个 session 的 manager \
         均非同一份 = {}（两 session 之间也互不相同 = {}）",
        Arc::ptr_eq(&projected[0], &projected[1]),
        stored
            .iter()
            .all(|(_, manager)| !Arc::ptr_eq(manager, &root_manager)),
        !Arc::ptr_eq(&stored[0].1, &stored[1].1)
    );
}

/// 主 plan §8 第 5 行（A21）的具名终态断言：`lsp` handler 的构造晚于配置合并。
///
/// 「晚于」的可观测判据不是时序，而是**同一份 pool 构造时已带合并后的配置**：装配点把
/// `load_merged_lsp_servers` 的结果直接喂给 `create_host_lsp_pool`，而 handler 在
/// `run_initialize` 内按 `has_servers()` 快照工具面（不支持热更新）。因此
/// 「装配后的 pool 有 server」⇔「handler 看到非空配置」；反向（无配置）同样必须成立：
/// 空配置 ⇒ `has_servers()` 假 ⇒ 工具面空表但实例仍 ready（A6）。
#[cfg(not(windows))]
#[tokio::test]
#[serial]
async fn lsp_handler_constructed_after_config_merge() {
    // A：settings.json 声明 1 个 server ⇒ 生效配置非空 ⇒ pool 构造时已登记该 server。
    let dirs = FixtureDirs::new();
    dirs.write_lsp_settings("wave2_merge_probe");
    let cfg = assemble_host(&dirs).await;
    let declared: Vec<&str> = cfg
        .plugin_lsp_servers
        .iter()
        .map(|server| server.name.as_str())
        .collect();
    assert_eq!(
        declared,
        vec!["wave2_merge_probe"],
        "生效配置必须来自宿主装配期的合并结果: {declared:?}"
    );
    let pool = cfg.lsp_pool.clone().expect("非空配置的 host pool");
    let concrete = Arc::clone(&pool)
        .downcast_arc::<LspServerPool>()
        .unwrap_or_else(|_| panic!("host pool 必须是 LspServerPool"));
    assert!(
        concrete.has_servers(),
        "配置合并在 pool 构造之前 ⇒ pool 必须已登记 server（handler 据此快照非空工具面）"
    );
    println!(
        "[W2-V03 终态] 配置合并：生效配置 = {declared:?}；pool 已登记 server = {}（handler 构造晚于合并）",
        concrete.has_servers()
    );

    // B：无 settings.json / 无插件 ⇒ 空配置也构造 pool，但工具面为空表。
    let empty_dirs = FixtureDirs::new();
    let empty_cfg = assemble_host(&empty_dirs).await;
    assert!(
        empty_cfg.plugin_lsp_servers.is_empty(),
        "无任何 LSP 配置时生效配置必须为空: {:?}",
        empty_cfg.plugin_lsp_servers
    );
    let empty_pool = empty_cfg
        .lsp_pool
        .clone()
        .expect("空配置也必须构造 host pool（A6：不得用「不构造」表达「无配置」）");
    let empty_concrete = Arc::clone(&empty_pool)
        .downcast_arc::<LspServerPool>()
        .unwrap_or_else(|_| panic!("host pool 必须是 LspServerPool"));
    assert!(
        !empty_concrete.has_servers(),
        "空配置 ⇒ has_servers() 为假 ⇒ lsp handler 工具面为空表（实例仍 ready）"
    );
    println!(
        "[W2-V03 终态] 配置合并：空配置 pool 已构造 = true；已登记 server = {}",
        empty_concrete.has_servers()
    );
}
