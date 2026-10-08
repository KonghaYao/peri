//! H-04 / V-03：wave 2（cron 实迁为 builtin MCP 实例）的**终态**宿主装配用例。
//!
//! 模块名 `host::mcp_v4_wave2`（由 `peri-acp/src/host/mod.rs` 的 `#[path]` 挂载）：
//! 终态断言只写在本文件（主 plan §8 表头「不能用 baseline 代替」）。
//!
//! ## 覆盖（每条都对应主计划 §8 的具名行）
//!
//! 1. [`wave2_final_first_request_and_deferred_summary`]（§8 第 3 行 / R7 / A4 / A20）
//!    - **首个 LLM 请求**（直连面）：不含任何 `mcp__cron__*`
//!      （四工具 `direct: false`，`system_mcp_tools: []` 不提升 direct）；
//!    - **deferred 目录**（= 搜索/执行面，A20 的唯一等价对照面）：按**有效配置**包含
//!      `mcp__cron__cron_register` / `mcp__cron__cron_list` / `mcp__cron__cron_remove`。
//!
//! ## 为什么本文件自己起 pool（而不是复用 `host::mcp_v4_wire_fixture`）
//!
//! A33 要求实例上下文在 `run_initialize` **之前**注入。`WireFixtureHarness`
//! （`peri-acp/src/host/mcp_v4_wire_fixture_test.rs`，owner 序列 V-06/V-02，本波不改）
//! 的 host 装配里**没有**这一步（它起的 pool 从未注入上下文），因此它的 pool 里
//! builtin 实例一律以 `ContextMissing` 收口——迁移前的 wave 1 用例只断言
//! web/artifact 的**裸名/生效名存在性对照**，与该缺口无关；wave 2 的终态面必须
//! 有真实连上的 cron 实例，否则断言面是空的。
//!
//! 本文件因此复刻**生产装配的同一步骤序列**（`peri-acp/src/host/assemble.rs`）：
//! 构造 pool → 构造 `BuiltinInstanceContext`（同一批 `Arc`）→ 注入 → `run_initialize`。
//! 复用的是同属 `host` 树的 model 替身与 prompt 驱动
//! （`host::mcp_v4_wire_fixture::{WireScriptedModel, run_wire_prompt}` 与
//! `host::executor_flow_tests::{MockEventSink, make_session_context}`），
//! 不复制它们一行实现。
//!
//! 本文件因此复刻**生产装配的同一步骤序列**（`peri-acp/src/host/assemble.rs`）：
//! 构造 pool → 构造 `BuiltinInstanceContext`（同一批 `Arc`）→ 注入 → `run_initialize`。
//! 复用的是同属 `host` 树的 model 替身与 prompt 驱动
//! （`host::mcp_v4_wire_fixture::{WireScriptedModel, run_wire_prompt}` 与
//! `host::executor_flow_tests::{MockEventSink, make_session_context}`），
//! 不复制它们一行实现。
//!
//! ## 与生产装配的差异（显式登记，避免把夹具当生产）
//!
//! - 夹具 cwd 是临时 workspace，HOME 重定向到临时目录（与 wave 1 夹具同款隔离），
//!   因此读不到开发者本机的 `~/.claude.json` / `~/.peri/settings.json`；
//! - 夹具**不**起 ACP server，直接以 `run_wire_prompt` 驱动一个 turn：断言面是
//!   「首个 LLM 请求 + 搜索面」，与宿主装配顺序无关的部分（transport、审批、TUI）
//!   由 V-02/V-03 的其他文件覆盖。

use crate::transport::AcpTransport;
use peri_acp_types::{
    interaction::UserInteractionBroker,
    session::{ExecutionFailureKind, MessageSource},
};
use peri_model::Model;
use serde_json::json;

use std::{
    collections::HashMap,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
};

use peri_acp_types::{
    builtin_mcp::find as find_builtin_instance,
    messages::BaseMessage,
    permission::{PermissionMode, SharedPermissionMode},
    ports::McpPoolPort,
};
use peri_middlewares::{
    assembly::{BuiltinContextError, BuiltinInstanceContext, CronInstanceInput},
    mcp::{ClientStatus, McpClientPool, McpInitStatus, McpTaskOwner},
    tool_search::{core_tools, SEARCH_EXTRA_TOOLS_NAME},
};
use serial_test::serial;

