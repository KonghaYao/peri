//! Dynamic MCP 的初始化分流（plan 4.2 / B2）：
//! 配置照常登记，但首次连接、进程启动与进度分母只针对 eager 服务器。

use super::{Arc, ClientStatus, McpClientPool, McpInitStatus, SystemMcpManifest};

/// 全部 Dynamic：初始化必须正常收口，只登记配置，不启动任何进程。
///
/// 分母只统计需要首次连接的服务器——全 lazy 时 `total = 0`；否则模型会等待
/// 一个永远不会出现的连接进度。System 依赖为空时也必须在启动批次结束即 settled。
#[tokio::test]
async fn all_dynamic_servers_register_without_connecting() {
    let fixture = tempfile::tempdir().unwrap();
    let cwd = fixture.path().join("project");
    std::fs::create_dir(&cwd).unwrap();
    std::fs::write(cwd.join("server.js"), super::SCRIPT).unwrap();
    let config: crate::mcp::config::McpConfigFile = serde_json::from_value(serde_json::json!({
        "mcpServers": {
            "lazy-a": { "command": "node", "args": ["server.js"], "dynamic": true },
            "lazy-b": { "command": "node", "args": ["server.js"], "dynamic": true },
        }
    }))
    .unwrap();
    let (mut tasks, spawner) = crate::mcp::task_scope::McpTaskOwner::new();
    let pool = Arc::new(McpClientPool::new_pending_with_spawner(spawner));
    let (status_tx, status_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);
    McpClientPool::initialize_config(
        pool.clone(),
        &cwd,
        config,
        Default::default(),
        status_tx,
        None,
    )
    .await;

    assert!(
        matches!(&*pool.init_status.read(), McpInitStatus::Ready { total: 0 }),
        "全 dynamic 必须收口为 Ready 且分母为 0，实际: {:?}",
        *pool.init_status.read()
    );
    assert!(
        matches!(&*status_rx.borrow(), McpInitStatus::Ready { total: 0 }),
        "watch 通道必须发布同一收口，实际: {:?}",
        *status_rx.borrow()
    );
    for name in ["lazy-a", "lazy-b"] {
        assert!(
            pool.client(name).is_none(),
            "{name} 不得写入连接句柄（说明首次连接被触发）"
        );
        let configs = pool.configs.read();
        let registered = configs
            .get(name)
            .unwrap_or_else(|| panic!("{name} 必须登记到本地配置表"));
        assert!(registered.dynamic, "{name} 的 dynamic 标记必须保留");
        assert!(
            registered.disabled != Some(true),
            "{name} 未声明 disabled，不得被当成禁用"
        );
    }
    assert!(
        !cwd.join("starts").exists(),
        "dynamic 不得启动 stdio 进程（starts 标记文件不应出现）"
    );
    let infos = pool.all_server_infos();
    for name in ["lazy-a", "lazy-b"] {
        let info = infos
            .iter()
            .find(|info| info.name == name)
            .unwrap_or_else(|| panic!("{name} 必须出现在服务器清单中"));
        assert!(
            matches!(info.status, ClientStatus::Uninitialized),
            "{name} 的登记态应为 Uninitialized，实际: {:?}",
            info.status
        );
    }
    assert!(
        pool.system_settled(),
        "无系统依赖时启动批次必须立即 settled"
    );
    assert!(
        pool.initialized.load(std::sync::atomic::Ordering::SeqCst),
        "全 dynamic 也是完整初始化，必须允许模型开始 Reason"
    );

    pool.begin_shutdown();
    tasks.begin_shutdown();
    let _ = tasks.shutdown().await;
    assert!(pool.shutdown().await.is_complete());
}

