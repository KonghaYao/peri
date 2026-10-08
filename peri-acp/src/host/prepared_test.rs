//! 会话准备输入（`PreparedSessionInputs`）的目标测试：
//! 同输入复用、frozen 字节同源、按需复用 host 配置、fork 复用源字节、准备无写、
//! 发布段只消费给定的准备对象（不重读外部输入）。

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::Arc,
};

use peri_middlewares::permission::shared_mode::{PermissionMode, SharedPermissionMode};
use serial_test::serial;
use tempfile::TempDir;

use super::assemble::WorkspaceAssembly;
use super::prepared::PreparedSessionInputs;
use super::AcpServerConfig;
use crate::provider::{LlmProvider, ProviderConfig, ProviderModels};
use crate::session::frozen_snapshot::{decode_frozen_snapshot, encode_frozen_snapshot};

/// 外部输入（cwd/CLAUDE.md 内容）的三个取样：创建期读到的那份，与它前后各一次改写。
///
/// **捕获点（W5/J2 §7.0 已裁决）**：项目指令的唯一来源是 builtin `workspace`
/// 资源面，读取发生在**创建步**（`new_session_from_prepared` 的 P4）；`prepare_new`
/// 不读盘。两个改写内容用于证明「捕获一次」：创建前改写只影响创建期采集，
/// 创建后改写不得再进入任何状态。
const FROZEN_INPUT_AT_PREPARATION: &str = "frozen-seam: content frozen at preparation\n";
const FROZEN_INPUT_AFTER_PREPARATION: &str = "frozen-seam: rewritten after preparation\n";
const FROZEN_INPUT_AFTER_CREATION: &str = "frozen-seam: rewritten after creation\n";

/// 生产形态的 workspace 装配（bare：只建 workspace 池，不加载插件聚合）。
///
/// W5 后项目指令（CLAUDE.md）经 builtin `workspace` 实例的资源面采集：宿主没有
/// 本装配就没有 workspace 环境，指令面按 X4/J5 整体缺席（零磁盘兜底）。
///
/// `startup_cwd` 取会话 cwd 的**规范化**形态：与准备面的「同目录复用配置」判据同
/// 口径（`resolve_configuration` 对 startup_cwd 做 `canonicalize` 后与会话 cwd
/// 比较），命中复用路径即不触发第二次只读配置加载。
fn workspace_assembly(cwd: &str) -> WorkspaceAssembly {
    WorkspaceAssembly {
        startup_cwd: cwd.to_owned(),
        bare: true,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
        capabilities: Default::default(),
    }
}

fn make_provider() -> LlmProvider {
    let mut config = crate::provider::PeriConfig::default();
    config.config.active_alias = "sonnet".to_string();
    config.config.providers = vec![ProviderConfig {
        id: "prepared-test".into(),
        provider_type: "anthropic".into(),
        api_key: "prepared-test-placeholder".into(),
        models: ProviderModels {
            sonnet: "prepared-test-model".into(),
            ..Default::default()
        },
        ..Default::default()
    }];
    LlmProvider::from_config(&config).expect("test provider config must resolve")
}