use super::{
    assemble::HostAssemblyInput,
    connection::ConnectionContext,
    executor_flow_tests::{MockEventSink, SessionTaskBindings},
    mcp_v4_wire_fixture::{run_wire_prompt, ScriptedToolCall, WireScriptedModel},
    AcpServerConfig, PromptLocks, SharedSessions,
};
use crate::{
    provider::{ConfigSource, LlmProvider, PeriConfig, ProviderConfig, ProviderModels},
    session::executor::{PromptResult, SessionContext},
    transport::stdio::StdioTransport,
};

// ── 夹具 1：HOME 重定向（进程级 env ⇒ 必须 `#[serial]`）────────────────────────

/// 把 `HOME` 重定向到临时目录：`config_path()`（`~/.peri/settings.json`）、
/// `dirs_next::home_dir()` 的插件目录都跟随 `HOME`，否则用例会读开发者本机配置。
///
/// 与 wave 1 夹具（`WireHomeRedirect`）同形但不共享（那个是私有类型）；两者都要求
/// 调用方标 `#[serial]`——HOME 是进程全局状态，`serial_test` 保证带该属性的用例
/// 互斥执行。
struct HomeRedirect {
    previous: Option<OsString>,
}

impl HomeRedirect {
    fn set(home: &Path) -> Self {
        let previous = std::env::var_os("HOME");
        std::env::set_var("HOME", home);
        Self { previous }
    }
}

impl Drop for HomeRedirect {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }
}

/// 临时工作区：临时目录 + 重定向后的 HOME + workspace + claude home。
struct FixtureDirs {
    tmp: tempfile::TempDir,
    _home_guard: HomeRedirect,
    workspace: PathBuf,
    claude_home: PathBuf,
}

impl FixtureDirs {
    fn new() -> Self {
        let tmp = tempfile::TempDir::new().expect("临时目录");
        let home = tmp.path().join("home");
        let workspace = tmp.path().join("workspace");
        let claude_home = tmp.path().join("claude");
        for dir in [&home, &workspace, &claude_home] {
            std::fs::create_dir_all(dir).expect("创建临时目录");
        }
        let guard = HomeRedirect::set(&home);
        Self {
            tmp,
            _home_guard: guard,
            workspace,
            claude_home,
        }
    }

    fn workspace_str(&self) -> String {
        self.workspace.to_string_lossy().into_owned()
    }

    /// 第二个工作目录（多 cwd 退化用例：与 `workspace` 不同的根）。
    fn second_workspace(&self) -> PathBuf {
        let second = self.tmp.path().join("workspace-second");
        std::fs::create_dir_all(&second).expect("第二个 cwd");
        second
    }
}

// ── 夹具 2：生产同构的 builtin host（pool + 上下文注入 + run_initialize）───────

/// 生产同构的 builtin 宿主：真实 pool + **注入过的** 实例上下文 + 真实
/// `run_initialize`（A33 的顺序：pool → 上下文 → 注入 → initialize）。
struct BuiltinHostFixture {
    dirs: FixtureDirs,
    pool: Arc<McpClientPool>,
    _owner: McpTaskOwner,
    /// 会话任务管理器登记（`session_context` 按生产语义绑定）。
    session_tasks: SessionTaskBindings,
}

impl BuiltinHostFixture {
    /// `tick_enabled = false` ⇒ 不挂 tick 驱动（工具面 / 归一用例不需要）。
    async fn start(dirs: FixtureDirs, tick_enabled: bool) -> Self {
        let cwd = dirs.workspace_str();
        let (cron_trigger_tx, _cron_triggers) = tokio::sync::mpsc::unbounded_channel();
        let scheduler = Arc::new(parking_lot::Mutex::new(peri_mcp_cron::CronScheduler::new(
            cron_trigger_tx,
        )));
        let context = BuiltinInstanceContext::new(cwd.clone()).with_cron(CronInstanceInput {
            scheduler,
            tick_enabled,
        });
        Self::start_with_context(dirs, context).await
    }

