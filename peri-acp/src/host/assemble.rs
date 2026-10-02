//! ACP Host 装配——TUI / print / stdio 三路径共用的 host 装配函数。
//!
//! 3.0 目标（`docs/top-level.md` §7/§8）：ACP Host = 部署单元，由 cli/TUI 作为
//! 部署装配点启动；客户端只经 ACP 拿数据。本模块收拢三处此前各自复制的主机
//! 装配（`launch.rs` 内嵌 server 装配、`cli_print.rs` 业务装配、stdio init），
//! 避免装配逻辑漂移。中间件链序事实源仍为 Agent 层 session 工厂
//! （ARC-MIDDLEWARE-001）：本模块只组装 hook 组（顺序与迁移前一致），
//! 不参与链序蓝本。

use std::sync::Arc;

use parking_lot::RwLock;
use peri_acp_types::command::command_route::RouteEntry;
use peri_acp_types::cron::CronSchedulerPort;
use peri_acp_types::hooks::{RegisteredHook, SettingsHooksPort};
use peri_acp_types::mcp::McpSubscriptionPort;
use peri_acp_types::permission::SharedPermissionMode;
use peri_acp_types::plugin::{PluginLoadResult, PluginManagerPort};
use peri_acp_types::ports::{AgentCatalogPort, LspPoolPort, McpPoolPort, ToolSearchPort};
use peri_acp_types::session_resources::{SessionResources, SessionStoreShutdownPort};
use peri_acp_types::skills::SkillRoot;

use crate::provider::{LlmProvider, PeriConfig};
use crate::session::SessionManager;

use super::task_scope::{HostTaskKind, HostTaskOwner, HostTaskOwnerKind};
use super::AcpServerConfig;

/// host 装配输入：调用方（cli/TUI/print/stdio）持有的轻量输入。
///
/// M-TUI 收口（`spec/history/2026-08.md` 2026-08-05 条目，原文见 Git 历史）：
/// middlewares 具体实现（CronScheduler / McpClientPool / ToolSearchIndex /
/// AgentCatalogProvider / PluginManager / SettingsHooksLoader /
/// WorkflowAgentMiddlewareFactory / 插件聚合数据）全部由本装配面内部构造
/// ——「ACP Host = 部署单元」，TUI/print/stdio 只提供协议面输入
/// （provider / config / permission / session_resources / cwd），不再直接触碰
/// 业务 crate（§0 依赖方向，`docs/top-level.md` §7/§8）。
#[derive(Clone)]
pub(crate) struct WorkspaceAssembly {
    pub(crate) startup_cwd: String,
    pub(crate) bare: bool,
    pub(crate) drive_cron_tick: bool,
    pub(crate) mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile,
}

/// 准备阶段的严格只读插件发现（无合成清单、无插件缓存写）。
///
/// 会话准备面（`host/prepared.rs`）经本函数调用：具体实现与插件装配同属宿主
/// 装配面，准备面不新建越层引用（§0 依赖门边 2）。
///
/// 用户级 `.claude` 在本装配面解析：`peri_middlewares::plugin::claude_home()`
/// 是 HOME 优先的唯一权威（Windows 的 `dirs_next::home_dir()` 读 Profile
/// known-folder、忽略 HOME/USERPROFILE，自带一份解析会与其余插件入口取到
/// 不同目录）。准备面因此只提供执行目录。
pub(crate) fn discover_enabled_plugins_readonly(
    cwd: &str,
) -> Result<PluginLoadResult, peri_middlewares::plugin::LoaderError> {
    peri_middlewares::plugin::load_enabled_plugins_aggregated_readonly(
        &peri_middlewares::plugin::claude_home(),
        Some(std::path::Path::new(cwd)),
    )
}

/// Bind session execution identity before static discovery or dynamic load can use the pool.
/// A host-only pool stays unbound; its dynamic connector must reject implicit execution.
fn pending_mcp_pool(
    spawner: peri_middlewares::mcp::McpTaskSpawner,
    profile: peri_middlewares::mcp::apps::McpCapabilityProfile,
    session_cwd: Option<&std::path::Path>,
    resources: &Arc<dyn SessionResources>,
) -> Arc<peri_middlewares::mcp::McpClientPool> {
    let pool = Arc::new(
        peri_middlewares::mcp::McpClientPool::new_pending_with_spawner_and_profile(
            spawner, profile,
        ),
    );
    if let Some(credentials) = resources.oauth_credentials() {
        match peri_mcp_credentials::OAuthCredentialClient::new(credentials)
            .and_then(|client| pool.inject_oauth_credentials(client))
        {
            Ok(()) => {}
            Err(_) => tracing::error!("OAuth credential MCP initialization failed"),
        }
    }
    if let Some(cwd) = session_cwd {
        if let Err(error) = pool.bind_execution_cwd(cwd) {
            // The pool records initialization failure and remains unusable for
            // implicit subprocess launches. Never fall back to the host's cwd.
            tracing::error!(%error, cwd = %cwd.display(), "MCP session execution directory binding failed");
        }
    }
    pool
}

