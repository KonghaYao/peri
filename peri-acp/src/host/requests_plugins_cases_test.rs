use super::*;

/// 等待 `session/update` 通知达到目标条数（on_change 经 tokio::spawn
/// 异步发送 → 轮询短等待，对齐 A5 重发测试先例）。仅计数 session/update：
/// 未协商 caps 回退 all_enabled（`unstable_event: true`），install/uninstall
/// 还会发 `peri/unstable_event`（plugin-action-result / plugin-snapshot）。
fn session_update_count(transport: &MockTransport) -> usize {
    transport
        .notifications()
        .iter()
        .filter(|(m, _)| m == "session/update")
        .count()
}

async fn wait_for_session_updates(transport: &MockTransport, target: usize) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while session_update_count(transport) < target {
        assert!(
            std::time::Instant::now() < deadline,
            "等待通知超时（target={target}）"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    // 稳定窗口：异步重发落袋后再等一拍（防迟到的重复重发漏判）
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
}

fn plugin_entries(registry: &peri_acp_types::command::CommandRegistry) -> Vec<String> {
    registry
        .snapshot()
        .iter()
        .filter(|e| e.fullname.to_lowercase().starts_with("plugin:"))
        .map(|e| e.fullname.clone())
        .collect()
}

/// B3 成功分支（install 调用路径 :907）：mock install 成功 + 磁盘重载出
/// `plugin:ecc:deploy` → 注册表投影含新插件命令（provenance 剥离前缀，
/// P0-1 回归）+ 注册表 on_change 触发投影推送**恰一次**。
#[tokio::test]
#[serial]
async fn test_plugin_install_refreshes_plugin_domain_and_pushes_once() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let _home_guard = HomeDirGuard::set(&home);
    seed_plugin_ecc(&home);

    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let mut cfg = make_server_config(peri_config, provider, &tmp).await;
    cfg.plugin_manager = Arc::new(MockPluginManager::install_ok("ecc"));
    let mut sessions = HashMap::new();
    let transport: Arc<MockTransport> = Arc::new(MockTransport::default());
    let transport_dyn: Arc<dyn crate::transport::AcpTransport> = transport.clone();

    let result = handle_request(
        "session/new",
        &json!({ "cwd": tmp.path().to_str().unwrap() }),
        &cfg,
        &mut sessions,
        &transport_dyn,
    )
    .await
    .unwrap();
    let sid = result["sessionId"].as_str().unwrap().to_string();
    super::session_lifecycle::after_new_response(&cfg, &transport_dyn, &sid).await;
    assert_eq!(
        transport.notifications().len(),
        1,
        "session/new response 后首发仅 available_commands_update 一条"
    );
    let registry = cfg
        .session_manager
        .command_registry_for(&sid)
        .expect("session 应持有命令注册表");
    assert!(
        plugin_entries(&registry).is_empty(),
        "初始 plugin 域应为空（无插件命令）"
    );

    // Act：plugin/install（mock 成功）
    let resp = handle_request(
        "plugin/install",
        &json!({
            "sessionId": sid,
            "name": "ecc",
            "marketplace": "test-mkt",
        }),
        &cfg,
        &mut sessions,
        &transport_dyn,
    )
    .await
    .unwrap();
    assert_eq!(resp["success"], true, "install RPC 应成功");
    assert_eq!(resp["plugin"], "ecc");

    // Assert ①：注册表投影含新插件命令，provenance 剥离 plugin: 前缀
    let entries = plugin_entries(&registry);
    assert_eq!(entries, vec!["plugin:ecc:deploy"]);
    let entry = registry
        .snapshot()
        .into_iter()
        .find(|e| e.fullname == "plugin:ecc:deploy")
        .expect("插件命令应已注册");
    use peri_acp_types::command::command_route::{
        CommandEntryKind, CommandLifecycle, CommandSource as RouteCommandSource,
    };
    assert_eq!(entry.kind, CommandEntryKind::Command);
    assert_eq!(entry.provenance.lifecycle, CommandLifecycle::Connected);
    match &entry.provenance.source {
        RouteCommandSource::Plugin { name } => {
            assert_eq!(name, "ecc", "plugin: 前缀必须剥离，实际: {name}");
        }
        other => panic!("source 应为 Plugin，实际: {other:?}"),
    }

    // Assert ②：注册表 on_change → 投影推送恰一次（首发 + 重发）
    wait_for_session_updates(&transport, 2).await;
    let notifications = transport.notifications();
    let updates: Vec<_> = notifications
        .iter()
        .filter(|(m, _)| m == "session/update")
        .collect();
    assert_eq!(
        updates.len(),
        2,
        "install 后注册表 on_change 应触发投影重发恰一次"
    );
    let commands = updates[1].1["update"]["availableCommands"]
        .as_array()
        .expect("重发载荷应含 availableCommands");
    assert!(
        commands.iter().any(|c| c["name"] == "plugin:ecc:deploy"),
        "投影推送应含插件命令条目"
    );
}