    /// 注入**调用方给定**的上下文后跑真实 `run_initialize`。
    ///
    /// 与 [`Self::start`] 的唯一差别是上下文来源：W3 的故障注入（上下文缺某类输入）与
    /// tick 观测（调用方自己持有 scheduler / 触发通道）需要这一份入口，装配步骤序列
    /// （pool → 绑定 cwd → 注入 → initialize）与生产逐位相同。
    ///
    /// 「初始化收口为 `Ready`」的断言在这里同样成立：**单实例**装配失败不等于 pool 级
    /// 失败（`Ready { total }` 只统计连上的实例）。
    async fn start_with_context(dirs: FixtureDirs, context: BuiltinInstanceContext) -> Self {
        let (owner, spawner) = McpTaskOwner::new();
        let pool = Arc::new(McpClientPool::new_pending_with_spawner(spawner));
        // 与生产装配同一步骤（`host/assemble.rs::pending_mcp_pool`）：绑定 host cwd。
        pool.bind_execution_cwd(&dirs.workspace)
            .expect("夹具第一次绑定 execution cwd");
        // A33：上下文注入必须早于 `run_initialize`（含其后台 spawn 窗口）。
        pool.set_builtin_instance_context(Arc::new(context))
            .expect("首次注入必须成功（夹具不会二次注入）");

        let (status_tx, mut status_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);
        let init_pool = Arc::clone(&pool);
        let workspace = dirs.workspace.clone();
        let claude_home = dirs.claude_home.clone();
        let init_task = tokio::spawn(async move {
            McpClientPool::run_initialize(init_pool, &workspace, &claude_home, status_tx, None)
                .await;
        });
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                if matches!(
                    &*status_rx.borrow_and_update(),
                    McpInitStatus::Ready { .. } | McpInitStatus::Failed(_)
                ) {
                    break;
                }
                status_rx
                    .changed()
                    .await
                    .expect("init status 发送端在本任务内");
            }
        })
        .await
        .expect("真实 MCP 初始化不得挂起");
        tokio::time::timeout(std::time::Duration::from_secs(30), init_task)
            .await
            .expect("初始化任务必须在有界等待内结束")
            .expect("初始化任务不得 panic");
        assert!(
            matches!(&*status_rx.borrow(), McpInitStatus::Ready { .. }),
            "生产同构装配必须收口为 Ready（失败 ⇒ builtin 实例未连上，断言面空洞）: {:?}",
            *status_rx.borrow()
        );
        Self {
            dirs,
            pool,
            _owner: owner,
            session_tasks: SessionTaskBindings::default(),
        }
    }

    /// 断言实例真实连上（未连上时断言面空洞，必须先证伪这一条）。
    fn assert_instance_connected(&self, instance: &str) {
        let handle = self
            .pool
            .get_client(instance)
            .unwrap_or_else(|| panic!("{instance} 必须完成连接"));
        assert!(
            matches!(handle.status, ClientStatus::Connected),
            "{instance} 必须是 Connected，实际: {:?}",
            handle.status
        );
    }

    /// 注入该 pool 的 session 装配面（与
    /// `host::mcp_v4_wire_fixture::WireFixtureHarness::session_context` 同形），
    /// 并按生产语义绑定本会话的任务管理器（`bind_session_tasks` 的会话侧一半）。
    async fn session_context(&self, session_id: &str) -> SessionContext {
        let mut ctx = super::executor_flow_tests::make_session_context(session_id).await;
        ctx.cwd = self.dirs.workspace_str();
        ctx.mcp_pool = Some(Arc::clone(&self.pool) as Arc<dyn McpPoolPort>);
        self.session_tasks.bind(&self.pool, &ctx);
        ctx
    }
}

// ── 构造宿主 AcpServerConfig（生产装配面）─────────────────────────────────────