/// 准备路径一次加载的插件数据：聚合本身 + 由该聚合派生的 roots。
///
/// 装配面消费同一对象，不重读插件目录；`data` 为 `None` 表示 host 级/bare。
#[derive(Clone)]
pub struct PreparedPlugins {
    pub data: Option<PluginLoadResult>,
    pub skill_roots: Vec<SkillRoot>,
}

pub struct HostAssemblyInput {
    pub provider: LlmProvider,
    pub peri_config: Arc<RwLock<PeriConfig>>,
    /// 配置源（读写路径决策的唯一事实源：TUI/print/stdio 共享，启动早期
    /// 经 [`crate::provider::ConfigSource::load`] 构建一次）。
    pub config_source: Arc<crate::provider::ConfigSource>,
    pub permission_mode: Arc<SharedPermissionMode>,
    /// 会话资源门面（消费侧唯一会话行为句柄）：Agent transcript/subagent、middleware、
    /// 协议面与 Controller 都经它访问会话，装配面不再另开裸存储句柄。
    pub session_resources: Arc<dyn SessionResources>,
    /// 部署关闭权（non-Clone）：由部署入口（TUI/print/stdio）从资源工厂取得后注入，
    /// 宿主在**自己的任务排空之后**消费它关闭会话存储。会话级装配与测试注入 `None`
    /// ——它们不是部署 owner，没有全局销毁权。
    pub session_store_shutdown: Option<Box<dyn SessionStoreShutdownPort>>,
    /// 工作目录（用于加载 project/local settings hooks）
    pub cwd: String,
    /// 跳过 settings hooks / LSP / 插件（print --bare 语义）
    pub bare: bool,
    /// 驱动 cron tick（TUI=true，复刻迁移前 TUI 每秒 tick 行为；print/stdio
    /// 保持现状无 tick——行为零变化，L2 遗留登记 M-TUI issue）。
    pub drive_cron_tick: bool,
    /// 遗留测试输入槽；生产传 `None`，Workspace Bash 在 MCP 侧自持任务。
    pub workspace_input: Option<peri_mcp_workspace::WorkspaceInstanceInput>,
    /// builtin `workspace` 实例的**资源面**输入（资源根 / builtin 关闭位 / 预算），
    /// 随 builtin 实例上下文一次注入 pool，早于
    /// `McpClientPool::run_initialize`（A33）。
    ///
    /// `None` = 资源面未接线（`resources/list` 只有 git ref）：顶层三路径保持 `None`
    /// ——它们不产生会话，资源面没有消费者；会话环境装配传 `Some`，其内容由装配期
    /// 已有事实源构造（本层不第二次读配置）。
    pub workspace_resources: Option<peri_mcp_workspace::WorkspaceResourcesInput>,
    /// 准备路径一次加载的插件聚合：`Some` 时装配面不再重读插件目录
    /// （`None` = 既有语义，由装配面自行加载；仅 host 级/非准备调用点如此）。
    pub prepared_plugins: Option<PreparedPlugins>,
    /// ACP session setup MCP servers, bound before the session pool starts initialization.
    pub session_mcp_servers:
        Option<std::collections::HashMap<String, peri_acp_types::plugin::McpServerConfig>>,
    /// A24 关闭集（builtin 实例名）：本会话环境**冻结** policy 的投影
    /// （`frozen.meta_harness.disabled_middlewares` → `builtin_closed_instances`），
    /// 随 builtin 实例上下文一次注入 pool。
    ///
    /// 消费面只有订阅建立门（`McpClientPool::subscription_allowed`）：关闭的 builtin
    /// 实例不建立 `subscriptions/listen` 长流（订阅是能力的外部副作用，关闭语义必须
    /// 覆盖它，ARC-CAPABILITY-CLOSURE-001）。它**不**改变 pool 级就绪判定与实例在 MCP
    /// 面板上的连接状态（契约明文）。顶层三路径（`session_resources = false`，无会话
    /// 上下文）传空集；会话装配从 frozen snapshot 派生（禁止回退当轮 config，设计 §2.5）。
    pub builtin_closed: std::collections::BTreeSet<String>,
    /// 宿主技能面关闭位：`"SkillsMiddleware" ∈ disabled_middlewares`（与
    /// [`Self::builtin_closed`] **同一份** disabled 集合的投影，装配期一次派生）。
    ///
    /// 语义 = 关闭宿主技能面（`core:{skill}` 裸名命令投影）：链槽关闭
    /// （`SkillsMiddleware` 不构造 ⇒ 13_skills 段落 + SkillTool/DiscoverSkillsTool
    /// 消失）时命令面不得留下幽灵路由。**不是**实例关闭——workspace 实例、7 个
    /// 工具与 `{server}:{skill}` MCP 发现面均不受影响（两个位的派生事实源相同，
    /// 判据不同）。随 builtin 实例上下文注入 pool（发现管线的唯一消费点）。
    /// 顶层三路径（无会话上下文）恒为 `false`；会话装配从 frozen snapshot 派生。
    pub skills_face_closed: bool,
}