/// 真实 SQLite 门面 + 桥的 host（`workspace_assembly` 由调用方决定）。
async fn prepared_test_host(
    tmp: &TempDir,
    workspace_assembly: Option<WorkspaceAssembly>,
) -> AcpServerConfig {
    // 生产同形：只注入门面（SessionManager 与 Controller 都只持它）。
    let session_resources =
        peri_agent::resources::open_session_resources_with(Some(tmp.path().join("threads.db")))
            .await
            .unwrap();
    let peri_config = crate::provider::PeriConfig::default();
    let provider = make_provider();
    let session_manager = crate::session::SessionManager::new(
        session_resources.clone(),
        provider.clone(),
        Arc::new(peri_config.clone()),
        SharedPermissionMode::new(PermissionMode::Bypass),
        None,
        None,
        None,
        None,
        Some(Arc::new(|| {
            Arc::new(peri_agent::agent::async_tasks::TaskManager::new())
                as Arc<dyn peri_acp_types::tasks::TaskManager>
        })),
        Arc::new(peri_middlewares::host_ports::AgentCatalogProvider::new()),
        Vec::new(),
    );
    let (host_task_owner, host_task_spawner) = crate::host::task_scope::HostTaskOwner::new();
    let (mcp_task_owner, _mcp_task_spawner) = peri_middlewares::mcp::McpTaskOwner::new();
    AcpServerConfig {
        workspace_assembly,
        host_task_owner: Some(host_task_owner),
        host_task_spawner,
        mcp_task_owner: Some(Box::new(mcp_task_owner)),
        provider: Arc::new(parking_lot::RwLock::new(provider)),
        peri_config: Arc::new(parking_lot::RwLock::new(peri_config)),
        permission_mode: SharedPermissionMode::new(PermissionMode::Bypass),
        cron_scheduler: None,
        mcp_pool: None,
        mcp_apps_relay: None,
        acp_mcp: None,
        dynamic_mcp: None,
        oauth_event_tx: None,
        oauth_event_rx: None,
        plugin_skill_roots: Vec::new(),
        plugin_command_entries: Vec::new(),
        plugin_face_closed: false,
        plugin_hooks: Vec::new(),
        plugin_hooks_only: Vec::new(),
        plugin_loaded: Vec::new(),
        hook_groups: Vec::new(),
        tool_search_index: Arc::new(peri_middlewares::tool_search::ToolSearchIndex::new()),
        agent_catalog: Arc::new(peri_middlewares::host_ports::AgentCatalogProvider::new()),
        plugin_manager: Arc::new(peri_middlewares::host_ports::PluginManager),
        settings_hooks: Arc::new(peri_middlewares::host_ports::SettingsHooksLoader),
        shared_tools: Arc::new(parking_lot::RwLock::new(BTreeMap::new())),
        workflow_middleware_factory: Arc::new(
            peri_middlewares::assembly::WorkflowAgentMiddlewareFactory,
        ),
        session_resources: session_resources.clone(),
        // 测试宿主：不注入部署关闭权（没有部署生命周期）。
        session_store_shutdown: None,
        controller: Arc::new(peri_controller::Controller::new(session_resources)),
        langfuse_session: None,
        langfuse_shutdown_owner: None,
        config_source: Arc::new(
            crate::provider::ConfigSource::load_at(
                &tmp.path().join("empty-cwd"),
                tmp.path().join("test_config.json"),
            )
            .unwrap(),
        ),
        session_manager,
        stdio_command_filter: false,
    }
}