fn peri_config_with_provider() -> PeriConfig {
    let mut peri_config = PeriConfig::default();
    peri_config.config.active_alias = "sonnet".to_string();
    peri_config.config.providers = vec![ProviderConfig {
        id: "a".to_string(),
        provider_type: "openai".to_string(),
        // 夹具占位符：不经网络，不是任何真实凭据。
        api_key: "fixture-placeholder-unused".to_string(),
        models: ProviderModels {
            sonnet: "gpt-4o".to_string(),
            ..Default::default()
        },
        ..Default::default()
    }];
    peri_config
}

/// 经**生产装配**构造 host 部署配置。
///
/// `session_resources = true`（与 `peri-acp/src/host/workspace.rs` 的会话环境装配
/// 同源）⇒ `workspace_assembly` 为 `None` ⇒ session 不再各自分裂出「会话环境」
/// （每个环境会自带一份部署配置与 pool），因此同一部署单元内的多个 session 共享
/// 这一份 host pool。
///
/// `drive_cron_tick` 是**既有装配开关**（TUI 路径 true ⇒ builtin cron 实例那一代挂
/// 1s tick 驱动；print/stdio false ⇒ 无 tick）。W3 的 cron 端到端与 tick 计数用例取
/// true；其余用例保持 false（与既有用例逐位相同）。
async fn assemble_host_with_tick(dirs: &FixtureDirs, drive_cron_tick: bool) -> AcpServerConfig {
    assemble_host_with_workspace_input(dirs, drive_cron_tick, None).await
}

/// 同上，但 builtin `workspace` 实例的 session 级输入（AW3-11 的
/// `WorkspaceInstanceInput`）由调用方给定。
///
/// 夹具默认传 `None`（[`assemble_host_with_tick`] 的全部既有调用点）：`workspace`
/// 实例可见但退化，本文件其余用例的断言面（cron）不含该输入。1:N root 形态的
/// 补验用例 [`wave3_one_to_n_root_never_injects_per_session_workspace_input`] 传
/// `Some`——该形态下这份输入只在 **root** 装配时进一次 builtin 上下文，session 侧
/// 没有任何路径能再注入它或取回它（[`super::workspace::SessionEnvironment::assemble`]
/// 恒返回 `Ok(None)`）。
async fn assemble_host_with_workspace_input(
    dirs: &FixtureDirs,
    drive_cron_tick: bool,
    workspace_input: Option<peri_mcp_workspace::WorkspaceInstanceInput>,
) -> AcpServerConfig {
    let peri_config = peri_config_with_provider();
    let provider = LlmProvider::from_config(&peri_config).expect("夹具 provider");
    let session_resources =
        peri_agent::resources::open_session_resources_with(Some(dirs.workspace.join("threads.db")))
            .await
            .unwrap();
    let config_source = Arc::new(
        ConfigSource::load_at(&dirs.workspace, crate::provider::config_path()).expect("夹具配置源"),
    );
    super::assemble::assemble_server_config_with_mcp_profile(
        HostAssemblyInput {
            provider,
            peri_config: Arc::new(parking_lot::RwLock::new(peri_config)),
            config_source,
            permission_mode: peri_acp_types::permission::SharedPermissionMode::new(
                peri_acp_types::permission::PermissionMode::Bypass,
            ),
            session_resources,
            workspace_id: None,
            session_store_shutdown: None,
            cwd: dirs.workspace_str(),
            bare: false,
            drive_cron_tick,
            // session 级 `workspace` 输入（AW3-11）由调用方给定：默认 `None` ⇒
            // 可见但退化，本文件断言面（cron）不含它。
            workspace_input,
            // 资源面输入同为会话级（见上）：本文件断言面不含它，恒为 `None`。
            workspace_resources: None,
            // 无会话上下文（测试夹具）：A24 关闭集为空集。
            builtin_closed: Default::default(),
            // 宿主技能面关闭位与关闭集同源：本夹具无会话上下文，恒为假。
            skills_face_closed: false,
            plugin_face_closed: false,
            prepared_plugins: None,
            session_mcp_servers: None,
        },
        peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
        true,
        None,
        Default::default(),
    )
    .await
}

/// 不起 server 的 transport（`handle_new` 只在其中推通知，本文件不读输出）。
fn idle_transport() -> Arc<dyn AcpTransport> {
    let (client_write, transport_read) = tokio::io::duplex(64 * 1024);
    let (transport_write, _client_read) = tokio::io::duplex(64 * 1024);
    std::mem::forget(client_write);
    Arc::new(StdioTransport::from_reader_writer(
        transport_read,
        transport_write,
    ))
}

