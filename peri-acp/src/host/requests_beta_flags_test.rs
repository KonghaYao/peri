//! beta flag 配置面经生产装配路径进入会话冻结值的证据
//! （`requests_test.rs` 的子模块；`full-async-tools` 移除后的回归）。
//!
//! 覆盖三点：
//! - 注册表外、但被配置面白名单接受的 TUI 本地键仍能投影并随会话冻结；
//! - `full-async-tools` 已删除：配置里残留该键会在解析后剔除，冻结投影为空；
//! - Bash 不提供缺省后台（async bash 未实现）：无论 flag 配置如何，schema
//!   `run_in_background.default` 恒为 `false`。
//!
//! 装配路径（生产构造点）：`session/new` → 准备期从选中的配置来源投影 flag
//! （`PreparedSessionInputs::build_frozen_after_activation`）→
//! 随 `FrozenContext::beta_flags` 冻结 → `SessionEnvironment::assemble_with_frozen`
//! → builtin `workspace` 实例的 Bash 输入 schema。**没有测试专用构造器**参与这条链。
//!
//! `#[serial]`：builtin 注入开关是进程级 env（`PERI_MCP_BUILTIN`，由关闭 builtin 的
//! 串行用例改写），本文件必须有 builtin 实例在场。

use crate::host::requests::tests::MockTransport;
use crate::host::AcpServerConfig;
use crate::host::SessionState;
use crate::provider::LlmProvider;
use peri_config::betas::HISTORY_WINDOW_V1;
use serde_json::json;
use serial_test::serial;
use std::collections::HashMap;
use std::sync::Arc;

/// 夹具：`startup`（装配起点）与 `target`（会话 cwd，含 settings）。
///
/// `target` 的项目 settings 自带 provider 与 beta 覆盖：会话 cwd 与 `startup_cwd`
/// 不同目录时，准备路径按「异目录只读一次」在会话目录重解析配置
/// （`PreparedConfiguration`），flag 因此来自 **workspace 层**覆盖。
fn beta_fixture(betas: &str) -> (tempfile::TempDir, String, String) {
    let tmp = tempfile::TempDir::new().unwrap();
    let startup = tmp.path().join("startup");
    let target = tmp.path().join("target");
    std::fs::create_dir(&startup).unwrap();
    std::fs::create_dir_all(target.join(".peri")).unwrap();
    write_betas(&target, betas);
    (
        tmp,
        startup.to_string_lossy().into_owned(),
        target.to_string_lossy().into_owned(),
    )
}

/// 写会话目录的 settings（provider 自带 + 指定的 `config.betas` 覆盖）。
fn write_betas(target: &std::path::Path, betas: &str) {
    std::fs::write(
        target.join(".peri/settings.json"),
        format!(
            r#"{{"config":{{"active_alias":"sonnet","providers":[{{"id":"test","type":"openai","apiKey":"key","models":{{"sonnet":"model"}}}}],"betas":{{{betas}}}}}}}"#
        ),
    )
    .unwrap();
}

/// 生产装配的宿主配置（bare：只建 workspace 池 + 指定启动目录）。
async fn beta_server_config(tmp: &tempfile::TempDir, startup_cwd: String) -> AcpServerConfig {
    let config = crate::host::requests::tests::make_peri_config_with_provider(
        crate::host::requests::tests::make_provider_config("test", "openai", "key", "model"),
    );
    let provider = LlmProvider::from_config(&config).unwrap();
    let mut cfg = crate::host::requests::tests::make_server_config(config, provider, tmp).await;
    cfg.workspace_assembly = Some(crate::host::assemble::WorkspaceAssembly {
        startup_cwd,
        bare: true,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
        capabilities: Default::default(),
    });
    cfg
}

/// builtin `workspace` 实例的 Bash 输入 schema（`tools/list` 投影；失败返回 `None`）。
fn workspace_bash_schema(
    environment: Option<&Arc<crate::host::workspace::SessionEnvironment>>,
) -> Option<serde_json::Value> {
    let pool = environment?.cfg.mcp_pool.as_ref()?;
    let pool = (pool.as_ref() as &dyn std::any::Any)
        .downcast_ref::<peri_middlewares::mcp::McpClientPool>()?;
    // v5 默认装配只有一个 `peri` 出口；回滚路径仍保留 `workspace` 实例。
    let handle = pool.client("peri").or_else(|| pool.client("workspace"))?;
    let tool = handle
        .tools
        .iter()
        .find(|tool| tool.name.as_ref() == "Bash")?;
    Some(serde_json::Value::Object((*tool.input_schema).clone()))
}