/// B3 失败分支（install 调用路径 :907）：磁盘重载失败（installed_plugins.json
/// 非法 JSON → `load_enabled_plugins` Err）→ 保留空 plugin 域 + 告警，
/// **不阻塞 RPC 回包**（`{success: true}` 仍回），plugin 域无变化不触发推送。
#[tokio::test]
#[serial]
async fn test_plugin_install_reload_failure_keeps_domain_empty_and_does_not_block_rpc() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let claude_dir = home.join(".claude");
    std::fs::create_dir_all(claude_dir.join("plugins")).unwrap();
    let _home_guard = HomeDirGuard::set(&home);
    // 重载失败形态：installed_plugins.json 内容非法
    std::fs::write(
        claude_dir.join("plugins").join("installed_plugins.json"),
        "{invalid json",
    )
    .unwrap();

    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let mut cfg = make_server_config(peri_config, provider, &tmp).await;
    cfg.plugin_manager = Arc::new(MockPluginManager::install_ok("ecc"));
    let mut sessions = HashMap::new();
    let transport: Arc<MockTransport> = Arc::new(MockTransport::default());
    let transport_dyn: Arc<dyn crate::transport::AcpTransport> = transport.clone();

    let result = handle_request(
        "session/new",
        &json!({ "cwd": tmp.path().to_str().unwrap() }),
        &cfg,
        &mut sessions,
        &transport_dyn,
    )
    .await
    .unwrap();
    let sid = result["sessionId"].as_str().unwrap().to_string();
    super::session_lifecycle::after_new_response(&cfg, &transport_dyn, &sid).await;
    let registry = cfg
        .session_manager
        .command_registry_for(&sid)
        .expect("session 应持有命令注册表");

    // Act：plugin/install —— 重载失败不得阻塞回包
    let resp = handle_request(
        "plugin/install",
        &json!({
            "sessionId": sid,
            "name": "ecc",
            "marketplace": "test-mkt",
        }),
        &cfg,
        &mut sessions,
        &transport_dyn,
    )
    .await
    .unwrap();
    assert_eq!(resp["success"], true, "重载失败不得阻塞 RPC 回包");

    // Assert：plugin 域保持空 + 无内容变化 → 无投影推送（unstable_event
    // 通知 `peri/unstable_event` 与投影推送 `session/update` 相互独立）
    assert!(
        plugin_entries(&registry).is_empty(),
        "重载失败 → 保留空 plugin 域（过时条目不得残留）"
    );
    // 稳定窗口：若存在异步重发也已落袋（重载失败路径 reconcile 无内容
    // 变化，不应触发任何 on_change）
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(
        session_update_count(&transport),
        1,
        "plugin 域无内容变化，不得触发投影推送（仅首发一条）"
    );
}