// ── 用例 2：首个 LLM 请求的直连面 + deferred 目录面 ───────────────────────────

/// 一次 prompt 的观察面：首个请求的直连工具名 + 搜索结果里的工具名。
struct ToolFaces {
    direct: Vec<String>,
    searched: Vec<String>,
}

/// 脚本化调用一次 `SearchExtraTools`（A20 的唯一等价对照面），取回两类工具名。
///
/// `max_results = 50`（与 wave 1 基线同口径）：`search()` 的默认 5 会把断言变成
/// 「必须排进前 5」，那依赖排序而与迁移语义无关。脚本 = 1 次工具调用 + 1 次收尾
/// ⇒ 恰好 2 次模型调用。
async fn probe_tool_faces(label: &str, ctx: SessionContext, query: &str) -> ToolFaces {
    let sink = Arc::new(MockEventSink::new());
    let model = Arc::new(WireScriptedModel::new(vec![ScriptedToolCall::new(
        SEARCH_EXTRA_TOOLS_NAME,
        serde_json::json!({ "query": query, "max_results": 50 }),
    )]));
    let result = run_wire_prompt(ctx, &sink, &model).await;
    assert!(
        result.ok,
        "{label}：探针 prompt 必须正常结束: stop={:?}",
        result.stop_reason
    );
    assert_eq!(
        model.call_count(),
        2,
        "{label}：一次工具调用 + 一次收尾 = 恰好 2 次模型调用"
    );
    let searched = searched_tool_names(&result);
    println!(
        "[W2-V03 终态] {label}：查询 = {query:?}（max_results = 50）；首个请求直连工具 = {:?}；搜索面命中工具 = {searched:?}",
        model.first_request_tool_names()
    );
    ToolFaces {
        direct: model.first_request_tool_names(),
        searched,
    }
}

/// 搜索结果里的工具名（`SearchExtraTools` 的 JSON 原文，形状与 wave 1 基线同源）。
fn searched_tool_names(result: &PromptResult) -> Vec<String> {
    result
        .messages
        .iter()
        .filter(|message| matches!(message, BaseMessage::Tool { .. }))
        .filter_map(|message| serde_json::from_str::<serde_json::Value>(&message.content()).ok())
        .filter_map(|value| value.get("results")?.as_array().cloned())
        .flatten()
        .filter_map(|entry| {
            entry
                .get("name")
                .and_then(|name| name.as_str())
                .map(str::to_string)
        })
        .collect()
}

/// 注册表声明的原始工具名（`tools/list` 的唯一事实源）。
fn declared_tool_names(instance: &str) -> Vec<&'static str> {
    find_builtin_instance(instance)
        .unwrap_or_else(|| panic!("{instance} 必须在 builtin 注册表内"))
        .tools
        .iter()
        .map(|tool| tool.original_name)
        .collect()
}

/// 注册表声明的 effective name（模型可见名）。
fn declared_effective_names(instance: &str) -> Vec<&'static str> {
    find_builtin_instance(instance)
        .unwrap_or_else(|| panic!("{instance} 必须在 builtin 注册表内"))
        .tools
        .iter()
        .map(|tool| tool.effective_name)
        .collect()
}

/// 注册表声明为 `direct: true` 的 effective name。
fn declared_direct_effective_names(instance: &str) -> Vec<&'static str> {
    find_builtin_instance(instance)
        .unwrap_or_else(|| panic!("{instance} 必须在 builtin 注册表内"))
        .tools
        .iter()
        .filter(|tool| tool.direct)
        .map(|tool| tool.effective_name)
        .collect()
}

/// 面板投影行（`all_server_infos`）：用户可见面与句柄面分开取证，两者都要有。
fn panel_row(pool: &McpClientPool, instance: &str) -> peri_middlewares::mcp::ServerInfo {
    pool.all_server_infos()
        .into_iter()
        .find(|info| info.name == instance)
        .unwrap_or_else(|| {
            panic!("{instance} 必须在面板投影里可见（含 failed / uninitialized 行）")
        })
}

