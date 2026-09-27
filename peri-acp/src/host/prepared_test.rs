//! 会话准备输入（`PreparedSessionInputs`）的目标测试：
//! 同输入复用、frozen 字节同源、按需复用 host 配置、fork 复用源字节、准备无写、
//! 发布段只消费给定的准备对象（不重读外部输入）。

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use peri_middlewares::permission::shared_mode::{PermissionMode, SharedPermissionMode};
use tempfile::TempDir;

use super::assemble::WorkspaceAssembly;
use super::prepared::PreparedSessionInputs;
use super::AcpServerConfig;
use crate::provider::{LlmProvider, ProviderConfig, ProviderModels};
use crate::session::frozen_snapshot::{decode_frozen_snapshot, encode_frozen_snapshot};

/// 准备期冻结的外部输入（cwd/CLAUDE.md 内容）。准备之后改写它，用来证明发布段
/// 不再读盘；两次内容不同使「二次构建」与「原样消费」的字节可区分。
const FROZEN_INPUT_AT_PREPARATION: &str = "frozen-seam: content frozen at preparation\n";
const FROZEN_INPUT_AFTER_PREPARATION: &str = "frozen-seam: rewritten after preparation\n";

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
        Arc::new(peri_middlewares::host_ports::SkillsProvider),
        Vec::new(),
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
        channel_state: None,
        plugin_skill_roots: Vec::new(),
        plugin_command_entries: Vec::new(),
        plugin_agent_dirs: Vec::new(),
        plugin_hooks: Vec::new(),
        plugin_hooks_only: Vec::new(),
        plugin_loaded: Vec::new(),
        hook_groups: Vec::new(),
        plugin_lsp_servers: Vec::new(),
        tool_search_index: Arc::new(peri_middlewares::tool_search::ToolSearchIndex::new()),
        skills: Arc::new(peri_middlewares::host_ports::SkillsProvider),
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
        first.frozen_encoded, second.frozen_encoded,
        "同一准备输入必须产出同一 snapshot 字节"
    );
    assert_eq!(first.frozen.date(), second.frozen.date());
    let decoded = decode_frozen_snapshot(&first.frozen_encoded).unwrap();
    assert_eq!(
        decoded.date(),
        first.frozen.date(),
        "snapshot 字节与内存冻结数据必须同源（日期不各取一份）"
    );
    assert_eq!(decoded.system_prompt(), first.frozen.system_prompt());
    assert_eq!(first.skill_roots.len(), second.skill_roots.len());
    assert_eq!(first.agent_dirs.len(), second.agent_dirs.len());
}

/// 准备阶段不写会话数据/登记：整个数据目录逐字节不变。
#[tokio::test]
async fn prepare_new_writes_no_session_state() {
    let tmp = TempDir::new().unwrap();
    let cwd = make_workspace_dir(&tmp, "workspace");
    let host = prepared_test_host(&tmp, None).await;

    let before = snapshot_tree(tmp.path());
    let prepared = PreparedSessionInputs::prepare_new(&host, &cwd).unwrap();
    assert!(!prepared.frozen_encoded.is_empty());
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
            mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
        }),
    )
    .await;

    let prepared = PreparedSessionInputs::prepare_new(&host, &cwd).unwrap();
    assert!(
        Arc::ptr_eq(&prepared.config_source, &host.config_source),
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
    let forked =
        PreparedSessionInputs::prepare_fork(&host, &fork_cwd, &source.frozen_encoded).unwrap();

    assert_eq!(
        forked.frozen_encoded, source.frozen_encoded,
        "fork 必须保留 source 的精确 frozen 字节"
    );
    assert_eq!(forked.frozen.date(), source.frozen.date());
    assert_eq!(forked.frozen.system_prompt(), source.frozen.system_prompt());
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
        PreparedSessionInputs::prepare_legacy(&host, &registered_raw, &workspace_cwd).unwrap();

    assert_eq!(legacy.cwd, workspace_cwd);
    assert_eq!(
        legacy.legacy.as_ref().unwrap().saved_cwd,
        PathBuf::from(&registered_raw),
        "legacy 必须记录保存的绝对 cwd（不是调用方终端的 cwd）"
    );
    assert!(decode_frozen_snapshot(&legacy.frozen_encoded).is_ok());
}

