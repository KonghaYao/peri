//! M5 follow-up：恢复准入里「请求声明 servers 候选 → 校验 → 持久绑定」的顺序。
//!
//! 候选必须在 bootstrap 前可用（会话 MCP 池只在装配时消费一次 servers），但不可变
//! owner 声明只能在本请求通过 cwd/frozen/identity 校验之后落盘：失败请求不得污染
//! 声明，否则该会话后续不同的合法声明会被误判为篡改。本模块用真实门面（同一库句柄）
//! 与真实装配观察者证明这两条保证。

use super::*;
use crate::host::requests::resource_owners;
use peri_acp_types::session_resources::{work::WorkQuery, BindingState, FrozenState};

fn declared_http(name: &str, url: &str) -> Value {
    serde_json::to_value(agent_client_protocol_schema::v1::McpServer::Http(
        agent_client_protocol_schema::v1::McpServerHttp::new(name, url),
    ))
    .unwrap()
}

/// 持久 owner 声明的 canonical JSON（无声明时为 None）。
async fn persisted_declaration(cfg: &AcpServerConfig, id: &str) -> Option<String> {
    cfg.session_resources
        .load_resource_owner_facts(&id.to_owned(), 0)
        .await
        .unwrap()
        .current_owner
        .map(|owner| owner.connections_json)
}

fn session_workspace_assembly(cwd: &Path) -> crate::host::assemble::WorkspaceAssembly {
    crate::host::assemble::WorkspaceAssembly {
        startup_cwd: cwd.to_str().unwrap().to_owned(),
        // 非 bare：请求声明的 MCP server 才进入装配面（bare 只留 builtin workspace）。
        bare: false,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
        capabilities: Default::default(),
    }
}