/// 逐实例的 ready 取证：四段证据各有**独立**判据，缺任一段都失败。
///
/// ① transport：`peer` 在（句柄不是「光秃的 Connected」）；
/// ② 协议初始化：modern 握手协商出 `peer_info.server_info`，且句柄版本与它同源
///    （历史缓存不得冒充本次协商）；
/// ③ 能力协商：server 声明 `tools` 能力（五个 handler 只声明 tools）；
/// ④ `tools/list`：live 名单与注册表声明**逐字逐序**相等（不是「非空」就算完）。
///
/// 面板行同时取证（`transport_type == "builtin"` 的运行时身份 + `status_label`）。
fn assert_instance_ready(pool: &McpClientPool, instance: &str, expected_tools: &[&str]) {
    let handle = pool
        .get_client(instance)
        .unwrap_or_else(|| panic!("{instance} 必须完成连接（未发布 ready）"));
    assert!(
        matches!(handle.status, ClientStatus::Connected),
        "{instance} 必须经同一条处理链提交 Connected，实际: {:?}",
        handle.status
    );
    let peer = handle
        .peer
        .as_ref()
        .unwrap_or_else(|| panic!("{instance} 的 transport 必须留下 peer"));
    let peer_info = peer
        .peer_info()
        .unwrap_or_else(|| panic!("{instance} 的协议初始化必须协商出 peer_info"));
    let server_info = peer_info
        .server_info
        .as_ref()
        .unwrap_or_else(|| panic!("{instance} 必须协商出 server_info（真实 handler 的 get_info）"));
    assert!(
        peer_info.capabilities.tools.is_some(),
        "{instance} 必须协商出 tools 能力（五个 builtin handler 只声明 tools）"
    );
    assert_eq!(
        handle.version.as_deref(),
        Some(server_info.version.as_str()),
        "{instance} 句柄版本必须来自本次协商的 server_info，不得是历史缓存"
    );
    let served: Vec<&str> = handle.tools.iter().map(|tool| tool.name.as_ref()).collect();
    assert_eq!(
        served.as_slice(),
        expected_tools,
        "{instance} 的 live tools/list 必须与注册表声明逐字逐序相等"
    );
    let row = panel_row(pool, instance);
    assert_eq!(
        row.transport_type, "builtin",
        "{instance} 的面板 transport 身份必须来自运行时 `ConfigSource::Builtin`"
    );
    assert_eq!(
        row.status_label, "connected",
        "{instance} 的面板状态标签必须是 connected"
    );
    assert_eq!(
        row.tool_count,
        expected_tools.len(),
        "{instance} 的面板 tool_count 必须等于 tools/list 条数"
    );
    println!(
        "[W2 ready] instance={instance} transport=peer(未关闭)/协议初始化=server_info({})/能力协商=tools/工具面={served:?}",
        server_info.version
    );
}

/// 逐实例的「**未**发布 ready」取证 + typed 失败原因（失败不得伪装成任何 ready 证据）。
fn assert_no_ready_evidence(pool: &McpClientPool, instance: &str, expect_reason: &str) {
    let handle = pool
        .get_client(instance)
        .unwrap_or_else(|| panic!("{instance} 必须留下失败收口句柄（不得静默消失）"));
    let reason = match &handle.status {
        ClientStatus::Failed(reason) => reason.clone(),
        other => panic!("{instance} 必须收口为 Failed，实际: {other:?}"),
    };
    assert!(
        reason.contains(expect_reason),
        "{instance} 的失败原因必须含 typed 短语 `{expect_reason}`，实际: {reason}"
    );
    assert!(
        handle.peer.is_none(),
        "{instance} 失败后不得留下 transport（不得发布 ready 证据）"
    );
    assert!(
        handle.version.is_none(),
        "{instance} 失败后不得留下协议初始化证据"
    );
    assert!(
        handle.tools.is_empty(),
        "{instance} 失败后不得留下工具清单: {:?}",
        handle
            .tools
            .iter()
            .map(|tool| tool.name.to_string())
            .collect::<Vec<_>>()
    );
    let row = panel_row(pool, instance);
    assert_eq!(
        row.status_label, "failed",
        "{instance} 的面板状态标签必须是 failed"
    );
    assert_eq!(row.tool_count, 0, "{instance} 失败行的 tool_count 必须是 0");
    println!(
        "[W2 ready] instance={instance} transport=无/协议初始化=无/工具面=[]/ready=未发布/原因={reason}"
    );
}

