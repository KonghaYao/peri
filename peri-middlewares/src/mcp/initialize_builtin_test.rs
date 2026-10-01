use super::*;

// ── builtin 分支（E-03，W2）────────────────────────────────────────────────────
//
// builtin 条目**只能**由运行时标记 `source = ConfigSource::Builtin` 产生
// （`#[serde(skip)]`，配置文件无法构造），因此这些用例直接构造 typed 配置并调用
// `initialize_config` / `reconnect`，不经 `.mcp.json`。

/// builtin 默认层的条目形状：无 command / url，身份来自 `source`。
fn builtin_server_config(instance: &str) -> crate::mcp::config::McpServerConfig {
    crate::mcp::config::McpServerConfig {
        command: None,
        args: None,
        env: None,
        url: None,
        headers: None,
        oauth: None,
        disabled: None,
        protocol_version: None,
        subscriptions: None,
        system_mcp: Some(true),
        system_mcp_tools: Some(Vec::new()),
        system_mcp_timeout: None,
        source: Some(peri_acp_types::plugin::ConfigSource::Builtin {
            instance: instance.to_string(),
        }),
    }
}

fn builtin_config_file(
    servers: Vec<(&str, crate::mcp::config::McpServerConfig)>,
) -> crate::mcp::config::McpConfigFile {
    let mut config = crate::mcp::config::McpConfigFile::default();
    for (name, server) in servers {
        config.mcp_servers.insert(name.to_string(), server);
    }
    config
}

/// 「注册表里不存在」的实例名：本文件两条用例的**未知实例**输入。
///
/// v4-part-4（wave 3）起 `workspace` 已是**已实现**实例（`BUILTIN_MCP_INSTANCES` 第五项），
/// 不能再拿它当未注册名——那会改走 `ContextMissing` / `InstanceInputMissing` 分支，把
/// 「未注册名必须 typed 拒绝」这一不变量测成别的东西。未知名的唯一事实源仍是注册表：
/// 任何 `find` 不命中的良构名字都走 `UnknownBuiltinInstance`。
const UNREGISTERED_INSTANCE: &str = "not-a-builtin";

/// 未注册的 builtin 实例名：typed 失败 + 本代失败证据，**不**发布 ready。
///
/// 实例解析的唯一事实源是注册表（`builtin_mcp::find` 只命中已实现实例）：名字不在
/// 注册表内时不得建立传输，也不得降级成 stdio / http（那会把「未接线」伪装成可用）。
#[tokio::test]
async fn builtin_unregistered_instance_fails_without_ready_evidence() {
    let fixture = tempfile::tempdir().unwrap();
    let cwd = fixture.path().join("project");
    std::fs::create_dir(&cwd).unwrap();
    let pool = Arc::new(McpClientPool::new_pending());
    let (status_tx, _status_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);

    McpClientPool::initialize_config(
        pool.clone(),
        &cwd,
        builtin_config_file(vec![(
            UNREGISTERED_INSTANCE,
            builtin_server_config(UNREGISTERED_INSTANCE),
        )]),
        Default::default(),
        status_tx,
        None,
    )
    .await;

    let handle = pool
        .get_client(UNREGISTERED_INSTANCE)
        .expect("失败必须留下可核对的句柄，而不是空 entry");
    let ClientStatus::Failed(reason) = &handle.status else {
        panic!("未注册实例必须显式 Failed，实际: {:?}", handle.status);
    };
    assert!(
        reason.contains(&format!("builtin MCP 实例未注册: {UNREGISTERED_INSTANCE}")),
        "失败原因必须保留 typed 规则文本，实际: {reason}"
    );
    assert!(
        !reason.contains(&cwd.display().to_string()),
        "错误文本不得包含路径，实际: {reason}"
    );
    let evidence = pool
        .discovery_evidence(UNREGISTERED_INSTANCE)
        .expect("失败收口必须提交本代证据");
    assert!(
        !evidence.initialize_ok && !evidence.tools_list_ok,
        "启动失败不得留下任何 ready 证据，实际: {evidence:?}"
    );
    let status = pool.init_status.read().clone();
    match &status {
        McpInitStatus::Failed(message) => assert!(
            message.contains(UNREGISTERED_INSTANCE),
            "启动状态必须是 Failed 且指名实例，实际: {message}"
        ),
        other => panic!("必须发布 Failed，实际: {other:?}"),
    }
}

