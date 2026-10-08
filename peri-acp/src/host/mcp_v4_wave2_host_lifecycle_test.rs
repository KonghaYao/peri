use super::*;

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
/// ③ **跨 session 单一部署单元**：两个 cwd 不同的 session 都成功建立、都不报错；
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
            default_run_in_background: false,
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
         session = {created:?}；root 输入 manager 与两个 session 的 manager \
         均非同一份 = {}（两 session 之间也互不相同 = {}）",
        stored
            .iter()
            .all(|(_, manager)| !Arc::ptr_eq(manager, &root_manager)),
        !Arc::ptr_eq(&stored[0].1, &stored[1].1)
    );
}