/// Construct terminal hook execution; the session environment owns admission and joining.
pub(super) fn build_session_end_task(
    hooks: Vec<RegisteredHook>,
    cwd: String,
    session_id: String,
    model: String,
    tasks: Arc<dyn peri_acp_types::tasks::TaskManager>,
) -> peri_acp_types::tasks::OwnedTaskFuture {
    Box::pin(async move {
        peri_middlewares::hooks::fire_standalone_lifecycle_hooks_owned(
            &hooks,
            peri_acp_types::hooks::HookEvent::SessionEnd,
            &cwd,
            &session_id,
            "",
            &model,
            None,
            Some("session_close"),
            Some(tasks),
        )
        .await;
    })
}

/// 组装 settings hook 组（plugin → global → project → local，顺序即迁移前
/// TUI/print/stdio 三处一致的既有顺序，ARC-MIDDLEWARE-001 不重排）。
///
/// `skip_settings_hooks`：bare 模式跳过 global/project/local（与 print 既有语义
/// 一致）；plugin hooks 为空时不产生空组。三级 settings hooks 经
/// [`SettingsHooksPort`] 注入（装配点构造，磁盘加载留在实现方）。
pub fn assemble_hook_groups(
    plugin_hooks: &[RegisteredHook],
    settings_hooks: &dyn SettingsHooksPort,
    cwd: &str,
    skip_settings_hooks: bool,
) -> Vec<Vec<RegisteredHook>> {
    let mut hook_groups: Vec<Vec<RegisteredHook>> = Vec::new();
    if !plugin_hooks.is_empty() {
        hook_groups.push(plugin_hooks.to_vec());
    }
    if skip_settings_hooks {
        return hook_groups;
    }
    let global_hooks = settings_hooks.global();
    if !global_hooks.is_empty() {
        hook_groups.push(global_hooks);
    }
    let project_hooks = settings_hooks.project(cwd);
    if !project_hooks.is_empty() {
        hook_groups.push(project_hooks);
    }
    let local_hooks = settings_hooks.local(cwd);
    if !local_hooks.is_empty() {
        hook_groups.push(local_hooks);
    }
    hook_groups
}

/// 构造共享 SessionManager（支撑 cascade cancel 子 agent 与 goal_state）。
///
/// 装配细节与迁移前 `launch.rs` / `cli_print.rs` / stdio init 三处一致：
/// peri_config 冻结快照 + cron scheduler（可选）注入。
#[allow(clippy::too_many_arguments)] // 装配注入面：端口/工厂逐项注入，L5 装配迁出后可分组
pub fn build_session_manager(
    session_resources: Arc<dyn SessionResources>,
    provider: LlmProvider,
    peri_config: &Arc<RwLock<PeriConfig>>,
    permission_mode: Arc<SharedPermissionMode>,
    cron_scheduler: Option<Arc<dyn CronSchedulerPort>>,
    mcp_subscription: Option<Arc<dyn McpSubscriptionPort>>,
    dynamic_mcp: Option<Arc<dyn peri_acp_types::ports::DynamicMcpDeploymentPort>>,
    agent_catalog: Arc<dyn AgentCatalogPort>,
    plugin_command_entries: Vec<RouteEntry>,
) -> SessionManager {
    let peri_config_snapshot = Arc::new(peri_config.read().clone());
    SessionManager::new(
        session_resources,
        provider,
        peri_config_snapshot,
        permission_mode,
        None,
        cron_scheduler,
        mcp_subscription,
        dynamic_mcp,
        // 装配注入面：Agent 管理 session registry，工具环境提供本地 shell 执行。
        // ACP 协议面只持有契约 `peri_acp_types::tasks::TaskManager`。
        Some(Arc::new(|| {
            Arc::new(peri_mcp_common::create_local_task_manager())
                as Arc<dyn peri_acp_types::tasks::TaskManager>
        })),
        agent_catalog,
        plugin_command_entries,
    )
}

/// 组装完整的 ACP host 配置（TUI / print 路径入口）。
///
/// 自迁移前 `launch.rs` 的内嵌 server 装配原样搬移：hook 组加载、tool search
/// index、shared tools、Langfuse（环境启用时创建）、SessionManager。
///
/// M-TUI 收口：middlewares 具体实现（cron / MCP 池 / 工具检索索引 / skills /
/// plugin / settings hooks / workflow 装配端口）与插件聚合数据在本装配面
/// 内部构造（`peri-middlewares` 引用豁免见 `scripts/import-exemptions.conf`
/// 边 2 assemble 路径）；行为与迁移前三路径（launch / cli_print / stdio）
/// 各自装配一致（cron tick 驱动、MCP 初始化、孤儿插件清理时机均复刻）。
pub async fn assemble_server_config(input: HostAssemblyInput) -> AcpServerConfig {
    assemble_server_config_with_mcp_profile(
        input,
        peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
        false,
        None,
    )
    .await
}

/// stdio deployment variant. The assembly boundary derives the concrete MCP profile;
/// TUI/MPSC always use [`assemble_server_config`].
pub async fn assemble_server_config_with_mcp_apps(
    input: HostAssemblyInput,
    apps_enabled: bool,
) -> AcpServerConfig {
    assemble_server_config_with_mcp_profile(
        input,
        peri_middlewares::mcp::apps::deployment_profile(apps_enabled),
        false,
        None,
    )
    .await
}