/// 目录树快照（相对路径 + 字节内容），用于断言准备阶段没有写副作用。
fn snapshot_tree(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, root, out);
            } else {
                let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
                let content = std::fs::read(&path).unwrap_or_default();
                out.push((relative, content));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn make_workspace_dir(tmp: &TempDir, name: &str) -> String {
    let dir = tmp.path().join(name);
    std::fs::create_dir_all(&dir).unwrap();
    dir.to_str().unwrap().to_owned()
}

/// 会话解析后的执行目录是规范化路径（macOS 上 `/var` → `/private/var`）。
fn canonical_workspace_dir(tmp: &TempDir, name: &str) -> String {
    let dir = tmp.path().join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = std::fs::canonicalize(dir).unwrap();
    dir.to_str().unwrap().to_owned()
}

/// 同一输入重复准备必须得到同一 snapshot 字节，且字节与冻结数据同源。
#[tokio::test]
async fn prepare_new_is_repeatable_and_frozen_bytes_are_single_source() {
    let tmp = TempDir::new().unwrap();
    let cwd = make_workspace_dir(&tmp, "workspace");
    let host = prepared_test_host(&tmp, None).await;

    let first = PreparedSessionInputs::prepare_new(&host, &cwd).unwrap();
    let second = PreparedSessionInputs::prepare_new(&host, &cwd).unwrap();

    assert_eq!(first.cwd, cwd);
    assert_eq!(
        first.frozen_encoded.as_ref().unwrap(),
        second.frozen_encoded.as_ref().unwrap(),
        "同一准备输入必须产出同一 snapshot 字节"
    );
    assert_eq!(
        first.frozen.as_ref().unwrap().date(),
        second.frozen.as_ref().unwrap().date()
    );
    let decoded = decode_frozen_snapshot(first.frozen_encoded.as_ref().unwrap()).unwrap();
    assert_eq!(
        decoded.date(),
        first.frozen.as_ref().unwrap().date(),
        "snapshot 字节与内存冻结数据必须同源（日期不各取一份）"
    );
    assert_eq!(
        decoded.system_prompt(),
        first.frozen.as_ref().unwrap().system_prompt()
    );
    assert_eq!(first.skill_roots.len(), second.skill_roots.len());
}

/// 准备阶段不写会话数据/登记：整个数据目录逐字节不变。
#[tokio::test]
async fn prepare_new_writes_no_session_state() {
    let tmp = TempDir::new().unwrap();
    let cwd = make_workspace_dir(&tmp, "workspace");
    let host = prepared_test_host(&tmp, None).await;

    let before = snapshot_tree(tmp.path());
    let prepared = PreparedSessionInputs::prepare_new(&host, &cwd).unwrap();
    assert!(!prepared.frozen_encoded.as_ref().unwrap().is_empty());
    assert_eq!(
        before,
        snapshot_tree(tmp.path()),
        "准备阶段不得写会话数据或本机登记"
    );
}

/// 启动目录一致时必须复用 host 已装配的配置源，不重读配置/插件。
#[tokio::test]
async fn prepare_new_reuses_host_configuration_for_startup_directory() {
    let tmp = TempDir::new().unwrap();
    let cwd = canonical_workspace_dir(&tmp, "workspace");
    let host = prepared_test_host(
        &tmp,
        Some(WorkspaceAssembly {
            startup_cwd: cwd.clone(),
            bare: true,
            drive_cron_tick: false,
            mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
            capabilities: Default::default(),
        }),
    )
    .await;

    let prepared = PreparedSessionInputs::prepare_new(&host, &cwd).unwrap();
    assert!(
        Arc::ptr_eq(&prepared.configuration.config_source, &host.config_source),
        "同一启动目录必须复用已装配配置源，不第二次 load_at"
    );
    assert!(
        prepared.plugin_data.is_none(),
        "bare 工作区装配不加载插件聚合"
    );
}

/// 普通 fork 复用 source 的精确 frozen 字节，不按当前日期/目录重冻。
#[tokio::test]
async fn prepare_fork_reuses_source_snapshot_bytes() {
    let tmp = TempDir::new().unwrap();
    let cwd = make_workspace_dir(&tmp, "workspace");
    let fork_cwd = make_workspace_dir(&tmp, "fork-workspace");
    let host = prepared_test_host(&tmp, None).await;

    let source = PreparedSessionInputs::prepare_new(&host, &cwd).unwrap();
    let forked = PreparedSessionInputs::prepare_fork(
        &host,
        &fork_cwd,
        source.frozen_encoded.as_ref().unwrap(),
    )
    .unwrap();

    assert_eq!(
        forked.frozen_encoded.as_ref().unwrap(),
        source.frozen_encoded.as_ref().unwrap(),
        "fork 必须保留 source 的精确 frozen 字节"
    );
    assert_eq!(
        forked.frozen.as_ref().unwrap().date(),
        source.frozen.as_ref().unwrap().date()
    );
    assert_eq!(
        forked.frozen.as_ref().unwrap().system_prompt(),
        source.frozen.as_ref().unwrap().system_prompt()
    );
    assert_eq!(forked.cwd, fork_cwd);

    assert!(
        PreparedSessionInputs::prepare_fork(&host, &fork_cwd, "{ not a snapshot").is_err(),
        "损坏的 source 快照必须让准备失败，而不是带着空 frozen 继续"
    );
}

/// legacy 准备按解析出的执行目录构建，并记录保存的绝对 cwd（登记事实）。
#[tokio::test]
async fn prepare_legacy_records_saved_cwd_and_builds_from_workspace() {
    let tmp = TempDir::new().unwrap();
    let registered = tmp.path().join("saved-workspace");
    std::fs::create_dir_all(&registered).unwrap();
    let registered_raw = registered.to_str().unwrap().to_owned();
    let workspace_cwd = canonical_workspace_dir(&tmp, "saved-workspace");
    let host = prepared_test_host(&tmp, None).await;

    let legacy =
        PreparedSessionInputs::prepare_legacy_deferred(&host, &registered_raw, &workspace_cwd)
            .unwrap();

    assert_eq!(legacy.cwd, workspace_cwd);
    assert_eq!(
        legacy.legacy.as_ref().unwrap().saved_cwd,
        PathBuf::from(&registered_raw),
        "legacy 必须记录保存的绝对 cwd（不是调用方终端的 cwd）"
    );
    // M5 阶段一不构建 frozen：候选必须等资源 bootstrap 与内容读取后才定稿，
    // 由接纳事务一次性写入（不得先写空 frozen 再替换）。
    assert!(
        legacy.frozen.is_none() && legacy.frozen_encoded.is_none(),
        "legacy deferred 准备不得提前构建 frozen"
    );
}

/// new 路径的持久化属性：frozen 字节与发布到 live state 的冻结状态同源（一次写入），
/// binding 与 frozen 同时成立。
///
/// 端到端只走生产入口（resolve → 准备一次 → 发布段），不拿第二次准备比字节：断言
/// 的是同一次创建内部的自洽——持久化字节、live frozen 与本次工作区输入同源。
/// 项目指令的捕获点在创建步（P4，经 builtin `workspace` 资源面）：因此本用例的
/// `claude_md()` 断言的是**创建期**读到的 CLAUDE.md，而非准备期（准备期不读盘）。
/// 创建期经 builtin `workspace` 资源面采集指令 ⇒ 依赖注入**默认态**
/// （`PERI_MCP_BUILTIN` 非 `off`）。该 env 是进程级全局，由同进程的开关组用例在
/// `#[serial]` 临界区内改写（TEST-HERMETIC-001）⇒ 读侧必须同键互斥，否则并行窗口
/// 内读到 `off` 态：池里没有 `workspace` 句柄且 `initPhase` 已收口，资源面按 X4
/// 整体缺席、创建期指令快照退化为 `None`（不是被测语义）。
#[tokio::test]
#[serial]
async fn new_session_persists_frozen_bytes_from_its_single_preparation() {
    let tmp = TempDir::new().unwrap();
    let cwd = canonical_workspace_dir(&tmp, "workspace");
    // 生产形态宿主：指令面经 builtin `workspace` 实例（W5 后宿主本地无扫描点）。
    let host = prepared_test_host(&tmp, Some(workspace_assembly(&cwd))).await;
    std::fs::write(
        Path::new(&cwd).join("CLAUDE.md"),
        FROZEN_INPUT_AT_PREPARATION,
    )
    .unwrap();
    let mut sessions = std::collections::HashMap::new();

    let response = super::requests::session_lifecycle::handle_new(
        &serde_json::json!({ "cwd": cwd }),
        &host,
        &mut sessions,
    )
    .await
    .expect("session/new 必须经门面创建成功");

    let id = response["sessionId"]
        .as_str()
        .expect("response carries sessionId")
        .to_owned();
    assert!(sessions[&id].frozen.is_some(), "创建必须发布冻结输入");
    let snapshot = host
        .session_resources
        .load_session_snapshot(&id)
        .await
        .expect("created session must be readable through the facade");
    let persisted = match snapshot.frozen {
        peri_acp_types::session_resources::FrozenState::Present(bytes) => bytes.into_string(),
        other => panic!("frozen must be present after create_session: {other:?}"),
    };
    let decoded = decode_frozen_snapshot(&persisted).expect("持久化字节必须可解码");
    assert_eq!(
        decoded.claude_md(),
        Some(FROZEN_INPUT_AT_PREPARATION),
        "持久化字节必须来自本次工作区输入（创建期经 workspace 资源面读到的 CLAUDE.md）"
    );
    let live = sessions[&id]
        .frozen
        .clone()
        .expect("创建必须发布 live frozen");
    assert_eq!(
        encode_frozen_snapshot(&live).unwrap(),
        persisted,
        "发布到 live state 的 frozen 与持久化字节必须逐字节同源"
    );
    assert!(
        matches!(
            snapshot.binding,
            peri_acp_types::session_resources::BindingState::Bound(_)
        ),
        "创建必须写入不可变 binding"
    );
}

/// 外部输入只在**创建期**被读一次：准备不读盘，创建读到的内容被定格，之后改写磁盘
/// 不再影响 live 或持久化状态。
///
/// 捕获点（W5/J2 §7.0 裁决）：项目指令经 builtin `workspace` 资源面在
/// `new_session_from_prepared` 的 P4 采集；准备阶段不读盘。本用例取两处证据：
/// 1. 测试夹具 `prepare_new` **立刻**构建 frozen，但指令恒为空——同一夹具、同一份
///    磁盘文件在 pre-W5 会在这里读到内容（旧断言即 `Some(FROZEN_INPUT_AT_PREPARATION)`），
///    现在宿主本地已无扫描点，指令只经创建步的资源面进入；
/// 2. 生产 new 路径的准备形态 `prepare_new_deferred` 连 frozen 都不建（结构证据：
///    准备阶段没有构建动作），构建整体让给创建步的 P4（`handle_new` 即此形状）。
///
/// no-reread 的主体落在**创建之后**：捕获只发生一次，创建后改写磁盘不得进入任何
/// 重读路径。（`new_session_from_prepared` 就是 `handle_new` 准备之后的同一条生产
/// 路径；准备与创建之间改写文件，按新语义创建期读到的就是改写后的内容。）
/// 与 [`new_session_persists_frozen_bytes_from_its_single_preparation`] 同一读侧依赖
/// （创建期指令快照 ⇒ builtin `workspace` 资源面 ⇒ `PERI_MCP_BUILTIN` 默认态），
/// 故同样与进程级开关组用例同键互斥（TEST-HERMETIC-001）。
#[tokio::test]
#[serial]
async fn new_session_from_prepared_does_not_reread_external_frozen_inputs() {
    let tmp = TempDir::new().unwrap();
    let cwd = canonical_workspace_dir(&tmp, "workspace");
    let host = prepared_test_host(&tmp, Some(workspace_assembly(&cwd))).await;
    let claude_md = Path::new(&cwd).join("CLAUDE.md");
    std::fs::write(&claude_md, FROZEN_INPUT_AT_PREPARATION).unwrap();

    // ① 准备阶段不读盘：frozen 已构建（snapshot 非空），但指令面为空。
    let probe = PreparedSessionInputs::prepare_new(&host, &cwd).unwrap();
    assert_eq!(
        probe.frozen.as_ref().unwrap().claude_md(),
        None,
        "准备阶段不得读盘采集项目指令（捕获点已移到创建步）"
    );
    assert!(
        !probe.frozen_encoded.as_ref().unwrap().is_empty(),
        "准备产物仍是完整 frozen snapshot（只是不含指令面内容）"
    );
    drop(probe);

    // 准备之后改写外部输入：创建期读到的就是这份改写后的内容。
    std::fs::write(&claude_md, FROZEN_INPUT_AFTER_PREPARATION).unwrap();

    // ② 生产 new 路径的准备形态：frozen 延后到创建步构建（本步不抢跑冻结）。
    let prepared = PreparedSessionInputs::prepare_new_deferred(&host, &cwd).unwrap();
    assert!(
        prepared.frozen.is_none() && prepared.frozen_encoded.is_none(),
        "deferred 准备不得提前构建 frozen——构建归创建步 P4（含指令面采集）"
    );

    let workspace = host
        .session_resources
        .resolve_workspace(Path::new(&cwd))
        .await
        .expect("工作区必须可解析");
    let mut sessions = std::collections::HashMap::new();
    let response = super::requests::session_lifecycle::new_session_from_prepared(
        &host,
        &workspace,
        prepared,
        &mut sessions,
    )
    .await
    .expect("发布段必须用给定准备对象创建成功");

    let id = response["sessionId"]
        .as_str()
        .expect("response carries sessionId")
        .to_owned();
    assert!(sessions[&id].frozen.is_some(), "创建必须发布冻结输入");
    let snapshot = host
        .session_resources
        .load_session_snapshot(&id)
        .await
        .expect("created session must be readable through the facade");
    let persisted = match snapshot.frozen {
        peri_acp_types::session_resources::FrozenState::Present(bytes) => bytes.into_string(),
        other => panic!("frozen must be present after create_session: {other:?}"),
    };
    let live = sessions[&id]
        .frozen
        .clone()
        .expect("创建必须发布 live frozen");
    let live_bytes_at_create = encode_frozen_snapshot(&live).unwrap();
    assert_eq!(
        live_bytes_at_create, persisted,
        "发布到 live state 的 frozen 与持久化字节必须逐字节同源"
    );
    assert_eq!(
        live.claude_md(),
        Some(FROZEN_INPUT_AFTER_PREPARATION),
        "创建期采集的是创建时的磁盘内容（准备之后改写的那份），不是准备期的"
    );

    // ③ 创建完成后再改写磁盘：捕获只发生一次，live 与持久化快照都不变。
    std::fs::write(&claude_md, FROZEN_INPUT_AFTER_CREATION).unwrap();
    let live_after = sessions[&id]
        .frozen
        .clone()
        .expect("live frozen 仍在（创建后改写磁盘不得撤销会话状态）");
    assert_eq!(
        encode_frozen_snapshot(&live_after).unwrap(),
        live_bytes_at_create,
        "创建后改写外部输入不得改变 live frozen"
    );
    assert_eq!(
        live_after.claude_md(),
        Some(FROZEN_INPUT_AFTER_PREPARATION),
        "创建期捕获一次后不再重读（创建后改写不进冻结状态）"
    );
    let snapshot_after = host
        .session_resources
        .load_session_snapshot(&id)
        .await
        .expect("created session must stay readable through the facade");
    let persisted_after = match snapshot_after.frozen {
        peri_acp_types::session_resources::FrozenState::Present(bytes) => bytes.into_string(),
        other => panic!("frozen must stay present after create_session: {other:?}"),
    };
    assert_eq!(
        persisted_after, persisted,
        "创建后改写外部输入不得改变持久化快照"
    );
}

// ─── H3/D1：准入期冻结运行环境按**有效 Workspace 来源**判定 ────────────────

/// 远端 Workspace 声明（`load_for_restore` 与新会话声明产出的同一形状：
/// `ConfigSource::WorkspaceRemote`）⇒ 冻结运行环境 unavailable，且准入路径
/// **不探测**宿主（探测计数 0，不冒充远端执行环境）。
#[tokio::test]
async fn remote_workspace_declaration_freezes_unavailable_runtime_env_without_probe() {
    use peri_acp_types::plugin::ConfigSource;

    let tmp = TempDir::new().unwrap();
    let cwd = canonical_workspace_dir(&tmp, "remote-ws");
    let host = prepared_test_host(&tmp, None).await;
    // 会话声明（持久 owner 装载同样落到这里：`resource_owners::load_for_restore`
    // 对 workspace 连接写入 WorkspaceRemote 来源）。
    let mut workspace: peri_acp_types::plugin::McpServerConfig =
        serde_json::from_value(serde_json::json!({"url": "https://remote.example.test/mcp"}))
            .unwrap();
    workspace.source = Some(ConfigSource::WorkspaceRemote);

    crate::prompt::reset_detect_call_count();
    let mut inputs = PreparedSessionInputs::prepare_new_deferred(&host, &cwd).unwrap();
    inputs.session_mcp_servers = HashMap::from([("workspace".to_string(), workspace)]);
    inputs
        .build_frozen_after_activation(&host, HashMap::new(), &[], &Default::default())
        .unwrap();

    assert_eq!(
        crate::prompt::detect_call_count(),
        0,
        "显式远端 Workspace 会话不得探测宿主运行环境"
    );
    let frozen = inputs.frozen.clone().expect("frozen built");
    assert_eq!(
        frozen.runtime_env(),
        None,
        "执行环境未知 ⇒ 冻结运行环境 unavailable（不冒充）"
    );
    assert!(
        frozen
            .system_prompt()
            .contains(crate::prompt::RUNTIME_ENV_UNAVAILABLE),
        "渲染显式标记 unavailable"
    );
    assert!(
        !frozen
            .system_prompt()
            .contains(&format!("Platform: {}", std::env::consts::OS)),
        "不得用宿主平台冒充远端执行环境"
    );
    // 持久化后仍为 unavailable（解码视图一致）
    let decoded = decode_frozen_snapshot(inputs.frozen_encoded.as_ref().unwrap()).unwrap();
    assert_eq!(decoded.runtime_env(), None);
}

/// 池中合并配置（部署/全局配置接管等，initialize 后可见）为显式远端
/// Workspace ⇒ 同样 unavailable + 不探测（池来源路径，不依赖准备输入声明）。
#[tokio::test]
async fn remote_workspace_pool_source_freezes_unavailable_runtime_env_without_probe() {
    use peri_acp_types::plugin::ConfigSource;

    let tmp = TempDir::new().unwrap();
    let cwd = canonical_workspace_dir(&tmp, "remote-pool-ws");
    let mut host = prepared_test_host(&tmp, None).await;
    let pool = Arc::new(peri_middlewares::mcp::McpClientPool::new_pending());
    let mut workspace: peri_acp_types::plugin::McpServerConfig =
        serde_json::from_value(serde_json::json!({"url": "https://remote.example.test/mcp"}))
            .unwrap();
    workspace.source = Some(ConfigSource::WorkspaceRemote);
    pool.set_session_servers(HashMap::from([("workspace".to_string(), workspace)]))
        .unwrap();
    host.mcp_pool = Some(Arc::clone(&pool) as Arc<dyn peri_acp_types::ports::McpPoolPort>);
    assert_eq!(
        pool.workspace_source(),
        Some(ConfigSource::WorkspaceRemote),
        "池来源事实必须暴露显式远端身份"
    );

    crate::prompt::reset_detect_call_count();
    let mut inputs = PreparedSessionInputs::prepare_new_deferred(&host, &cwd).unwrap();
    inputs
        .build_frozen_after_activation(&host, HashMap::new(), &[], &Default::default())
        .unwrap();

    assert_eq!(
        crate::prompt::detect_call_count(),
        0,
        "池来源为远端时不得探测宿主"
    );
    assert_eq!(inputs.frozen.as_ref().unwrap().runtime_env(), None);
}

/// 本地执行环境（无远端接管）⇒ 准入期**恰好探测一次**，宿主值随冻结持久化
/// 并在渲染中出现（H3 正向面）。
#[tokio::test]
async fn local_workspace_freezes_host_runtime_env_with_single_probe() {
    let tmp = TempDir::new().unwrap();
    let cwd = canonical_workspace_dir(&tmp, "local-ws");
    let host = prepared_test_host(&tmp, Some(workspace_assembly(&cwd))).await;

    crate::prompt::reset_detect_call_count();
    let mut inputs = PreparedSessionInputs::prepare_new_deferred(&host, &cwd).unwrap();
    inputs
        .build_frozen_after_activation(&host, HashMap::new(), &[], &Default::default())
        .unwrap();

    assert_eq!(
        crate::prompt::detect_call_count(),
        1,
        "本地准入恰好探测一次（不在渲染期重探）"
    );
    let frozen = inputs.frozen.clone().expect("frozen built");
    let runtime_env = frozen.runtime_env().expect("本地执行环境可冻结");
    assert_eq!(runtime_env.platform, std::env::consts::OS);
    assert!(
        frozen
            .system_prompt()
            .contains(&format!("Platform: {}", std::env::consts::OS)),
        "渲染消费冻结的宿主环境"
    );
    assert!(
        !frozen
            .system_prompt()
            .contains(crate::prompt::RUNTIME_ENV_UNAVAILABLE),
        "本地会话不得标记 unavailable"
    );
}

// ── M6：准备面的插件准入必须与装配面同一份决定 ───────────────────────────────
//
// 装配面（`workspace.rs` → `assemble.rs`）用 **frozen** 的 disabled 集合派生
// `plugin_face_closed`，同一位随 `HostAssemblyInput` 进 MCP 合并（pool）与命令面。
// 准备面必须在同样的事实上决定「读不读插件目录」，否则恢复 / fork 会出现两份
// 决定：能力面部分闭合，或先读插件内容再藏目录。

/// 进程级 HOME 覆盖（与 `requests_test` 的守卫同形；`#[serial]` 串行化）。
struct PreparedHomeGuard {
    home: Option<std::ffi::OsString>,
}

impl PreparedHomeGuard {
    fn set(path: &Path) -> Self {
        let home = std::env::var_os("HOME");
        std::env::set_var("HOME", path);
        Self { home }
    }
}

impl Drop for PreparedHomeGuard {
    fn drop(&mut self) {
        match self.home.take() {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }
}

/// 会话级装配（含插件能力）：`startup_cwd` 取会话 cwd 的规范化形态 ⇒ 准备面
/// 复用 host 内存配置（当轮 session-local config 由测试直接控制）。
fn plugin_workspace_assembly(cwd: &str) -> WorkspaceAssembly {
    WorkspaceAssembly {
        startup_cwd: cwd.to_owned(),
        bare: false,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
        capabilities: super::assemble::HostCapabilities {
            plugins: true,
            ..Default::default()
        },
    }
}

fn peri_config_with_plugin_face(closed: bool) -> crate::provider::PeriConfig {
    let mut config = crate::provider::PeriConfig::default();
    config.config.meta_harness = Some(HashMap::from([("PluginMiddleware".to_string(), !closed)]));
    config
}

async fn host_with_plugin_face(tmp: &TempDir, cwd: &str, closed: bool) -> AcpServerConfig {
    let mut host = prepared_test_host(tmp, Some(plugin_workspace_assembly(cwd))).await;
    host.peri_config = Arc::new(parking_lot::RwLock::new(peri_config_with_plugin_face(
        closed,
    )));
    host
}

/// 在 HOME/.claude 下装一个已启用插件（含命令与技能根）。
fn seed_enabled_plugin(home: &Path) -> PathBuf {
    let plugin_dir = home.join(".claude/plugins/cache/market/sample/1.0.0");
    std::fs::create_dir_all(plugin_dir.join(".claude-plugin")).unwrap();
    std::fs::create_dir_all(plugin_dir.join("commands")).unwrap();
    std::fs::create_dir_all(plugin_dir.join("skills/demo")).unwrap();
    std::fs::write(
        plugin_dir.join("commands/hello.md"),
        "---\ndescription: demo command\n---\nBody\n",
    )
    .unwrap();
    std::fs::write(
        plugin_dir.join("skills/demo/SKILL.md"),
        "---\nname: demo\ndescription: demo skill\n---\nBody\n",
    )
    .unwrap();
    write_plugin_manifest(&plugin_dir, r#"{"srv":{"command":"run-srv"}}"#);
    std::fs::create_dir_all(home.join(".claude/plugins")).unwrap();
    std::fs::write(
        home.join(".claude/plugins/installed_plugins.json"),
        serde_json::json!({
            "version": 2,
            "plugins": [{
                "id": "sample@market",
                "name": "sample",
                "version": "1.0.0",
                "marketplace": "market",
                "install_path": plugin_dir,
                "scope": "User",
            }],
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        home.join(".claude/settings.json"),
        r#"{"enabledPlugins":["sample@market"]}"#,
    )
    .unwrap();
    plugin_dir
}

fn write_plugin_manifest(plugin_dir: &Path, mcp_servers: &str) {
    std::fs::write(
        plugin_dir.join(".claude-plugin/plugin.json"),
        format!(
            r#"{{"name":"sample","version":"1.0.0","skills":["./skills"],"commands":["./commands"],"mcpServers":{mcp_servers}}}"#
        ),
    )
    .unwrap();
}

/// 非法插件 MCP 声明：严格只读路径会失败——准备面读插件目录就会被发现。
fn poison_plugin_manifest(plugin_dir: &Path) {
    write_plugin_manifest(plugin_dir, r#"{"broken":{"system_mcp_tools":["Read"]}}"#);
}

#[tokio::test]
#[serial]
async fn restore_and_fork_take_the_plugin_decision_from_the_frozen_snapshot() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let _home = PreparedHomeGuard::set(&home);
    let plugin_dir = seed_enabled_plugin(&home);
    let cwd = canonical_workspace_dir(&tmp, "workspace");

    let host_closed = host_with_plugin_face(&tmp, &cwd, true).await;
    let host_open = host_with_plugin_face(&tmp, &cwd, false).await;

    // 方向 A：frozen=关 + 当轮=开 ⇒ 准备面不得读插件目录（先读再藏即被 poison 抓到）。
    let closed_prepared = PreparedSessionInputs::prepare_new(&host_closed, &cwd).unwrap();
    assert!(
        closed_prepared.plugins().data.is_none(),
        "新建（frozen 缺席）按 session-local 配置判关闭"
    );
    let closed_snapshot = closed_prepared.frozen_encoded.clone().unwrap();
    poison_plugin_manifest(&plugin_dir);

    let restored = PreparedSessionInputs::prepare_restore(&host_open, &cwd, &closed_snapshot)
        .expect("frozen=关 时准备面不得读插件目录（非法清单不得让恢复失败）");
    assert!(
        restored.plugins().data.is_none() && restored.plugins().skill_roots.is_empty(),
        "frozen=关 必须胜过当轮 config=开"
    );
    assert!(
        restored
            .frozen
            .as_ref()
            .unwrap()
            .meta_harness()
            .disabled_middlewares
            .contains("PluginMiddleware"),
        "同一位也是装配面派生 plugin_face_closed / pool.set_plugin_face_closed 的来源"
    );
    let forked = PreparedSessionInputs::prepare_fork(&host_open, &cwd, &closed_snapshot).unwrap();
    assert!(
        forked.plugins().data.is_none(),
        "fork 与 restore 必须消费同一份 frozen 决定"
    );

    // 方向 B：frozen=开 + 当轮=关 ⇒ 准备面仍按 frozen 取来源（不得被当轮 config 关掉）。
    write_plugin_manifest(&plugin_dir, r#"{"srv":{"command":"run-srv"}}"#);
    let open_prepared = PreparedSessionInputs::prepare_new(&host_open, &cwd).unwrap();
    assert!(
        open_prepared.plugins().data.is_some(),
        "前置：开启位的宿主必须真的加载到插件聚合"
    );
    let open_snapshot = open_prepared.frozen_encoded.clone().unwrap();

    let restored_open =
        PreparedSessionInputs::prepare_restore(&host_closed, &cwd, &open_snapshot).unwrap();
    assert!(
        restored_open.plugins().data.is_some(),
        "frozen=开 必须胜过当轮 config=关（否则 MCP 合并与技能/命令面会分裂）"
    );
    assert!(!restored_open.plugins().skill_roots.is_empty());
    assert!(!restored_open
        .frozen
        .as_ref()
        .unwrap()
        .meta_harness()
        .disabled_middlewares
        .contains("PluginMiddleware"));
}