/// B3 注销分支（uninstall 调用路径 :969）：install 预置 plugin 域条目 →
/// 磁盘 enabledPlugins 清空 → uninstall 成功 → stale 条目按名注销，
/// plugin 域为空，RPC 仍 success。
#[tokio::test]
#[serial]
async fn test_plugin_uninstall_removes_stale_plugin_entries() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let _home_guard = HomeDirGuard::set(&home);
    seed_plugin_ecc(&home);

    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let mut cfg = make_server_config(peri_config, provider, &tmp).await;
    cfg.plugin_manager = Arc::new(MockPluginManager::install_ok("ecc"));
    let mut sessions = HashMap::new();
    let transport: Arc<MockTransport> = Arc::new(MockTransport::default());
    let transport_dyn: Arc<dyn crate::transport::AcpTransport> = transport.clone();

    let result = handle_request(
        "session/new",
        &json!({ "cwd": tmp.path().to_str().unwrap() }),
        &cfg,
        &mut sessions,
        &transport_dyn,
    )
    .await
    .unwrap();
    let sid = result["sessionId"].as_str().unwrap().to_string();
    super::session_lifecycle::after_new_response(&cfg, &transport_dyn, &sid).await;
    let registry = cfg
        .session_manager
        .command_registry_for(&sid)
        .expect("session 应持有命令注册表");

    // 预置：install 成功 → plugin 域含 plugin:ecc:deploy
    handle_request(
        "plugin/install",
        &json!({
            "sessionId": sid,
            "name": "ecc",
            "marketplace": "test-mkt",
        }),
        &cfg,
        &mut sessions,
        &transport_dyn,
    )
    .await
    .unwrap();
    assert_eq!(
        plugin_entries(&registry),
        vec!["plugin:ecc:deploy"],
        "install 预置失败"
    );

    // 卸载后磁盘重载面清空（enabledPlugins 空 → 无插件）
    std::fs::write(
        home.join(".claude").join("settings.json"),
        r#"{"enabledPlugins":[]}"#,
    )
    .unwrap();

    // Act：plugin/uninstall（mock 成功）
    let resp = handle_request(
        "plugin/uninstall",
        &json!({
            "sessionId": sid,
            "pluginId": "ecc@test-mkt",
        }),
        &cfg,
        &mut sessions,
        &transport_dyn,
    )
    .await
    .unwrap();
    assert_eq!(resp["success"], true, "uninstall RPC 应成功");

    // Assert：stale 条目按名注销 → plugin 域空
    assert!(
        plugin_entries(&registry).is_empty(),
        "uninstall 后 stale 插件命令应全部注销"
    );
    // 通知：首发 + install on_change + uninstall 注销 on_change，各恰一次
    wait_for_session_updates(&transport, 3).await;
    assert_eq!(
        session_update_count(&transport),
        3,
        "install/uninstall 各触发一次投影推送（首发 + 2 次重发）"
    );
}

/// M6：插件来源闭合位下，install / uninstall RPC **不得**让命令域重新出现
/// 可执行插件命令（注册表是真实执行面，见
/// `peri-agent/src/session/exec/executor_helpers/intercept.rs`）。
///
/// 关闭位随装配注入（`AcpServerConfig::plugin_face_closed`，取自 frozen）；
/// 本测试固定为关闭，断言两次管理 RPC 之后 plugin 域仍为空。
#[tokio::test]
#[serial]
async fn closed_plugin_face_keeps_the_command_registry_empty_across_plugin_rpcs() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let _home_guard = HomeDirGuard::set(&home);
    seed_plugin_ecc(&home);

    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let mut cfg = make_server_config(peri_config, provider, &tmp).await;
    cfg.plugin_manager = Arc::new(MockPluginManager::install_ok("ecc"));
    cfg.plugin_face_closed = true;
    let mut sessions = HashMap::new();
    let transport: Arc<MockTransport> = Arc::new(MockTransport::default());
    let transport_dyn: Arc<dyn crate::transport::AcpTransport> = transport.clone();

    let result = handle_request(
        "session/new",
        &json!({ "cwd": tmp.path().to_str().unwrap() }),
        &cfg,
        &mut sessions,
        &transport_dyn,
    )
    .await
    .unwrap();
    let sid = result["sessionId"].as_str().unwrap().to_string();
    super::session_lifecycle::after_new_response(&cfg, &transport_dyn, &sid).await;
    let registry = cfg
        .session_manager
        .command_registry_for(&sid)
        .expect("session 应持有命令注册表");
    assert!(
        plugin_entries(&registry).is_empty(),
        "前置：plugin 域初始为空"
    );

    // install：RPC 仍成功（管理面保留），但命令域不得出现可执行插件命令。
    let resp = handle_request(
        "plugin/install",
        &json!({ "sessionId": sid, "name": "ecc", "marketplace": "test-mkt" }),
        &cfg,
        &mut sessions,
        &transport_dyn,
    )
    .await
    .unwrap();
    assert_eq!(resp["success"], true, "管理面 RPC 不受插件来源闭合位影响");
    assert!(
        plugin_entries(&registry).is_empty(),
        "关闭位下 install 后 plugin 域必须仍为空: {:?}",
        plugin_entries(&registry)
    );

    // uninstall：同样不得注册（只允许注销 stale）。
    let resp = handle_request(
        "plugin/uninstall",
        &json!({
            "sessionId": sid,
            "pluginId": "ecc@test-mkt",
        }),
        &cfg,
        &mut sessions,
        &transport_dyn,
    )
    .await
    .unwrap();
    assert_eq!(resp["success"], true);
    assert!(
        plugin_entries(&registry).is_empty(),
        "关闭位下 uninstall 后 plugin 域必须仍为空: {:?}",
        plugin_entries(&registry)
    );
}