/// 建会话（生产路径）并返回 `(sessionId, Bash 输入 schema)`。
async fn create_session_with_bash_schema(
    tmp: &tempfile::TempDir,
    startup: String,
    target: String,
) -> (
    String,
    Option<serde_json::Value>,
    HashMap<String, SessionState>,
) {
    let cfg = beta_server_config(tmp, startup).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    let created = crate::host::requests::handle_request(
        "session/new",
        &json!({"cwd": target}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/new（beta flag 夹具）必须成功");
    let id = created["sessionId"].as_str().unwrap().to_string();
    let bash = workspace_bash_schema(
        sessions
            .get(&id)
            .and_then(|state| state.environment.as_ref()),
    );
    (id, bash, sessions)
}

/// 仍被白名单接受的本地 beta 键进入冻结值；Bash 缺省不随 flag 改变（async bash 不实现）。
#[tokio::test]
#[serial]
async fn new_session_beta_flag_freezes_but_never_changes_bash_default() {
    let (tmp, startup, target) = beta_fixture(r#""history-window-v1":true"#);
    let (id, bash, sessions) = create_session_with_bash_schema(&tmp, startup, target).await;

    let frozen = sessions[&id].frozen.as_ref().expect("创建必须发布 frozen");
    assert!(
        frozen.v2_frozen().beta_flags.is_enabled(HISTORY_WINDOW_V1),
        "本地 beta 键必须进入会话冻结值（配置面 → 冻结载体）：{:?}",
        frozen.v2_frozen().beta_flags
    );

    let bash = bash.expect("builtin workspace 实例必须暴露 Bash");
    assert_eq!(
        bash["properties"]["run_in_background"]["default"],
        json!(false),
        "Bash 缺省必须恒为前台：async bash 未实现，flag 不得改变它"
    );
}

/// `full-async-tools` 已删除：配置残留键在解析后剔除，冻结投影为空；
/// Bash 保持前台缺省（不再有 Bash 侧行为）。
#[tokio::test]
#[serial]
async fn removed_full_async_tools_is_dropped_and_has_no_effect() {
    let (tmp, startup, target) = beta_fixture(r#""full-async-tools":true"#);
    let (id, bash, sessions) = create_session_with_bash_schema(&tmp, startup, target).await;

    let frozen = sessions[&id].frozen.as_ref().expect("创建必须发布 frozen");
    assert!(
        frozen.v2_frozen().beta_flags.is_empty(),
        "已删除的 flag 键不得进入冻结投影：{:?}",
        frozen.v2_frozen().beta_flags
    );

    let bash = bash.expect("builtin workspace 实例必须暴露 Bash");
    assert_eq!(
        bash["properties"]["run_in_background"]["default"],
        json!(false),
        "已删除的 flag 不得再改变 Bash 缺省"
    );
}

/// 既有会话保持冻结值：新建（未配置）→ 磁盘改成开启 → `session/load` 恢复
/// → 恢复会话仍用持久 blob 的冻结值（不按当前配置重建）。
///
/// 覆盖 `SessionEnvironment::assemble_with_frozen` 的 `Some(frozen)` 分支
/// （恢复 / fork 路径）；新建路径见上面两个用例。
#[tokio::test]
#[serial]
async fn restored_session_keeps_frozen_beta_flag() {
    let (tmp, startup, target) = beta_fixture("");
    let cfg = beta_server_config(&tmp, startup).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    let created = crate::host::requests::handle_request(
        "session/new",
        &json!({"cwd": target}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/new 必须成功");
    let id = created["sessionId"].as_str().unwrap().to_string();
    assert!(
        !sessions[&id]
            .frozen
            .as_ref()
            .expect("创建必须发布 frozen")
            .v2_frozen()
            .beta_flags
            .is_enabled(HISTORY_WINDOW_V1),
        "前置：新建会话说该键未开启"
    );

    // 会话创建之后改设置：新会话会读到 true，**既有会话**必须不受影响。
    write_betas(std::path::Path::new(&target), r#""history-window-v1":true"#);

    // 冷恢复形态：会话不在本 host 的活跃表里（`sessions` 为空）——
    // `prepare_existing` 因此走 `Publish` 分支，环境按**持久 blob** 重新装配
    // （活跃表命中只补历史、不重装配，覆盖不到 `Some(frozen)` 分支）。
    let mut restored_sessions = HashMap::new();
    crate::host::requests::handle_request(
        "session/load",
        &json!({"sessionId": id, "cwd": target}),
        &cfg,
        &mut restored_sessions,
        &transport,
    )
    .await
    .expect("session/load 必须成功");
    let restored = restored_sessions.get(&id).expect("恢复后的会话必须登记");
    let frozen = restored.frozen.as_ref().expect("恢复必须载入 frozen");
    assert!(
        !frozen.v2_frozen().beta_flags.is_enabled(HISTORY_WINDOW_V1),
        "既有会话必须保持冻结值（frozen 只接受持久 blob，不按当前配置重建）：{:?}",
        frozen.v2_frozen().beta_flags
    );
    let bash = workspace_bash_schema(restored.environment.as_ref())
        .expect("builtin workspace 实例必须暴露 Bash");
    assert_eq!(
        bash["properties"]["run_in_background"]["default"],
        json!(false),
        "恢复会话的 Bash 缺省必须仍是前台"
    );
}