pub(crate) async fn assemble_server_config_with_mcp_profile(
    input: HostAssemblyInput,
    mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile,
    // `true` = 会话级装配（带 workspace 装配与 per-session MCP 池）；`false` = host 级
    // 装配。与会话资源门面字段 `session_resources` 不同义，故另名 `session_scoped`。
    session_scoped: bool,
    activation: Option<tokio_util::sync::CancellationToken>,
) -> AcpServerConfig {
    let (host_task_owner, host_task_spawner) = HostTaskOwner::new();
    let (mcp_task_owner, mcp_task_spawner) = peri_middlewares::mcp::McpTaskOwner::new();
    let HostAssemblyInput {
        provider,
        peri_config,
        config_source,
        permission_mode,
        session_resources,
        session_store_shutdown,
        cwd,
        bare,
        drive_cron_tick,
        workspace_input,
        workspace_resources,
        prepared_plugins,
        session_mcp_servers,
        builtin_closed,
        skills_face_closed,
    } = input;

    // 用户级 `.claude` 与准备面、插件 RPC 共用同一权威（HOME 优先）：
    // 见 `peri_middlewares::plugin::claude_home`。
    let claude_dir = peri_middlewares::plugin::claude_home();

    // ── 插件聚合数据（bare 时跳过；准备路径消费同一聚合，不重读插件目录）──
    let (prepared_data, prepared_skill_roots) = match prepared_plugins {
        Some(prepared) => (Some(prepared.data), Some(prepared.skill_roots)),
        None => (None, None),
    };
    let prepared_supplied = prepared_skill_roots.is_some();
    let plugin_data: Option<PluginLoadResult> = if bare || !session_scoped {
        None
    } else if prepared_supplied {
        // 准备路径已严格只读加载一次：装配面消费同一聚合，不重读插件目录。
        prepared_data.flatten()
    } else {
        Some(peri_middlewares::plugin::load_enabled_plugins_aggregated(
            &claude_dir,
            Some(std::path::Path::new(&cwd)),
        ))
    };

    // ── cron 调度器（迁移前 TUI launch / cli_print 各自构造）──
    //
    // A1/A32：每个会话环境构造一份 scheduler，并以同一 `Arc` 同时喂给
    // `CronSchedulerPort`（宿主事件面）与 builtin `cron` 实例的
    // `CronInstanceInput`（工具面）。1s tick 的唯一 spawn 点是 pool 建立本代
    // builtin transport 处（`McpClientPool::spawn_builtin_transport`，由
    // `cron.tick_enabled` 决定）；宿主不再 spawn `HostTaskKind::CronTick`
    // ——同一 scheduler 任一时刻至多一个驱动，tick 随该代 supervisor 关闭。
    // 部署层不建 MCP 池；其 tick 策略经 WorkspaceAssembly 原样传入会话。
    let cron_scheduler_concrete = Arc::new(parking_lot::Mutex::new(
        peri_mcp_cron::CronScheduler::new(tokio::sync::mpsc::unbounded_channel().0),
    ));
    let cron_scheduler: Option<Arc<dyn CronSchedulerPort>> = Some(Arc::new(
        peri_mcp_cron::CronSchedulerPortHandle(Arc::clone(&cron_scheduler_concrete)),
    ));

    // ── LSP：配置合并 + host 级唯一 pool（A11/A21/A22，顺序冻结见 sub-plan H §5.1）──
    //
    // 构造归属（2026-09-29 裁决）：配置合并与 pool 工厂归 `peri_mcp_lsp`，本层只调用
    // `peri_mcp_lsp::{load_merged_lsp_servers, create_host_lsp_pool}` 并注入同一 `Arc`；
    // A11/A21/A22 的单 pool / 空配置 / root_uri 行为约束不变。
    //
    // ① 配置合并必须**早于** builtin `lsp` handler 构造：handler 在
    //    `run_initialize` 内按生效配置非空（`has_servers()`）快照工具面，
    //    不支持热更新（A21）；
    // ② host pool 必须**早于** MCP 池的 `run_initialize` 构造，才能随
    //    `BuiltinInstanceContext` 一次注入（A33）；
    // ③ 空配置也构造 pool（`has_servers()` 假 ⇒ 工具面空表但仍 ready，A6）——
    //    不得用「不构造」表达「无配置」。
    //
    // H5：全局 settings.json（config.lspServers）与插件 LSP 服务器合并
    //（优先级对齐 MCP：global < plugin；无插件时全局配置单独生效）。
    // 读取路径跟随宿主全局配置加载机制（config_path，支持测试重定向）。
    let plugin_lsp_servers = if bare {
        Default::default()
    } else {
        peri_mcp_lsp::load_merged_lsp_servers(
            &crate::provider::config_path(),
            plugin_data
                .as_ref()
                .map(|pd| pd.all_lsp_servers.clone())
                .unwrap_or_default(),
        )
    };
    let host_lsp_pool_concrete = peri_mcp_lsp::create_host_lsp_pool(&cwd, &plugin_lsp_servers);
    // 宿主侧投影：端口即消费面（A23/A30），链上同步中间件与 host shutdown 都只经它。
    let lsp_pool: Arc<dyn LspPoolPort> =
        Arc::clone(&host_lsp_pool_concrete) as Arc<dyn LspPoolPort>;

    // ── 会话 MCP 池（bare 仅装配 workspace；后台初始化不阻塞）──
    // OAuth 授权事件通道：MCP 授权回调（AuthorizationNeeded/Completed/Failed）
    // 经 tx 转发 AcpEvent，run_acp_server 侧消费者以 peri/agent_event 送达 TUI。
    let (oauth_event_tx, oauth_event_rx) =
        tokio::sync::mpsc::unbounded_channel::<crate::event::oauth::HostOAuthEvent>();
    let mcp_pool_concrete: Option<Arc<peri_middlewares::mcp::McpClientPool>> = if !session_scoped {
        None
    } else {
        let pool = pending_mcp_pool(
            mcp_task_spawner.clone(),
            mcp_profile.clone(),
            Some(std::path::Path::new(&cwd)),
            &session_resources,
        );
        if let Some(snapshot) = config_source.snapshot() {
            if let Err(error) = pool.set_configuration_snapshot(snapshot) {
                tracing::error!(error = %error, "MCP configuration snapshot binding failed");
            }
        }
        if let Some(servers) = session_mcp_servers {
            if let Err(error) = pool.set_session_servers(servers) {
                tracing::error!(error = %error, "ACP session MCP server binding failed");
            }
        }
        // ── A33：builtin 实例上下文由**宿主装配**构造并注入，必须早于下面的
        //    `run_initialize` 及其后台 spawn ──
        //
        // 同一批 `Arc`（A1）：cron 用组合根唯一 scheduler，lsp 用上面那份 host
        // pool（同一 `Arc` 同时喂端口投影）。`closed` 的**来源**分两处：
        // `HostAssemblyInput::builtin_closed`（会话装配从 frozen 派生）随本上下文注入
        // pool，只被订阅建立门消费；链装配（`McpMiddleware` / `open_builtin_bridges`）
        // 仍从同一 frozen policy 派生本 turn 的工具投影关闭集。
        //
        // `tick_enabled` 是 `HostAssemblyInput::drive_cron_tick` 的投影（TUI=true；
        // print/stdio=false；会话继承所属部署的开关）。
        //
        // `workspace` 输入是**唯一 session 级**的一项（AW3-11：per-session
        // `TaskManager` + session 级 `on_bg_complete`），由会话环境装配原样转交，
        // 本层不包装、不派生；`None` = 可见但退化（handler 照常构造，只是 `Bash`
        // 失去后台任务那一路），不是「实例不可装配」——无 `instance_input_ready` arm。
        let mut builtin_context =
            peri_middlewares::assembly::BuiltinInstanceContext::new(cwd.clone())
                .with_cron(peri_middlewares::assembly::CronInstanceInput {
                    scheduler: Arc::clone(&cron_scheduler_concrete),
                    tick_enabled: drive_cron_tick,
                })
                .with_lsp(peri_middlewares::assembly::LspInstanceInput {
                    pool: Arc::clone(&host_lsp_pool_concrete),
                });
        if let Some(workspace_input) = workspace_input {
            builtin_context = builtin_context.with_workspace(workspace_input);
        }
        // 资源面输入与 session 级输入同批（同一上下文、同一次注入）：`None` 保持
        // 「资源面未接线」的既有行为。
        if let Some(workspace_resources) = workspace_resources {
            builtin_context = builtin_context.with_workspace_resources(workspace_resources);
        }
        let builtin_context = builtin_context
            .with_closed(builtin_closed)
            // W4b 收口：宿主技能面关闭位与关闭集同源（同一份 disabled 集合），
            // 由发现管线的 core 投影消费（`core:{skill}` 裸名命令撤下）。
            .with_skills_face_closed(skills_face_closed);
        let builtin_context = Arc::new(builtin_context);
        if let Err(error) = pool.set_builtin_instance_context(builtin_context) {
            // 本池是上一行刚构造的（从未 initialize）⇒ 两个 typed 拒绝都不可能出现：
            // 出现即装配顺序 bug。不在此处伪造 ready —— 缺上下文的 builtin 实例会在
            // `run_initialize` 里以 typed 原因走 `insert_failed` +
            // `commit_discovery_failure` 收口，`system_mcp` 闸门 fatal（A33）。
            tracing::error!(%error, "builtin 实例上下文注入失败");
        }
        let pool_clone = pool.clone();
        let cwd_clone = cwd.clone();
        let claude_home_clone = claude_dir.clone();
        let (init_tx, _init_rx) =
            tokio::sync::watch::channel(peri_middlewares::mcp::McpInitStatus::Pending);
        // OAuth 事件回调：AuthorizationNeeded 时注册回传通道（TUI 经
        // mcp/oauth_callback RPC 投递授权码）并转发 OauthNeeded；完成/失败
        // 直接转发对应 AcpEvent。L5 装配面豁免全路径引用（import-exemptions
        // ACP-biz-fullpath），不引入 use 语句。
        type OAuthFlowEvent = peri_middlewares::mcp::oauth_flow::OAuthFlowEvent;
        let oauth_event_callback: Option<
            Box<dyn Fn(peri_middlewares::mcp::oauth_flow::OAuthFlowEvent) + Send + Sync>,
        > = {
            let cb_tx = oauth_event_tx.clone();
            let cb_pool = Arc::downgrade(&pool);
            Some(Box::new(move |event: OAuthFlowEvent| match event {
                OAuthFlowEvent::DynamicAuthorizationNeeded {
                    instance,
                    flow_id,
                    server_name,
                    authorization_url,
                    callback_tx,
                } => {
                    let Some(cb_pool) = cb_pool.upgrade() else {
                        return;
                    };
                    if !cb_pool.register_dynamic_oauth_callback(
                        instance.clone(),
                        &flow_id,
                        callback_tx,
                    ) {
                        return;
                    }
                    let _ = cb_tx.send(
                        crate::event::oauth::HostOAuthEvent::DynamicAuthorizationNeeded {
                            instance,
                            flow_id,
                            server_name,
                            authorization_url,
                        },
                    );
                }
                OAuthFlowEvent::AuthorizationNeeded {
                    flow_id,
                    server_name,
                    authorization_url,
                    callback_tx,
                } => {
                    let Some(cb_pool) = cb_pool.upgrade() else {
                        return;
                    };
                    if !cb_pool.register_oauth_callback(&server_name, &flow_id, callback_tx) {
                        return;
                    }
                    let _ = cb_tx.send(crate::event::oauth::HostOAuthEvent::AuthorizationNeeded {
                        flow_id,
                        server_name,
                        authorization_url,
                    });
                }
                OAuthFlowEvent::AuthorizationCompleted {
                    flow_id,
                    server_name,
                } => {
                    let _ = cb_tx.send(crate::event::oauth::HostOAuthEvent::Completed {
                        flow_id,
                        server_name,
                    });
                }
                OAuthFlowEvent::AuthorizationFailed {
                    flow_id,
                    server_name,
                    failure_kind,
                    error,
                } => {
                    let failure_class = match failure_kind {
                        peri_middlewares::mcp::oauth_flow::OAuthFailureKind::CallbackUnavailable => crate::event::oauth::OAuthFailureClass::CallbackUnavailable,
                        peri_middlewares::mcp::oauth_flow::OAuthFailureKind::CallbackTimeout => crate::event::oauth::OAuthFailureClass::CallbackTimeout,
                        peri_middlewares::mcp::oauth_flow::OAuthFailureKind::ProviderRejected => crate::event::oauth::OAuthFailureClass::ProviderRejected,
                        peri_middlewares::mcp::oauth_flow::OAuthFailureKind::ConnectionFailed => crate::event::oauth::OAuthFailureClass::ConnectionFailed,
                        peri_middlewares::mcp::oauth_flow::OAuthFailureKind::Internal => crate::event::oauth::OAuthFailureClass::Internal,
                    };
                    let _ = cb_tx.send(crate::event::oauth::HostOAuthEvent::Failed {
                        flow_id,
                        server_name,
                        failure_class,
                        legacy_error: error,
                    });
                }
                OAuthFlowEvent::AuthorizationCancelled {
                    flow_id,
                    server_name,
                } => {
                    let _ = cb_tx.send(crate::event::oauth::HostOAuthEvent::Cancelled {
                        flow_id,
                        server_name,
                    });
                }
                OAuthFlowEvent::AuthorizationRestored {
                    flow_id,
                    server_name,
                } => {
                    let _ = cb_tx.send(crate::event::oauth::HostOAuthEvent::Restored {
                        flow_id,
                        server_name,
                    });
                }
            }))
        };
        let _ = pool.spawn_background(peri_middlewares::mcp::McpTaskKey::Initialize, async move {
            if let Some(activation) = activation {
                activation.cancelled().await;
            }
            if bare {
                peri_middlewares::mcp::McpClientPool::run_initialize_bare(
                    pool_clone,
                    std::path::Path::new(&cwd_clone),
                    init_tx,
                )
                .await;
            } else {
                peri_middlewares::mcp::McpClientPool::run_initialize(
                    pool_clone,
                    std::path::Path::new(&cwd_clone),
                    &claude_home_clone,
                    init_tx,
                    oauth_event_callback,
                )
                .await;
            }
        });
        Some(pool)
    };
    let dynamic_mcp_concrete = peri_middlewares::mcp::dynamic::DynamicMcpRegistry::new(
        mcp_task_spawner.clone(),
        Arc::new(
            peri_middlewares::mcp::dynamic::ProductionDynamicMcpConnector::from_environment(
                mcp_task_spawner.clone(),
                mcp_pool_concrete.clone().unwrap_or_else(|| {
                    pending_mcp_pool(
                        mcp_task_spawner.clone(),
                        mcp_profile.clone(),
                        session_scoped.then_some(std::path::Path::new(&cwd)),
                        &session_resources,
                    )
                }),
            ),
        ),
    );
    let dynamic_mcp: Arc<dyn peri_acp_types::ports::DynamicMcpDeploymentPort> =
        dynamic_mcp_concrete;
    // 订阅端口同源复用：同一 McpClientPool 同时承担 McpPoolPort（命令面）与
    // McpSubscriptionPort（订阅通知 → 会话 inbox 唤醒）两个角色。
    let mcp_pool: Option<Arc<dyn McpPoolPort>> =
        mcp_pool_concrete.clone().map(|p| p as Arc<dyn McpPoolPort>);
    let mcp_subscription: Option<Arc<dyn McpSubscriptionPort>> = mcp_pool_concrete
        .clone()
        .map(|p| p as Arc<dyn McpSubscriptionPort>);
    // MCP over ACP 服务：会话 setup 声明的 `type: "acp"` server 经它建连，连接
    // 进的就是**本装配的池**——会话级装配（`session_scoped`）才有池，连接
    // 因此只能进声明它的会话的工具面。host 级装配无池即无此服务。
    let acp_mcp: Option<Arc<dyn peri_acp_types::ports::AcpMcpServerPort>> =
        mcp_pool_concrete.clone().filter(|_| !bare).map(|pool| {
            Arc::new(peri_middlewares::mcp::AcpMcpService::new(pool))
                as Arc<dyn peri_acp_types::ports::AcpMcpServerPort>
        });
    let mcp_apps_relay: Option<Arc<dyn peri_acp_types::mcp_apps::McpAppsRelayPort>> =
        if mcp_profile.apps_enabled() {
            mcp_pool_concrete.clone().map(|pool| {
                Arc::new(peri_middlewares::mcp::apps_relay::PoolMcpAppsRelay::new(
                    pool,
                )) as Arc<dyn peri_acp_types::mcp_apps::McpAppsRelayPort>
            })
        } else {
            None
        };

    // ── 资源类/业务面端口默认实现（构造下沉：ACP Host = 部署单元）──
    let tool_search_index: Arc<dyn ToolSearchPort> =
        Arc::new(peri_middlewares::tool_search::ToolSearchIndex::new());
    let agent_catalog: Arc<dyn AgentCatalogPort> =
        Arc::new(peri_middlewares::host_ports::AgentCatalogProvider::new());
    let plugin_manager: Arc<dyn PluginManagerPort> =
        Arc::new(peri_middlewares::host_ports::PluginManager);
    let settings_hooks: Arc<dyn SettingsHooksPort> =
        Arc::new(peri_middlewares::host_ports::SettingsHooksLoader);
    // A6 面③：workflow agent 的工具面必须同样保留 Web / Artifact 能力（迁移后为
    // builtin 实例的 direct bridge `mcp__web__*` 等），因此装配点把 deployment pool
    // 交给工厂；bare 池同样保留 workspace，无 pool 时为 None。
    let workflow_middleware_factory =
        peri_middlewares::assembly::default_workflow_middleware_factory_with_pool(
            mcp_pool_concrete.clone(),
        );

    // E2：启动时清理孤儿插件文件（迁移前 TUI launch 行为；bare 时跳过）
    if !bare && !session_scoped {
        let claude_dir_clone = claude_dir.clone();
        let _ = host_task_spawner.spawn(
            HostTaskOwnerKind::Startup,
            HostTaskKind::PluginCleanup,
            async move {
                if let Err(e) =
                    peri_middlewares::plugin::cleanup_orphaned_plugins(&claude_dir_clone).await
                {
                    tracing::warn!(target: "peri", error = %e, "启动时清理孤儿插件文件失败");
                } else {
                    tracing::info!(target: "peri", "启动时清理孤儿插件文件完成");
                }
            },
        );
    }

    // W4b（F6/J5）：`prepared_skill_roots` 不再经 session 管理器注入命令面
    // （技能命令改由 MCP 发现投影）；它仍是**技能资源根**的事实源，由会话环境
    // 装配（`SessionEnvironment::assemble_with_frozen` 的 provider 输入）与
    // `AcpServerConfig.plugin_skill_roots` 消费。
    let plugin_skill_roots = prepared_skill_roots.unwrap_or_else(|| {
        plugin_data
            .as_ref()
            .map(|pd| pd.all_skill_roots.clone())
            .unwrap_or_default()
    });
    // Phase 6 B2：插件命令静态条目预转（全路径引用豁免见
    // scripts/import-exemptions.conf 边 2 assemble 路径；bare 时为空）。
    let plugin_command_entries = plugin_data
        .as_ref()
        .map(|pd| peri_middlewares::plugin::plugin_route_entries(&pd.all_commands))
        .unwrap_or_default();
    // `plugin_lsp_servers` / host pool 已在 MCP 池初始化之前构造（见上方
    // 「LSP：配置合并 + host 级唯一 pool」块）：配置合并必须在 builtin `lsp`
    // handler 构造之前完成（A21）。
    let plugin_hooks = plugin_data
        .as_ref()
        .map(|pd| pd.all_hooks.clone())
        .unwrap_or_default();
    let plugin_loaded = plugin_data
        .as_ref()
        .map(|pd| pd.plugins.clone())
        .unwrap_or_default();

    let hook_groups = assemble_hook_groups(
        &plugin_hooks,
        settings_hooks.as_ref(),
        &cwd,
        bare || !session_scoped,
    );
    let flat_hooks: Vec<RegisteredHook> = hook_groups.iter().flatten().cloned().collect();
    tracing::info!(
        groups = hook_groups.len(),
        total_hooks = flat_hooks.len(),
        "Hook groups assembled for ACP host"
    );

    let shared_tools = Arc::new(parking_lot::RwLock::new(std::collections::BTreeMap::new()));

    let session_manager = build_session_manager(
        session_resources.clone(),
        provider.clone(),
        &peri_config,
        permission_mode.clone(),
        cron_scheduler.clone(),
        mcp_subscription,
        Some(Arc::clone(&dynamic_mcp)),
        agent_catalog.clone(),
        // Phase 6 B2：插件静态命令条目注入 session 管理器（会话创建时注册；
        // 技能命令面已改由 MCP 发现异步投影，不再经此处）。
        plugin_command_entries.clone(),
    );

    // Langfuse 观测（与迁移前 TUI/stdio/print 一致：环境启用时创建）
    let langfuse_config = if session_scoped {
        None
    } else {
        config_source
            .snapshot()
            .map(|snapshot| snapshot.observability().clone())
            .filter(|config| config.public_key.is_some() && config.secret_key.is_some())
    };
    let (langfuse_session, langfuse_shutdown_owner) = if let Some(config) = langfuse_config {
        tracing::info!("Langfuse tracing enabled (host mode)");
        match peri_controller::langfuse::LangfuseSession::new_owned(config, "live".into()).await {
            Some((session, owner)) => (Some(session), Some(owner)),
            None => (None, None),
        }
    } else {
        (None, None)
    };

    // 指标出口：仅 Langfuse 可用时安装（Langfuse event）；未配置时保持未安装，
    // 指标事件丢弃——不落盘。
    if let Some(session) = &langfuse_session {
        let metrics_session: Arc<dyn peri_controller::langfuse::LangfuseSessionLike> =
            session.clone();
        peri_agent::metrics::set_sink(Some(Arc::new(
            peri_controller::langfuse::LangfuseMetricsSink::new(metrics_session),
        )));
    }

    AcpServerConfig {
        workspace_assembly: (!session_scoped).then(|| WorkspaceAssembly {
            startup_cwd: cwd.clone(),
            bare,
            drive_cron_tick,
            mcp_profile,
        }),
        host_task_owner: Some(host_task_owner),
        host_task_spawner,
        mcp_task_owner: Some(Box::new(mcp_task_owner)),
        provider: Arc::new(RwLock::new(provider)),
        peri_config,
        permission_mode,
        cron_scheduler,
        mcp_pool,
        mcp_apps_relay,
        acp_mcp,
        dynamic_mcp: Some(dynamic_mcp),
        oauth_event_tx: Some(oauth_event_tx),
        oauth_event_rx: Some(oauth_event_rx),
        plugin_skill_roots,
        plugin_command_entries,
        plugin_hooks: flat_hooks,
        // 仅插件 hooks（hooks 面板数据源；plugin/list 命令面返回，TUI 不再
        // 直读 plugin_data）
        plugin_hooks_only: plugin_hooks,
        plugin_loaded,
        hook_groups,
        plugin_lsp_servers,
        lsp_pool: Some(lsp_pool),
        tool_search_index,
        agent_catalog,
        plugin_manager,
        settings_hooks,
        shared_tools,
        workflow_middleware_factory,
        session_resources: session_resources.clone(),
        session_store_shutdown,
        controller: Arc::new(peri_controller::Controller::new(session_resources.clone())),
        langfuse_session,
        langfuse_shutdown_owner,
        // 默认 false（TUI/print 保留全部命令）；stdio 装配点（assemble_stdio_config）
        // 显式置 true，过滤 rewind/clear。
        stdio_command_filter: false,
        config_source,
        session_manager,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn worktree_mcp_pool_is_bound_before_deferred_initialization() {
        let target = tempfile::TempDir::new().unwrap();
        let sibling = tempfile::TempDir::new().unwrap();
        let (resources, shutdown) =
            peri_resources::Resources::open_with(Some(target.path().join("configured.db")))
                .await
                .unwrap()
                .into_parts();
        let (_owner, spawner) = peri_middlewares::mcp::McpTaskOwner::new();
        let pool = pending_mcp_pool(
            spawner,
            peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
            Some(target.path()),
            &resources,
        );
        assert_eq!(pool.snapshot()["initPhase"], "pending");
        assert!(pool.bind_execution_cwd(sibling.path()).is_err());
        assert_eq!(
            pool.bind_execution_cwd(target.path()).unwrap(),
            target.path()
        );
        peri_acp_types::session_resources::SessionStoreShutdownPort::shutdown(&shutdown)
            .await
            .unwrap();
    }
}