/// new 路径的持久化属性：frozen 字节与发布到 live state 的冻结状态同源（一次写入），
/// binding 与 owner 同时成立。
///
/// 端到端只走生产入口（resolve → 准备一次 → 发布段），不拿第二次准备比字节：断言
/// 的是同一次创建内部的自洽——持久化字节、live frozen 与本次工作区输入同源。
#[tokio::test]
async fn new_session_persists_frozen_bytes_from_its_single_preparation() {
    let tmp = TempDir::new().unwrap();
    let host = prepared_test_host(&tmp, None).await;
    let cwd = canonical_workspace_dir(&tmp, "workspace");
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
    assert!(
        sessions[&id].execution_owner.is_some(),
        "创建必须给出执行 owner"
    );
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
        "持久化字节必须来自本次工作区输入（准备期读到的 CLAUDE.md）"
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

/// 发布段只消费给定的准备对象：准备定格后改写外部输入，保存字节与 live 状态仍精确
/// 等于准备时的字节。
///
/// `new_session_from_prepared` 就是 `handle_new` 准备之后的同一条生产路径。若将来有人
/// 在这里再准备一次（重读配置/重建 frozen），`rebuilt` 哨兵与 `persisted` 断言都会
/// 失败——本用例不靠「两次准备相等」证明同源。
#[tokio::test]
async fn new_session_from_prepared_does_not_reread_external_frozen_inputs() {
    let tmp = TempDir::new().unwrap();
    let host = prepared_test_host(&tmp, None).await;
    let cwd = canonical_workspace_dir(&tmp, "workspace");
    let claude_md = Path::new(&cwd).join("CLAUDE.md");
    std::fs::write(&claude_md, FROZEN_INPUT_AT_PREPARATION).unwrap();

    let prepared = PreparedSessionInputs::prepare_new(&host, &cwd).unwrap();
    assert_eq!(
        prepared.frozen.claude_md(),
        Some(FROZEN_INPUT_AT_PREPARATION),
        "准备必须定格当时的外部输入"
    );

    // 准备之后改写外部输入：任何重读/重建都会给出不同字节。
    std::fs::write(&claude_md, FROZEN_INPUT_AFTER_PREPARATION).unwrap();
    let rebuilt = PreparedSessionInputs::prepare_new(&host, &cwd).unwrap();
    assert_eq!(
        rebuilt.frozen.claude_md(),
        Some(FROZEN_INPUT_AFTER_PREPARATION)
    );
    assert_ne!(
        rebuilt.frozen_encoded, prepared.frozen_encoded,
        "哨兵：外部输入已变，二次准备必须产出不同字节——否则本用例无法证明未重读"
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
        &prepared,
        &mut sessions,
    )
    .await
    .expect("发布段必须用给定准备对象创建成功");

    let id = response["sessionId"]
        .as_str()
        .expect("response carries sessionId")
        .to_owned();
    assert!(
        sessions[&id].execution_owner.is_some(),
        "创建必须给出执行 owner"
    );
    let snapshot = host
        .session_resources
        .load_session_snapshot(&id)
        .await
        .expect("created session must be readable through the facade");
    let persisted = match snapshot.frozen {
        peri_acp_types::session_resources::FrozenState::Present(bytes) => bytes.into_string(),
        other => panic!("frozen must be present after create_session: {other:?}"),
    };
    assert_eq!(
        persisted, prepared.frozen_encoded,
        "保存字节必须精确等于给定准备输入的字节"
    );
    let live = sessions[&id]
        .frozen
        .clone()
        .expect("创建必须发布 live frozen");
    assert_eq!(
        encode_frozen_snapshot(&live).unwrap(),
        prepared.frozen_encoded,
        "live frozen 必须与准备输入逐字节同源"
    );
    assert_eq!(
        live.claude_md(),
        Some(FROZEN_INPUT_AT_PREPARATION),
        "准备之后写入的外部内容不得进入发布的冻结状态"
    );
}