/// 有界收集 tick 触发（触发个数可以是 0，用于「无 tick」的可证伪观测）。
///
/// 只从**调用方持有的**接收端读数：`CronScheduler::tick` 触发到期任务时向该通道发送
/// `CronTrigger`，因此「窗口内收到几条」是 tick 是否被驱动的直接投影。
/// 四个 builtin 实例与各自**注册表声明的原始工具名**（顺序即 `tools/list` 顺序）。
///
/// 内联期望是刻意的：用例自己先断言「注册表声明 == 本表」，避免把用例的臆想当成契约。
///
/// v4-part-4（wave 3）：`workspace` 随第 4 个实例加入本表——生产装配的 builtin 池由
/// 三个变四个是本波的**目标事实**（注册表 `BUILTIN_MCP_INSTANCES`），因此
/// 「池内全部实例 ready / 裸名不得复活 / off 下零注入」等断言面必须同批覆盖它，
/// 否则这些断言会在「少盯一个实例」的空洞下继续绿。
const WAVE2_INSTANCES: [(&str, &[&str]); 4] = [
    ("web", &["WebSearch", "WebFetch"]),
    ("artifact", &["artifact"]),
    ("cron", &["cron_register", "cron_list", "cron_remove"]),
    (
        "workspace",
        &[
            "Read",
            "Write",
            "Edit",
            "Glob",
            "Grep",
            "folder_operations",
            "Bash",
        ],
    ),
];

/// `PERI_MCP_BUILTIN=off` 的进程级守卫（A2）。
///
/// 与 `host::mcp_v4_builtin` 的 `BuiltinInjectionOff` **同形但不共享**（那个类型是另一
/// 模块的私有夹具，本任务不得改它）。env 是进程全局状态 ⇒ 必须与 `#[serial]` 一起使用
/// （进程内所有 `#[serial]` 用例互斥，含 HOME 重定向组）。
struct BuiltinInjectionOff {
    previous: Option<OsString>,
}

impl BuiltinInjectionOff {
    const ENV: &'static str = "PERI_MCP_BUILTIN";

    fn set() -> Self {
        let previous = std::env::var_os(Self::ENV);
        std::env::set_var(Self::ENV, "off");
        Self { previous }
    }
}

impl Drop for BuiltinInjectionOff {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var(Self::ENV, value),
            None => std::env::remove_var(Self::ENV),
        }
    }
}

/// `WAVE2_INSTANCES` 的工具面（按实例名查：避免用例按下标引用造成的顺序耦合）。
fn wave2_tools(instance: &str) -> &'static [&'static str] {
    WAVE2_INSTANCES
        .iter()
        .find(|(name, _)| *name == instance)
        .unwrap_or_else(|| panic!("{instance} 必须在 WAVE2_INSTANCES 内"))
        .1
}

use self::mcp_v4_wave2_cron_support_test::*;

#[path = "mcp_v4_wave2_capability_test.rs"]
mod mcp_v4_wave2_capability_test;
#[path = "mcp_v4_wave2_cron_support_test.rs"]
mod mcp_v4_wave2_cron_support_test;
#[path = "mcp_v4_wave2_cron_test.rs"]
mod mcp_v4_wave2_cron_test;
#[path = "mcp_v4_wave2_host_lifecycle_test.rs"]
mod mcp_v4_wave2_host_lifecycle_test;
#[path = "mcp_v4_wave2_readiness_test.rs"]
mod mcp_v4_wave2_readiness_test;
#[path = "mcp_v4_wave2_supervisor_test.rs"]
mod mcp_v4_wave2_supervisor_test;
#[path = "mcp_v4_wave2_tool_surface_test.rs"]
mod mcp_v4_wave2_tool_surface_test;