/// reconnect 的 builtin 分支：实例上下文未注入时 typed 失败（**不** fallback 到进程 cwd）。
///
/// 与 stdio 路径「执行目录未绑定」是同一保护意图（不得拿进程 cwd 凑合），但事实源已随
/// A33 换成宿主注入的上下文：`artifact` 的相对路径解析根是
/// `BuiltinInstanceContext::cwd`，不再从 pool 的 `execution_cwd` 现取。
#[tokio::test]
async fn reconnect_builtin_without_injected_context_is_typed_failure() {
    let pool = Arc::new(McpClientPool::new_pending());
    pool.configs
        .write()
        .insert("artifact".to_string(), builtin_server_config("artifact"));

    let result = pool.reconnect("artifact", None).await;
    let Err(crate::mcp::client::McpPoolError::ConnectionFailed { server, reason }) = result else {
        panic!("未注入实例上下文必须失败，实际: {result:?}");
    };
    assert_eq!(server, "artifact");
    assert!(
        reason.contains("builtin 实例上下文未注入: artifact"),
        "失败原因必须逐字保留 typed 规则文本，实际: {reason}"
    );
    // 与 stdio 路径同构：该分支在建立传输之前就失败收口（`insert_failed` +
    // `commit_discovery_failure`）——失败证据必须提交，不得伪装成「无结论」或成功。
    let evidence = pool
        .discovery_evidence("artifact")
        .expect("失败收口必须提交本代证据");
    assert!(!evidence.is_complete(), "失败不得构成完整证据");
    assert_eq!(pool.builtin_task_count(), 0, "未建立链路不得登记 task");
}

/// reconnect 的 builtin 分支：未注册实例名 + 已绑定目录 → typed `ConnectionFailed`，
/// 并且不留下任何 ready 证据。
#[tokio::test]
async fn reconnect_builtin_unregistered_instance_records_failure() {
    let fixture = tempfile::tempdir().unwrap();
    let cwd = fixture.path().join("project");
    std::fs::create_dir(&cwd).unwrap();
    let pool = Arc::new(McpClientPool::new_pending());
    pool.bind_execution_cwd(&cwd).unwrap();
    pool.configs.write().insert(
        UNREGISTERED_INSTANCE.to_string(),
        builtin_server_config(UNREGISTERED_INSTANCE),
    );

    let result = pool.reconnect(UNREGISTERED_INSTANCE, None).await;
    let Err(crate::mcp::client::McpPoolError::ConnectionFailed { server, reason }) = result else {
        panic!("未注册实例必须失败，实际: {result:?}");
    };
    assert_eq!(server, UNREGISTERED_INSTANCE);
    assert!(
        reason.contains(&format!("builtin MCP 实例未注册: {UNREGISTERED_INSTANCE}")),
        "实际: {reason}"
    );
    let evidence = pool
        .discovery_evidence(UNREGISTERED_INSTANCE)
        .expect("失败收口必须提交本代证据");
    assert!(
        !evidence.is_complete(),
        "失败不得构成完整证据，实际: {evidence:?}"
    );
    assert_eq!(pool.builtin_task_count(), 0, "失败路径不得登记 task");
}