/// 失败请求（这里是读不懂的 frozen，解码顺序晚于候选解析）不得写 owner 声明。
#[tokio::test]
#[serial]
async fn legacy_unreadable_frozen_failure_does_not_write_owner_declaration() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let cwd = std::fs::canonicalize(tmp.path())
        .unwrap()
        .join("owner-frozen");
    std::fs::create_dir(&cwd).unwrap();
    let config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "test", "model"));
    let (mut cfg, bridge) = make_server_config_with_bridge(
        config.clone(),
        LlmProvider::from_config(&config).unwrap(),
        &tmp,
    )
    .await;
    cfg.workspace_assembly = Some(session_workspace_assembly(&cwd));
    let id = legacy_tests::old_thread(&bridge, &cwd).await;
    bridge
        .store_frozen_snapshot_if_absent(&id, "broken")
        .await
        .unwrap();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    let error = handle_request(
        "session/load",
        &json!({
            "sessionId": id,
            "mcpServers": [declared_http("declared", "http://127.0.0.1:9/mcp")]
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect_err("坏 frozen 必须拒绝恢复");
    assert!(
        error.message.contains("frozen snapshot"),
        "原始错误原因必须保留: {error:?}"
    );
    assert!(sessions.is_empty(), "失败不得发布会话");
    assert!(
        persisted_declaration(&cfg, &id).await.is_none(),
        "frozen 解码失败（晚于候选解析）不得写入不可变 owner 声明"
    );
}

/// 失败请求之后，另一个**不同的**合法声明仍必须能被接纳并成为持久声明。
///
/// 这是 M5 follow-up 的核心回归：若失败请求先写 owner（或成功请求不落盘），
/// 后者就会分别被误判为篡改 / 声明丢失。
#[tokio::test]
#[serial]
async fn legacy_failed_request_then_different_declaration_is_admitted() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let cwd = std::fs::canonicalize(tmp.path())
        .unwrap()
        .join("owner-order");
    let config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "test", "model"));
    let (cfg, bridge) = make_server_config_with_bridge(
        config.clone(),
        LlmProvider::from_config(&config).unwrap(),
        &tmp,
    )
    .await;
    // 目录尚不存在：第一次 session/load 会在执行资格解析处失败（可修复的请求级失败）。
    // 不安装 workspace_assembly：本用例只观察准入 seam（声明顺序），装配面由
    // `legacy_bootstrap_applies_request_declared_mcp_servers` 覆盖。
    let id = legacy_tests::old_thread(&bridge, &cwd).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    // 请求 A：声明 rejected 候选；执行资格失败 ⇒ 不得留下任何声明。
    let error = handle_request(
        "session/load",
        &json!({
            "sessionId": id,
            "mcpServers": [declared_http("declared-a", "http://127.0.0.1:9/mcp")]
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect_err("缺失执行目录的 legacy 恢复必须失败");
    assert!(!error.message.is_empty(), "原始错误原因必须保留: {error:?}");
    assert!(sessions.is_empty(), "失败不得发布会话");
    assert!(
        persisted_declaration(&cfg, &id).await.is_none(),
        "失败请求不得把候选写成不可变声明"
    );
    // 修复执行资格后，请求 B 声明**不同**的合法 servers：必须被接纳。
    std::fs::create_dir(&cwd).unwrap();
    std::fs::write(cwd.join("CLAUDE.md"), "OWNER_ORDER_PROJECT").unwrap();
    handle_request(
        "session/load",
        &json!({
            "sessionId": id,
            "mcpServers": [declared_http("declared-b", "http://127.0.0.1:9/mcp")]
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("不同合法声明在失败请求之后仍必须被接纳");
    let declaration = persisted_declaration(&cfg, &id)
        .await
        .expect("接纳成功必须把有效 servers 定稿为持久声明（下次恢复靠它取回 MCP 面）");
    assert!(
        declaration.contains("declared-b"),
        "持久声明必须是请求 B 的合法集合: {declaration}"
    );
    assert!(
        !declaration.contains("declared-a"),
        "失败请求的候选不得混入持久声明: {declaration}"
    );
    // 失败请求留下的旧候选不得混入；本夹具不装配资源面（见上），声明持久化即观测面。
    assert!(sessions[&id].environment.is_none());
    assert!(sessions[&id]
        .frozen
        .as_ref()
        .is_some_and(|frozen| frozen.claude_md().is_none()));
}

/// 竞态 fixture：同一 legacy 会话已被并发赢家发布到 live 表时，输家的候选环境必须
/// 排空且不得替换 live 状态；请求声明的候选不写回（live 环境已经定稿）。
#[tokio::test]
#[serial]
async fn legacy_already_live_race_drains_candidate_environment() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = HomeDirGuard::set(tmp.path());
    let cwd = std::fs::canonicalize(tmp.path())
        .unwrap()
        .join("owner-race");
    std::fs::create_dir(&cwd).unwrap();
    let config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "test", "model"));
    let (mut cfg, bridge) = make_server_config_with_bridge(
        config.clone(),
        LlmProvider::from_config(&config).unwrap(),
        &tmp,
    )
    .await;
    cfg.workspace_assembly = Some(session_workspace_assembly(&cwd));
    let id = legacy_tests::old_thread(&bridge, &cwd).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    // 竞态中间态：持久事实仍未绑定（赢家尚未落盘），但 live 表已有会话。
    let mut sessions = HashMap::new();
    sessions.insert(
        id.clone(),
        crate::host::SessionState {
            session_id: id.clone(),
            thread_id: id.clone(),
            cwd: cwd.to_str().unwrap().to_owned(),
            environment: None,
            closing: false,
            history: Vec::new(),
            history_payloads: Vec::new(),
            cancel_token: None,
            frozen: None,
            recall_items: Vec::new(),
            agent_pool: crate::session::agent_pool::AgentPool::new(),
            workflow_middleware: None,
            title: None,
            tags: Vec::new(),
        },
    );
    let _ = crate::host::workspace::take_shutdown_observations();
    handle_request(
        "session/load",
        &json!({
            "sessionId": id,
            "mcpServers": [declared_http("declared-race", "http://127.0.0.1:9/mcp")]
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("已 live 的会话必须按 AlreadyLive 成功返回");
    let shutdowns = crate::host::workspace::take_shutdown_observations();
    assert!(
        shutdowns.iter().any(|session| session == &id),
        "AlreadyLive 必须排空竞态候选环境（观察到的 shutdown: {shutdowns:?}）"
    );
    // live 状态未被替换：环境保持 None（未被候选环境接管），历史按需补齐。
    assert!(
        sessions[&id].environment.is_none(),
        "live 状态不得被候选替换"
    );
    assert_eq!(sessions[&id].history[0].content(), "legacy user message");
    assert!(
        persisted_declaration(&cfg, &id).await.is_none(),
        "live 环境已定稿，请求声明不得回写为持久 owner"
    );
    let snapshot = cfg
        .session_resources
        .load_session_snapshot(&id.to_owned())
        .await
        .unwrap();
    // write-once 接纳事实不回滚（赢家/输家字节一致；此处仅确认可解释的绑定事实存在）。
    assert!(
        matches!(snapshot.binding, BindingState::Bound(_)),
        "接纳是 write-once 原子事实，必须留下可解释绑定"
    );
    assert!(matches!(snapshot.frozen, FrozenState::Present(_)));
    let work = cfg
        .session_resources
        .load_session_work(&WorkQuery {
            session_id: id.clone(),
            limit: 1,
        })
        .await
        .unwrap();
    assert!(
        !work
            .state
            .resource_owners
            .contains_key(&work.control.lifecycle),
        "竞态输家不得写 owner 声明"
    );
    // 便于复用 resource_owners 读路径做同一事实的复核。
    assert!(resource_owners::load(&cfg, &id).await.is_err());
}
