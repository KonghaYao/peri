//! beta flag（`config.betas`）经**生产装配路径**进入会话冻结值与 builtin `workspace`
//! 实例的证据（`requests_test.rs` 的子模块）。
//!
//! 两个用例是**差分对照**：同一夹具只差 workspace settings 的
//! `config.betas["full-async-tools"]`：
//! - [`new_session_beta_flag_reaches_frozen_value_and_builtin_bash_default`]：
//!   `true` ⇒ 冻结值含该 flag，builtin `workspace` 实例的 Bash schema
//!   `run_in_background.default = true`（Bash 缺省后台）；
//! - [`new_session_without_beta_flag_keeps_foreground_default`]：
//!   未配置 ⇒ 冻结值为空投影，schema `default = false`（与 flag 引入前一致）。
//!
//! 装配路径（两条相同，均为生产构造点）：`session/new` → 准备期从选中的配置来源投影
//! flag（`PreparedSessionInputs::build_frozen_after_activation`）→
//! 随 `FrozenContext::beta_flags` 冻结 → `SessionEnvironment::assemble_with_frozen`
//! 从**冻结值**派生 Bash 有效缺省 → `HostAssemblyInput` →
//! `BuiltinInstanceContext` → `dispatch::builtin_server_handler` 的 `workspace` arm 调
//! `WorkspaceMcpServer::standalone` → 实例工具 schema。**没有测试专用构造器**参与这条链。
//!
//! `#[serial]`：builtin 注入开关是进程级 env（`PERI_MCP_BUILTIN`，由关闭 builtin 的
//! 串行用例改写），本文件必须有 builtin 实例在场。

use super::*;
use peri_acp_types::beta_flags::FULL_ASYNC_TOOLS;

/// 夹具：`startup`（装配起点）与 `target`（会话 cwd，含 settings）。
///
/// `target` 的项目 settings 自带 provider 与 beta flag 覆盖：会话 cwd 与 `startup_cwd`
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
    let config =
        make_peri_config_with_provider(make_provider_config("test", "openai", "key", "model"));
    let provider = LlmProvider::from_config(&config).unwrap();
    let mut cfg = make_server_config(config, provider, tmp).await;
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
    let pool = pool
        .as_any()
        .downcast_ref::<peri_middlewares::mcp::McpClientPool>()?;
    let handle = pool.get_client("workspace")?;
    let tool = handle
        .tools
        .iter()
        .find(|tool| tool.name.as_ref() == "Bash")?;
    Some(serde_json::Value::Object((*tool.input_schema).clone()))
}

/// 建会话（生产路径）并返回 `(sessionId, Bash 输入 schema)`。
///
/// 会话池的 builtin 握手在准备期完成，句柄与工具清单此时已可读（同 `meta_resources`
/// 用例的观察面）。
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
    let created = handle_request(
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

/// flag = true：冻结值含该 flag，且 builtin 实例的 Bash 缺省后台（schema 同步）。
#[tokio::test]
#[serial]
async fn new_session_beta_flag_reaches_frozen_value_and_builtin_bash_default() {
    let (tmp, startup, target) = beta_fixture(r#""full-async-tools":true"#);
    let (id, bash, sessions) = create_session_with_bash_schema(&tmp, startup, target).await;

    let frozen = sessions[&id].frozen.as_ref().expect("创建必须发布 frozen");
    assert!(
        frozen.v2_frozen().beta_flags.is_enabled(FULL_ASYNC_TOOLS),
        "flag 必须进入会话冻结值（配置面 → 冻结载体）：{:?}",
        frozen.v2_frozen().beta_flags
    );

    let bash = bash.expect("builtin workspace 实例必须暴露 Bash");
    assert_eq!(
        bash["properties"]["run_in_background"]["default"],
        json!(true),
        "Bash 有效缺省必须随会话冻结值到达 builtin 实例（schema 与执行缺省同源）"
    );
}

/// 未配置：冻结值为空投影，Bash 保持前台缺省（与 flag 引入前一致）。
#[tokio::test]
#[serial]
async fn new_session_without_beta_flag_keeps_foreground_default() {
    let (tmp, startup, target) = beta_fixture("");
    let (id, bash, sessions) = create_session_with_bash_schema(&tmp, startup, target).await;

    let frozen = sessions[&id].frozen.as_ref().expect("创建必须发布 frozen");
    assert!(
        frozen.v2_frozen().beta_flags.is_empty(),
        "未配置时冻结投影必须为空（快照缺失按 false，不意外开启能力）"
    );

    let bash = bash.expect("builtin workspace 实例必须暴露 Bash");
    assert_eq!(
        bash["properties"]["run_in_background"]["default"],
        json!(false),
        "未配置时 Bash 缺省必须与 flag 引入前一致（前台）"
    );
}

/// 既有会话保持冻结值：新建（flag 未配置）→ **磁盘改成开启** → `session/load` 恢复
/// → 恢复会话仍用持久 blob 的冻结值（Bash 缺省仍是前台，不按当前配置重建）。
///
/// 这一条覆盖 `SessionEnvironment::assemble_with_frozen` 的 `Some(frozen)` 分支
/// （恢复 / fork 路径）；新建路径见上面两个用例。
#[tokio::test]
#[serial]
async fn restored_session_keeps_frozen_beta_flag() {
    let (tmp, startup, target) = beta_fixture("");
    let cfg = beta_server_config(&tmp, startup).await;
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
    let mut sessions = HashMap::new();
    let created = handle_request(
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
            .is_enabled(FULL_ASYNC_TOOLS),
        "前置：新建会话说 flag 未开启"
    );

    // 会话创建之后改设置：新会话会读到 true，**既有会话**必须不受影响。
    write_betas(std::path::Path::new(&target), r#""full-async-tools":true"#);

    // 冷恢复形态：会话不在本 host 的活跃表里（`sessions` 为空）——
    // `prepare_existing` 因此走 `Publish` 分支，环境按**持久 blob** 重新装配
    // （活跃表命中只补历史、不重装配，覆盖不到 `Some(frozen)` 分支）。
    let mut restored_sessions = HashMap::new();
    handle_request(
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
        !frozen.v2_frozen().beta_flags.is_enabled(FULL_ASYNC_TOOLS),
        "既有会话必须保持冻结值（frozen 只接受持久 blob，不按当前配置重建）：{:?}",
        frozen.v2_frozen().beta_flags
    );
    let bash = workspace_bash_schema(restored.environment.as_ref())
        .expect("builtin workspace 实例必须暴露 Bash");
    assert_eq!(
        bash["properties"]["run_in_background"]["default"],
        json!(false),
        "恢复会话的 Bash 缺省必须仍是前台（冻结值胜出）"
    );
}