/// 混合配置：eager 正常连接并计入进度，dynamic 只登记且不计入分母。
#[tokio::test]
async fn mixed_dynamic_and_eager_only_eager_connects() {
    let fixture = tempfile::tempdir().unwrap();
    let cwd = fixture.path().join("project");
    std::fs::create_dir(&cwd).unwrap();
    std::fs::write(cwd.join("server.js"), super::SCRIPT).unwrap();
    let config: crate::mcp::config::McpConfigFile = serde_json::from_value(serde_json::json!({
        "mcpServers": {
            "eager-srv": { "command": "node", "args": ["server.js"] },
            "lazy-srv": { "command": "node", "args": ["server.js"], "dynamic": true },
        }
    }))
    .unwrap();
    let (mut tasks, spawner) = crate::mcp::task_scope::McpTaskOwner::new();
    let pool = Arc::new(McpClientPool::new_pending_with_spawner(spawner));
    let (status_tx, status_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);
    McpClientPool::initialize_config(
        pool.clone(),
        &cwd,
        config,
        Default::default(),
        status_tx,
        None,
    )
    .await;

    let eager = pool.client("eager-srv").expect("eager 必须完成首次连接");
    assert!(
        matches!(eager.status, ClientStatus::Connected),
        "eager 必须 Connected，实际: {:?}",
        eager.status
    );
    assert!(
        pool.client("lazy-srv").is_none(),
        "dynamic 不得写入连接句柄"
    );
    assert!(
        pool.configs
            .read()
            .get("lazy-srv")
            .is_some_and(|config| config.dynamic),
        "dynamic 配置必须保留在本地登记表"
    );
    assert_eq!(
        super::discovery_lines(&cwd.join("starts")),
        1,
        "只有 eager 服务器允许启动进程，实际启动次数见 starts 文件"
    );
    assert!(
        matches!(&*pool.init_status.read(), McpInitStatus::Ready { total: 1 }),
        "分母只统计需要首次连接的服务器（1 个 eager），实际: {:?}",
        *pool.init_status.read()
    );
    assert!(
        matches!(&*status_rx.borrow(), McpInitStatus::Ready { total: 1 }),
        "watch 通道的分母同样只统计 eager，实际: {:?}",
        *status_rx.borrow()
    );
    let infos = pool.all_server_infos();
    let lazy = infos
        .iter()
        .find(|info| info.name == "lazy-srv")
        .expect("dynamic 仍需出现在登记清单中");
    assert!(
        matches!(lazy.status, ClientStatus::Uninitialized),
        "dynamic 的登记态应为 Uninitialized，实际: {:?}",
        lazy.status
    );

    pool.begin_shutdown();
    tasks.begin_shutdown();
    let _ = tasks.shutdown().await;
    assert!(pool.shutdown().await.is_complete());
}

/// dynamic + system_mcp：系统前置依赖不能被延迟加载，配置校验必须先行失败。
///
/// 失败发生在注册与连接之前：不写配置表、不启动进程，也不发布 Ready。
#[tokio::test]
async fn dynamic_system_conflict_fails_before_registration() {
    let fixture = tempfile::tempdir().unwrap();
    let cwd = fixture.path().join("project");
    std::fs::create_dir(&cwd).unwrap();
    std::fs::write(cwd.join("server.js"), super::SCRIPT).unwrap();
    let config: crate::mcp::config::McpConfigFile = serde_json::from_value(serde_json::json!({
        "mcpServers": {
            "conflict": {
                "command": "node",
                "args": ["server.js"],
                "dynamic": true,
                "system_mcp": true,
                "system_mcp_tools": [],
            },
        }
    }))
    .unwrap();
    let (mut tasks, spawner) = crate::mcp::task_scope::McpTaskOwner::new();
    let pool = Arc::new(McpClientPool::new_pending_with_spawner(spawner));
    let (status_tx, status_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);
    McpClientPool::initialize_config(
        pool.clone(),
        &cwd,
        config,
        Default::default(),
        status_tx,
        None,
    )
    .await;

    for status in [&*pool.init_status.read(), &*status_rx.borrow()] {
        let McpInitStatus::Failed(message) = status else {
            panic!("系统冲突必须发布 Failed，实际: {status:?}");
        };
        assert!(
            message.contains("conflict"),
            "失败必须携带来源 server 名，实际: {message}"
        );
        assert!(
            message.contains("dynamic = true cannot be combined with system_mcp = true"),
            "失败必须保留固定规则正文，实际: {message}"
        );
    }
    assert!(
        pool.clients.read().is_empty(),
        "系统冲突不得注册连接句柄（未开始 transport）"
    );
    assert!(
        pool.configs.read().is_empty(),
        "系统冲突不得把非法配置写入本地登记表"
    );
    assert!(!cwd.join("starts").exists(), "系统冲突不得启动 stdio 进程");
    assert!(
        !pool.initialized.load(std::sync::atomic::Ordering::SeqCst),
        "系统冲突不得标记 initialized"
    );
    assert_eq!(
        pool.system_manifest(),
        SystemMcpManifest::Failed,
        "系统依赖清单必须收口为 Failed"
    );
    assert!(
        pool.system_settled(),
        "配置失败也是系统依赖终态，等待方必须立即得到结论"
    );

    pool.begin_shutdown();
    tasks.begin_shutdown();
    let _ = tasks.shutdown().await;
    assert!(pool.shutdown().await.is_complete());
}