/// 已实现的 builtin 实例经**生产**初始化链完整落地：transport → 握手 → live
/// `tools/list` → `Connected` 句柄，且 server task 已登记（关闭时有归属、无 orphan）。
///
/// 本用例扮演宿主装配：按 A33，实例上下文必须在 `initialize_config` 之前注入。
#[tokio::test]
async fn builtin_instance_completes_initialization_and_registers_task() {
    let fixture = tempfile::tempdir().unwrap();
    let cwd = fixture.path().join("project");
    std::fs::create_dir(&cwd).unwrap();
    let pool = Arc::new(McpClientPool::new_pending());
    pool.set_builtin_instance_context(Arc::new(BuiltinInstanceContext::new(
        cwd.to_string_lossy().to_string(),
    )))
    .expect("宿主首次注入上下文必须成功");
    let (status_tx, _status_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);

    McpClientPool::initialize_config(
        pool.clone(),
        &cwd,
        builtin_config_file(vec![("web", builtin_server_config("web"))]),
        Default::default(),
        status_tx,
        None,
    )
    .await;

    let handle = pool
        .get_client("web")
        .expect("已实现的 builtin 实例必须完成连接");
    assert!(
        matches!(handle.status, ClientStatus::Connected),
        "builtin 实例必须经同一条处理链提交 Connected，实际: {:?}",
        handle.status
    );
    assert_eq!(
        handle
            .tools
            .iter()
            .map(|tool| tool.name.as_ref())
            .collect::<Vec<_>>(),
        vec!["WebSearch", "WebFetch"],
        "live tools/list 必须返回真实 handler 声明的工具"
    );
    assert!(
        matches!(
            handle.source,
            Some(peri_acp_types::plugin::ConfigSource::Builtin { .. })
        ),
        "句柄必须保留 builtin 身份（面板与 discover 工具的来源标签依赖它）"
    );
    assert!(handle.url.is_none(), "builtin 实例无 URL / 无凭据");
    let evidence = pool
        .discovery_evidence("web")
        .expect("成功必须提交本代完整发现证据");
    assert!(
        evidence.is_complete(),
        "只有真实成功的 live tools/list 才构成完整证据，实际: {evidence:?}"
    );
    assert_eq!(pool.builtin_task_count(), 1, "server task 必须已登记");

    let status = pool.init_status.read().clone();
    assert!(
        matches!(status, McpInitStatus::Ready { total: 1 }),
        "初始化必须收口为 Ready，实际: {status:?}"
    );

    pool.begin_shutdown();
    let _ = pool.shutdown().await;
    assert_eq!(
        pool.builtin_task_count(),
        0,
        "pool 关闭后不得留下 orphan server task"
    );
}

/// reconnect 的 builtin **成功**路径：重建全新链路（新 task + 新代际），旧 task 收敛。
///
/// 「全新」是可观察的：代际前进、发现证据换代、task 表仍恰好一条（旧条目已被移除并
/// 收敛，不是被覆盖后泄漏）。
#[tokio::test]
async fn reconnect_builtin_replaces_link_and_advances_generation() {
    let fixture = tempfile::tempdir().unwrap();
    let cwd = fixture.path().join("project");
    std::fs::create_dir(&cwd).unwrap();
    let pool = Arc::new(McpClientPool::new_pending());
    // A33：上下文与初始化同源（同一 host cwd），reconnect 复用同一份上下文。
    pool.set_builtin_instance_context(Arc::new(BuiltinInstanceContext::new(
        cwd.to_string_lossy().to_string(),
    )))
    .expect("宿主首次注入上下文必须成功");
    let (status_tx, _status_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);
    McpClientPool::initialize_config(
        pool.clone(),
        &cwd,
        builtin_config_file(vec![("artifact", builtin_server_config("artifact"))]),
        Default::default(),
        status_tx,
        None,
    )
    .await;

    let first = pool.get_client("artifact").expect("首轮必须完成连接");
    let first_generation = pool.handle_generation(&first);
    let first_evidence = pool
        .discovery_evidence("artifact")
        .expect("首轮成功必须提交本代证据");
    assert!(first_evidence.is_complete());

    pool.reconnect("artifact", None)
        .await
        .expect("已实现 builtin 实例的重连必须成功");

    let second = pool.get_client("artifact").expect("重连成功后必须有句柄");
    assert!(
        matches!(second.status, ClientStatus::Connected),
        "重连后必须仍为 Connected，实际: {:?}",
        second.status
    );
    assert_ne!(
        pool.handle_generation(&second),
        first_generation,
        "重连必须换新句柄（代际前进）"
    );
    let second_evidence = pool
        .discovery_evidence("artifact")
        .expect("重连成功必须提交本代证据");
    assert!(second_evidence.is_complete());
    assert_ne!(
        second_evidence.generation, first_evidence.generation,
        "重连的本代证据不得复用旧代"
    );
    assert_eq!(
        pool.builtin_task_count(),
        1,
        "旧 server task 必须已移除并收敛，新 task 恰好一条"
    );

    pool.begin_shutdown();
    let _ = pool.shutdown().await;
    assert_eq!(pool.builtin_task_count(), 0);
}
