//! A session owns one verified execution environment and its resource lifetime.

use std::sync::Arc;

use super::{assemble, task_scope, AcpServerConfig};
use crate::transport::types::AcpError;
use peri_acp_types::ports::McpBuiltinWorkspaceState;
use peri_acp_types::session_resources::{BindingRecheck, SessionResourceError};
use peri_acp_types::thread::ThreadId;
use peri_acp_types::workspace::ResolvedWorkspace;

enum SessionEndState {
    Pending,
    Running(tokio::task::JoinHandle<()>),
    Finished,
    Skipped,
    Failed,
}

pub(crate) struct SessionEnvironment {
    pub(crate) cfg: AcpServerConfig,
    activation: tokio_util::sync::CancellationToken,
    /// session 生命周期令牌：canonical idle 执行（App 回调 / Host invoke）的取消父。
    ///
    /// 与 [`Self::activation`] **不是同一个事实**：`activation` 是池初始化哨兵
    /// （语义「会话已装配」，创建期 `activate()` 即取消，池初始化等它），本令牌只在
    /// [`Self::shutdown`] 取消。二者混用会让 idle 执行的子令牌自出生即取消——
    /// `CancellationToken::child_token()` 对已取消父返回**立即取消**的子节点，而
    /// 审批 hook 走 `biased` select（取消优先于 hook 结果）⇒ 审批永不发出、
    /// outcome 落 `OutcomeUnknown`，App 回调与 Host invoke 在生产装配下全不可用。
    session_lifetime: tokio_util::sync::CancellationToken,
    /// session-owned 交互门：本环境的 router 借用它，`SessionState.interaction_gate`
    /// 也借用同一 `Arc`（一个 session 恰好一把门，跨轮共享）。
    interaction_gate: Arc<tokio::sync::Mutex<()>>,
    /// session-owned 交互路由（按可信归属选审批 endpoint；投递失败原因的诚实产出点：
    /// `NoOnlineEndpoint` / `OwnerUnmatched` / `AttachmentDetached`，见 `interaction_router.rs`）。
    ///
    /// canonical 调用入口由本环境的 relay（`cfg.mcp_apps_relay`）持有，本结构不再
    /// 另存句柄（无第二消费者即不新增访问面）。
    interaction_router: Arc<super::interaction_router::SessionInteractionRouter>,
    task_owner: tokio::sync::Mutex<task_scope::HostTaskOwner>,
    mcp_owner: tokio::sync::Mutex<Box<dyn peri_acp_types::ports::McpTaskOwnerPort>>,
    session_id: String,
    cwd: String,
    end_hooks: tokio::sync::Mutex<SessionEndState>,
    cleanup_tasks: Arc<dyn peri_acp_types::tasks::TaskManager>,
    /// ACP 会话自身的后台任务管理器；Workspace Bash 使用 MCP 侧独立 owner。
    task_manager: Arc<dyn peri_acp_types::tasks::TaskManager>,
    /// `cfg(test)`：canonical idle 入口（`SessionInvocationService`）的观察面。
    ///
    /// 装配点是**注入**取消父的地方，只读环境自己新建的令牌看不到「注入错了哪根」；
    /// 这里保留 service 句柄，让测试断言它**实际持有**的取消父状态
    /// （[`Self::session_invocation_cancelled`]）。
    #[cfg(test)]
    session_invocation:
        Option<Arc<peri_middlewares::mcp::session_invocation::SessionInvocationService>>,
    /// 送进 builtin 实例上下文的那份 A24 关闭集（订阅建立门的唯一输入）。
    ///
    /// 生产运行不读它（关闭集已随上下文一次注入 pool），
    /// 保留副本只为让「装配派生自哪份 frozen」**可断言**——冷恢复的装配必须等于持久
    /// blob 的投影，而不是当轮配置/目录状态的投影（ARC-FROZEN-001）。
    #[cfg(test)]
    builtin_closed: std::collections::BTreeSet<String>,
    /// builtin `workflow` 实例的运行时槽位（会话就绪后绑定，见
    /// [`Self::bind_workflow_runtime`]）；`None` = 本环境未注入该槽位。
    #[cfg(not(target_os = "emscripten"))]
    workflow_input: Option<peri_mcp_workflow::WorkflowInstanceInput>,
    /// 宿主侧 workflow 任务消费者（P3 接线；随会话关闭收口，design §5.5）。
    #[cfg(not(target_os = "emscripten"))]
    workflow_consumer:
        parking_lot::Mutex<Option<crate::host::workflow_consumer::WorkflowTaskConsumer>>,
}

/// 测试观察面：会话环境 shutdown 事件（session_id）。
///
/// 环境在失败路径上被排空后即丢弃，外部无法再读到它；这里记录「哪个会话的环境
/// 被 shutdown」，供生命周期守卫测试断言（与 `builtin_closed` 同类测试观察面）。
#[cfg(test)]
static SHUTDOWN_OBSERVATIONS: std::sync::OnceLock<std::sync::Mutex<Vec<String>>> =
    std::sync::OnceLock::new();

/// 读取并清空 shutdown 观察记录（测试用）。
#[cfg(test)]
pub(crate) fn take_shutdown_observations() -> Vec<String> {
    let mutex = SHUTDOWN_OBSERVATIONS.get_or_init(|| std::sync::Mutex::new(Vec::new()));
    std::mem::take(&mut *mutex.lock().unwrap())
}

impl SessionEnvironment {
    /// 冻结期技能清单快照（W4b / F3，J1）：内容准入期从 **system 来源**
    /// （builtin `workspace` 实例）取一次技能元数据，供冻结 system prompt 的
    /// 技能摘要渲染。
    ///
    /// 与 [`Self::read_meta_docs`] 同一节奏与同一判定链（P4，activate 之后、
    /// `commit_frozen` 之前）：
    /// - 池未装配 / 句柄未出现且池初始化已收口 / 实例被 A24 关闭集关闭 /
    ///   未声明 skills 能力 ⇒ `Ok(空)`：技能面不适用，**不是失败**（X5/X4），
    ///   也不回落磁盘（J5 后宿主已无技能 FS 读取点）；
    /// - 句柄已连接但读取失败 ⇒ `Err`：system 已声明且被选中的投递失败
    ///   fail-closed（J2 补偿：调用方排空环境并撤销未发布的创建）；
    /// - 有界等待（同 `read_meta_docs` 的 10s 上界，同进程 builtin 握手毫秒级）：
    ///   全程 `await`，不 `block_on`（ARC-MIDDLEWARE-CAPABILITY-001 的
    ///   「异步工作在明示生命周期阶段完成」）。
    pub(crate) async fn read_workspace_skill_catalog(
        &self,
    ) -> Result<Vec<peri_acp_types::skills::SkillMetadata>, AcpError> {
        let Some(pool) = self.cfg.mcp_pool.as_ref() else {
            return Ok(Vec::new());
        };
        let deadline = peri_time::monotonic_now() + std::time::Duration::from_secs(10);
        loop {
            match pool.builtin_workspace_state() {
                McpBuiltinWorkspaceState::Connected => {
                    return pool
                        .read_builtin_workspace_skills()
                        .await
                        .map_err(|error| AcpError::new(-32603, error));
                }
                McpBuiltinWorkspaceState::Unavailable | McpBuiltinWorkspaceState::NotConnected => {
                    return Ok(Vec::new());
                }
                McpBuiltinWorkspaceState::Absent => {}
            }
            let phase = pool.snapshot()["initPhase"]
                .as_str()
                .unwrap_or("pending")
                .to_owned();
            if !matches!(phase.as_str(), "pending" | "initializing") {
                return Ok(Vec::new());
            }
            if peri_time::monotonic_now() >= deadline {
                return Ok(Vec::new());
            }
            peri_time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    /// 冻结期项目指令读取（W5/E15）：内容准入期从 **system 来源**
    /// （builtin `workspace` 实例）读 `peri-instruction://workspace/{main|local}`，
    /// 供冻结 system prompt 之前的指令段使用。
    ///
    /// 与 [`Self::read_meta_docs`] / [`Self::read_workspace_skill_catalog`] 同一
    /// 节奏与同一判定链（P4，activate 之后、`commit_frozen` 之前）：
    /// - 池未装配 / 句柄未出现且池初始化已收口 / 实例被 A24 关闭集关闭 ⇒
    ///   `Ok(默认空)`：指令面不适用，**不是失败**（X4/X5），也不回落磁盘
    ///   （J5 后宿主已无指令 FS 读取点）；
    /// - 句柄已连接但读取失败 ⇒ `Err`：system 已声明且被选中的投递失败
    ///   fail-closed（J2 补偿：调用方排空环境并撤销未发布的创建）；
    /// - 有界等待（同技能面的 10s 上界，同进程 builtin 握手毫秒级）：全程
    ///   `await`，不 `block_on`（ARC-MIDDLEWARE-CAPABILITY-001）。
    pub(crate) async fn read_workspace_instructions(
        &self,
    ) -> Result<crate::session::executor::FrozenInstructions, AcpError> {
        let Some(pool) = self.cfg.mcp_pool.as_ref() else {
            return Ok(Default::default());
        };
        let deadline = peri_time::monotonic_now() + std::time::Duration::from_secs(10);
        loop {
            match pool.builtin_workspace_state() {
                McpBuiltinWorkspaceState::Connected => {
                    let (main, local) = pool
                        .read_builtin_workspace_instructions()
                        .await
                        .map_err(|error| AcpError::new(-32603, error))?;
                    return Ok(crate::session::executor::FrozenInstructions { main, local });
                }
                McpBuiltinWorkspaceState::Unavailable | McpBuiltinWorkspaceState::NotConnected => {
                    return Ok(Default::default());
                }
                McpBuiltinWorkspaceState::Absent => {}
            }
            let phase = pool.snapshot()["initPhase"]
                .as_str()
                .unwrap_or("pending")
                .to_owned();
            if !matches!(phase.as_str(), "pending" | "initializing") {
                return Ok(Default::default());
            }
            if peri_time::monotonic_now() >= deadline {
                return Ok(Default::default());
            }
            peri_time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    /// 测试夹具入口：按当前目录状态**新构建**一份准备输入再装配。
    ///
    /// 生产不可达：恢复路径必须显式消费持久 blob（[`Self::assemble_prepared`] +
    /// `PreparedSessionInputs::prepare_restore`），new/legacy 走 `handle_new` /
    /// `prepare_for_restore` 的已定格输入。保留它只为测试可以省去准备步骤；
    /// `cfg(test)` 让「装配期第二次构建 frozen」这条路径在发布构建里不存在。
    #[cfg(test)]
    pub(crate) async fn assemble(
        host: &AcpServerConfig,
        cwd: &str,
        session_id: &str,
    ) -> Result<Option<Arc<Self>>, AcpError> {
        if host.workspace_assembly.is_none() {
            return Ok(None);
        }
        let inputs = super::prepared::PreparedSessionInputs::prepare_new(host, cwd)?;
        Self::assemble_prepared(host, &inputs, session_id).await
    }

    pub(crate) async fn assemble_prepared_without_frozen(
        host: &AcpServerConfig,
        inputs: &super::prepared::PreparedSessionInputs,
        session_id: &str,
    ) -> Result<Option<Arc<Self>>, AcpError> {
        Self::assemble_with_frozen(
            host,
            &inputs.cwd,
            session_id,
            None,
            &inputs.plugins(),
            &inputs.configuration,
            &inputs.session_mcp_servers,
        )
        .await
    }

    /// 装配期不第二次 `ConfigSource::load_at`、不第二次加载插件、不构建第二份 frozen。
    ///
    /// MCP / hooks 与 OAuth 消费者仍在本函数内创建（顺序不变）；调用方
    /// 已完成会话准入校验，准备阶段本身不启动这些资源。
    pub(crate) async fn assemble_prepared(
        host: &AcpServerConfig,
        inputs: &super::prepared::PreparedSessionInputs,
        session_id: &str,
    ) -> Result<Option<Arc<Self>>, AcpError> {
        // 装配是消费者：frozen / 插件 / 配置全部取自同一份准备输入（ARC-FROZEN-001 的
        // 同源收口）；恢复路径的输入由 `prepare_restore` / winner 注入定格，不在这里重建。
        Self::assemble_with_frozen(
            host,
            &inputs.cwd,
            session_id,
            inputs.frozen.as_ref(),
            &inputs.plugins(),
            &inputs.configuration,
            &inputs.session_mcp_servers,
        )
        .await
    }

    /// 同源装配：frozen 与插件由调用方**显式给定**，装配期不构建 frozen。
    ///
    /// `frozen` 是唯一事实源：new/legacy 是本次准备产物，恢复路径是持久 blob 的解码视图
    /// （winner）；`plugins` 是同一份准备输入的插件聚合；`configuration` 是同一份配置
    /// 视图。MCP / hooks 与 OAuth 消费者仍在本函数内创建（顺序不变）；调用方必须
    /// 已完成会话准入校验，准备阶段本身不启动这些资源。
    pub(crate) async fn assemble_with_frozen(
        host: &AcpServerConfig,
        cwd: &str,
        session_id: &str,
        frozen: Option<&crate::session::executor::FrozenSessionData>,
        plugins: &super::assemble::PreparedPlugins,
        configuration: &super::prepared::PreparedConfiguration,
        session_mcp_servers: &std::collections::HashMap<
            String,
            peri_acp_types::plugin::McpServerConfig,
        >,
    ) -> Result<Option<Arc<Self>>, AcpError> {
        let Some(source) = host.workspace_assembly.as_ref() else {
            return Ok(None);
        };
        let workspace_id = host
            .session_resources
            .session_workspace_id(&session_id.to_owned())
            .await
            .map_err(|error| AcpError::new(-32603, error.to_string()))?;
        // 配置视图与执行目录来自准备阶段定格的同一份输入：本函数不第二次
        // `ConfigSource::load_at`、不第二次解析 provider（`prepare_new` 的
        // `resolve_configuration` 已按「同目录复用 host 视图 / 异目录只读一次」定过格）。
        let cwd = cwd.to_owned();
        // ACP 会话任务仍由 SessionManager 登记；Workspace Bash owner 独立，
        // 通过 MCP Tasks 回执和订阅与本会话通信。
        let task_manager = host.session_manager.new_session_task_manager();
        // ── W4b：builtin `workspace` 实例的**真实资源面**输入 ──
        //
        // 技能来源（J5）整体归位 MCP 侧：user/project 根 + 插件根 + builtin
        // 静态资产由该实例的 provider 读取，宿主侧不再
        // 有任何技能文件系统读取点。输入在 `run_initialize` **之前**随上下文一次
        // 注入（AW3-11 / W4a 已建立的节奏），装配期不第二次读配置：
        //
        // - `skill_roots`：用户、项目与 F11（插件 manifest → 根 + 插件标签）
        //   适配器的产物，只产出「路径 + scope/标签」，**不读技能
        //   内容**；Builtin 占位根不映射为资源根（资产是 provider 的内置静态面，
        //   由 `disable_bundled` 位控制）。
        // - `disable_bundled`：只从本次选中的配置来源投影；无可信配置时关闭。
        // - 缺根（目录不存在）交给 provider 既有语义处理（缺失目录 = 空批）。
        // - Agent 项目/插件根与指令面（`peri-instruction://`）由同一 provider 装配。
        #[cfg(not(target_os = "emscripten"))]
        let disable_bundled = configuration
            .config_source
            .resource_configuration()
            .map(|resources| resources.disable_bundled_skills)
            .unwrap_or(true);
        #[cfg(not(target_os = "emscripten"))]
        let workspace_resources = super::workspace_resources::workspace_resources_input(
            &cwd,
            plugins,
            configuration
                .config
                .config
                .claude_md_excludes
                .as_deref()
                .unwrap_or_default(),
            disable_bundled,
        );
        // A24 关闭集：从**同一份 frozen snapshot** 派生（设计 §2.5：禁止回退当轮 config；
        // fork 复用 source 的 frozen 字节时两者可能不同）。它是订阅建立门的唯一输入，
        // 不改变 pool 级就绪与面板连接状态（ARC-CAPABILITY-CLOSURE-001）。
        //
        // W4b 收口：同一份 disabled 集合同批派生**宿主技能面关闭位**
        //（`"SkillsMiddleware" ∈ disabled`，与链槽装配的跳过判据同一字面量，见
        // `peri_middlewares/src/assembly.rs` 的 `ChainSlot::Skills` 分支）——链槽关闭
        // 时发现管线的 `core:{skill}` 裸名投影同批撤下（不得留幽灵路由）；workspace
        // 实例本身不受影响；系统来源不注册 `{server}:{skill}` 路由。两个位一次派生、随 builtin 实例
        // 上下文注入 pool，不在别处第三次读配置。
        let disabled_middlewares: std::collections::HashSet<String> = match frozen {
            Some(frozen) => frozen.meta_harness().disabled_middlewares.clone(),
            None => {
                crate::session::build_meta_harness_state(
                    configuration.config.config.meta_harness.as_ref(),
                    std::collections::HashMap::new(),
                )
                .disabled_middlewares
            }
        };
        let builtin_closed =
            peri_middlewares::assembly::builtin_closed_instances(&disabled_middlewares);
        // ── beta flag（设计 §消费契约）──
        //
        // 恢复 / fork 复用持久 blob 的字节；**新建**会话此时 frozen 尚未构建
        // （`prepared.build_frozen_after_activation` 在本环境装配之后），投影取自
        // 同一份选中的配置来源——它与随后冻结的值同源，装配期只读一次。
        // 本层不回退当轮 `peri_config`。生效项在这里记录一次（flag id + 来源层），供诊断。
        #[cfg(not(target_os = "emscripten"))]
        let beta_flags = match frozen {
            Some(frozen) => frozen.v2_frozen().beta_flags.clone(),
            None => configuration.config_source.beta_flags(),
        };
        #[cfg(not(target_os = "emscripten"))]
        for (id, origin) in beta_flags.enabled() {
            tracing::info!(flag = %id, origin = ?origin, "beta flag enabled for session");
        }
        // Bash 的有效 `run_in_background` 缺省：flag 语义只在此处解析一次，工具侧
        // 收到语义化布尔值。
        #[cfg(not(target_os = "emscripten"))]
        let workspace_bash_default_run_in_background =
            beta_flags.is_enabled(peri_acp_types::beta_flags::FULL_ASYNC_TOOLS);
        let skills_face_closed = disabled_middlewares.contains("SkillsMiddleware");
        // M6：插件来源闭合位与上面两个是同**一份** disabled 集合的投影（链槽
        // 装配的跳过判据、资源 roots / 命令 / hooks / MCP 合并 / 继承面的派生
        // 与 `PluginSourceAdmission` 引用同一常量，不在别处第三次读配置）。
        let plugin_face_closed =
            peri_middlewares::assembly::plugin_face_closed(&disabled_middlewares);
        #[cfg(test)]
        let closed_for_test = builtin_closed.clone();
        // 双轨期（P3–P5）：builtin `workflow` 实例的运行时槽位。运行时由会话旧链在
        // session/new / restore / fork 构造（其执行依赖来自该时点才冻结的会话数据，
        // 早于本点不可得），环境装配返回后经 `bind_workflow_runtime` 绑定**同一份**
        // 实例——MCP 面与旧链共享同一状态（`Arc::ptr_eq` 可观察）。
        #[cfg(not(target_os = "emscripten"))]
        let workflow_input = peri_mcp_workflow::WorkflowInstanceInput::slot();
        let input = assemble::HostAssemblyInput {
            provider: configuration.provider.clone(),
            peri_config: Arc::new(parking_lot::RwLock::new((*configuration.config).clone())),
            config_source: configuration.config_source.clone(),
            permission_mode: peri_acp_types::permission::SharedPermissionMode::new(
                host.permission_mode.load(),
            ),
            session_resources: host.session_resources.clone(),
            workspace_id,
            // 会话级装配：这里不是部署 owner，拿不到也不持有全局关闭权。
            session_store_shutdown: None,
            cwd: cwd.clone(),
            bare: source.bare,
            drive_cron_tick: source.drive_cron_tick,
            #[cfg(not(target_os = "emscripten"))]
            workspace_input: None,
            #[cfg(not(target_os = "emscripten"))]
            workspace_bash_default_run_in_background,
            #[cfg(not(target_os = "emscripten"))]
            workspace_resources: Some(workspace_resources),
            #[cfg(not(target_os = "emscripten"))]
            workflow_input: Some(workflow_input.clone()),
            builtin_closed,
            skills_face_closed,
            plugin_face_closed,
            prepared_plugins: Some(plugins.clone()),
            session_mcp_servers: Some(session_mcp_servers.clone()),
        };
        let activation = tokio_util::sync::CancellationToken::new();
        // session 生命周期令牌：与装配哨兵同点创建，但**不随 `activate()` 取消**
        // （取消只发生在 `shutdown()`），故 idle 执行的取消父在会话存活期内始终有效。
        let session_lifetime = tokio_util::sync::CancellationToken::new();
        let mut cfg = assemble::assemble_server_config_with_mcp_profile(
            input,
            source.mcp_profile.clone(),
            true,
            Some(activation.clone()),
            source.capabilities,
        )
        .await;
        cfg.session_manager
            .share_registry_with(&host.session_manager);
        cfg.controller = host.controller.clone();
        cfg.langfuse_session = host.langfuse_session.clone();
        cfg.stdio_command_filter = host.stdio_command_filter;
        let task_owner = cfg.host_task_owner.take().expect("session resource owner");
        let mcp_owner = cfg
            .mcp_task_owner
            .take()
            .expect("session MCP resource owner");
        // Session OAuth keeps the existing host event transport, while callbacks remain
        // attached to this workspace's MCP pool.
        if let (Some(mut events), Some(host_tx)) =
            (cfg.oauth_event_rx.take(), host.oauth_event_tx.clone())
        {
            let shutdown = cfg.host_task_spawner.shutdown_token();
            let session_id = session_id.to_owned();
            let _ = cfg.host_task_spawner.spawn(task_scope::HostTaskOwnerKind::Session, task_scope::HostTaskKind::OAuthConsumer, async move {
                loop { tokio::select! {
                    _ = shutdown.cancelled() => break,
                    event = events.recv() => match event {
                        Some(event) => { if host_tx.send(crate::event::oauth::HostOAuthEvent::Session { session_id: session_id.clone(), event: Box::new(event) }).is_err() { break; } }
                        None => break,
                    }
                }}
            });
        }
        // ── W5 / P0：冻结目录的**定点绑定**（此处而不是别处）──
        //
        // 本函数是 new / load / resume / fork 的**共同装配点**，而冻结渲染发生在
        // 它返回之后（`session_lifecycle.rs` 在装配后取 `env.cfg` 调
        // `prepared.rs::build_frozen_after_activation` → `session/frozen.rs` 经
        // 目录端口渲染 `{{available_agents}}`）：不在这里补齐，会话创建期的目录
        // 恒为空。turn 级装配 bind（`assembly/preparation.rs`）要等首轮才发生，
        // 两者是同一份 registry 形状、覆盖式（最后一次生效）。恢复路径复用持久
        // blob 的渲染结果，提前绑定只是同形覆盖，不改变可见性。
        // 池不适用 = 面未装配 ⇒ 静默跳过（既有语义：空目录，X4/J5 不回落磁盘）。
        if let Some(pool) = cfg.mcp_pool.as_ref() {
            let _ = peri_middlewares::host_ports::bind_agent_catalog_from_pool(
                &cfg.agent_catalog,
                pool,
                session_id,
                &disabled_middlewares,
            );
        }
        // ── session-owned 交互路由 + canonical 调用入口（S4/S5 接线）──
        //
        // 装配顺序说明：`SessionInvocationService` 需要 session 身份（请求 / record
        // 归属校验、准入视图过滤、会话关闭撤销），而 `assemble_server_config_*` 不知道
        // session id；因此本环境装配点（同一份池、同一份 disabled 集合、同一份
        // provider 快照）是这两个 session 级对象与 relay 的**唯一**构造点。
        // relay 在此处注入 canonical 入口 ⇒ 不存在「未装配入口」的运行时状态。
        let interaction_gate = Arc::new(tokio::sync::Mutex::new(()));
        let interaction_router =
            Arc::new(super::interaction_router::SessionInteractionRouter::new(
                session_id.to_owned(),
                Arc::clone(&interaction_gate),
            ));
        let session_invocation = {
            let pool = cfg
                .mcp_pool
                .as_ref()
                .and_then(|port| {
                    Arc::clone(port)
                        .downcast_arc::<peri_middlewares::mcp::McpClientPool>()
                        .ok()
                })
                .ok_or_else(|| {
                    AcpError::new(
                        -32603,
                        "session MCP pool missing; cannot assemble the canonical invocation entry",
                    )
                })?;
            // 取消父 = **session 生命周期**令牌（不是装配哨兵 `activation`）：
            // 既作为规则链 `AssemblyContext.cancel`，也作为 service 的 `session_cancel`
            // （`InvocationCancel::issue` 由它签发子令牌）。
            let cancel = session_lifetime.clone();
            let rules = super::workspace_interaction::build_idle_invocation_rules(
                &peri_middlewares::assembly::ProductionChainAssembler,
                &cfg,
                &configuration.provider,
                &configuration.config,
                session_id,
                &cwd,
                Arc::clone(&interaction_router)
                    as Arc<dyn peri_acp_types::interaction::UserInteractionBroker>,
                cancel.clone(),
                &disabled_middlewares,
                {
                    #[cfg(not(target_os = "emscripten"))]
                    {
                        workspace_bash_default_run_in_background
                    }
                    #[cfg(target_os = "emscripten")]
                    {
                        false
                    }
                },
                &host.session_resources,
                &task_manager, // 会话级管理器（同一实例）：idle 期异步 hook 须挂会话作用域
            );
            Arc::new(
                peri_middlewares::mcp::session_invocation::SessionInvocationService::new(
                    peri_middlewares::mcp::session_invocation::SessionInvocationInputs {
                        pool: Arc::clone(&pool),
                        agent_id: peri_acp_types::identity::AgentId::new(),
                        session_id: session_id.to_owned(),
                        cwd: Arc::from(cwd.as_str()),
                        // hook 输出端口：idle 规则链的 hook 注入只走会话队列（与 turn 路径
                        // 同一队列实例，见 `SessionAccessPort::v2_message_queue`）。
                        queue: {
                            use peri_acp_types::session::SessionAccessPort as _;
                            let manager = host.session_manager.clone();
                            let sid = session_id.to_owned();
                            Arc::new(move || {
                                manager.v2_message_queue(&sid).unwrap_or_else(|| {
                                    // 与 `build_stage_context` 同一投影语义（取不到会话队列
                                    // 时用 detached 队列；生产路径上会话必然已登记）。
                                    tracing::debug!(
                                        session_id = %sid,
                                        "idle invocation queue lookup missed; using a detached queue"
                                    );
                                    peri_acp_types::session::MessageQueue::new()
                                })
                            })
                        },
                        rules: Arc::new(rules),
                        closed_instances: peri_middlewares::assembly::builtin_closed_instances(
                            &disabled_middlewares,
                        ),
                        session_cancel: cancel,
                    },
                ),
            )
        };
        // App 面 relay：canonical 入口在构造期注入（`PoolMcpAppsRelay::new` 必填），
        // 未装配入口的运行时状态不存在。仅 apps 能力开启时构造（其余情况 App 面
        // 本就不可达，relay 为 None 即显式拒绝）。
        if source.mcp_profile.apps_enabled() {
            let pool = cfg
                .mcp_pool
                .as_ref()
                .and_then(|port| {
                    Arc::clone(port)
                        .downcast_arc::<peri_middlewares::mcp::McpClientPool>()
                        .ok()
                })
                .ok_or_else(|| {
                    AcpError::new(
                        -32603,
                        "session MCP pool missing; cannot assemble the MCP Apps relay",
                    )
                })?;
            let runtime_epoch = session_invocation.runtime_epoch();
            let invocation_port: Arc<dyn peri_acp_types::mcp_apps::SessionInvocationPort> =
                Arc::clone(&session_invocation)
                    as Arc<dyn peri_acp_types::mcp_apps::SessionInvocationPort>;
            let apps_relay = peri_middlewares::mcp::apps_relay::PoolMcpAppsRelay::new(
                pool,
                runtime_epoch,
                Arc::clone(&invocation_port),
            );
            // 会话运行时世代登记：签发点（MCP bridge）据此给 App record 盖章，
            // 未登记即**不签发**。登记与会话环境同寿命（`close_session` 清除），
            // 登记点就是本装配点（与 relay 同一作用域、同一 epoch）。
            apps_relay.register_session_incarnation(session_id);
            let relay: Arc<dyn peri_acp_types::mcp_apps::McpAppsRelayPort> = Arc::new(apps_relay);
            // facade 与 relay **同点同源**：同一 router、同一 canonical 入口、同一
            // runtime epoch（纪元不同源会让 record 与 router 的纪元不一致，审批路由
            // 随即不匹配）；序号权威在 facade 的 ledger，relay 不再持任何序号计数器。
            cfg.mcp_apps_facade = Some(Arc::new(super::apps_facade::SessionAppsFacade::new(
                session_id.to_owned(),
                runtime_epoch,
                Arc::clone(&interaction_router),
                Arc::clone(&relay),
                invocation_port,
                cfg.host_task_spawner.clone(),
                host.session_authorization.clone(),
            )));
            cfg.mcp_apps_relay = Some(relay);
        }
        Ok(Some(Arc::new(Self {
            cfg,
            activation,
            session_lifetime,
            interaction_gate,
            interaction_router,
            #[cfg(test)]
            session_invocation: Some(Arc::clone(&session_invocation)),
            task_owner: tokio::sync::Mutex::new(task_owner),
            mcp_owner: tokio::sync::Mutex::new(mcp_owner),
            session_id: session_id.to_owned(),
            cwd,
            end_hooks: tokio::sync::Mutex::new(SessionEndState::Pending),
            cleanup_tasks: Arc::new(peri_agent::agent::async_tasks::TaskManager::new()),
            task_manager,
            #[cfg(test)]
            builtin_closed: closed_for_test,
            #[cfg(not(target_os = "emscripten"))]
            workflow_input: Some(workflow_input),
            #[cfg(not(target_os = "emscripten"))]
            workflow_consumer: parking_lot::Mutex::new(None),
        })))
    }

    pub(crate) fn activate(&self) {
        self.activation.cancel();
    }

    /// session-owned 交互门（`SessionState.interaction_gate` 借用同一 `Arc`）。
    pub(crate) fn interaction_gate(&self) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(&self.interaction_gate)
    }

    /// session-owned 交互路由（prompt / App / Host 三个入口共用）。
    pub(crate) fn interaction_router(
        &self,
    ) -> &Arc<super::interaction_router::SessionInteractionRouter> {
        &self.interaction_router
    }

    /// session-level MCP Apps facade（`apps/*` 的唯一入口；apps 能力关闭时为 `None`）。
    pub(crate) fn apps_facade(&self) -> Option<&Arc<super::apps_facade::SessionAppsFacade>> {
        self.cfg.mcp_apps_facade.as_ref()
    }

    /// 冻结前读取 MetaHarness 覆盖文档（J6/X7/X8）：只经**本会话环境**的真实
    /// builtin `workspace` 实例，等其连接收口后一次性读启用 section。
    ///
    /// - 无池 / 无句柄且池初始化已收口（ready/failed）/ 句柄非 Connected ⇒ 空批，
    ///   由宿主按 X8「覆盖不可得 ⇒ 保持内置」处理（不回落磁盘）；
    /// - 等待上界 10s：builtin 实例是同进程链路，握手很快；失败会以 Failed 句柄出现
    ///   而立即短路，这里的上界只兜底「句柄始终不出现」的异常装配。
    pub(crate) async fn read_meta_docs(
        &self,
        enabled_sections: &std::collections::HashSet<String>,
    ) -> Result<std::collections::HashMap<String, String>, AcpError> {
        let Some(pool) = self.cfg.mcp_pool.as_ref() else {
            return Ok(Default::default());
        };
        let deadline = peri_time::monotonic_now() + std::time::Duration::from_secs(10);
        loop {
            match pool.builtin_workspace_state() {
                McpBuiltinWorkspaceState::Connected => {
                    return pool
                        .read_builtin_workspace_meta(enabled_sections)
                        .await
                        .map_err(|error| AcpError::new(-32603, error));
                }
                McpBuiltinWorkspaceState::Unavailable | McpBuiltinWorkspaceState::NotConnected => {
                    return Ok(Default::default());
                }
                McpBuiltinWorkspaceState::Absent => {}
            }
            // 池初始化已收口而 workspace 句柄仍不存在（实例未装配 / 被关闭 / 非 bare
            // 配置类另有故障）：不再等待，按覆盖不可得处理。
            let phase = pool.snapshot()["initPhase"]
                .as_str()
                .unwrap_or("pending")
                .to_owned();
            if !matches!(phase.as_str(), "pending" | "initializing") {
                return Ok(Default::default());
            }
            if peri_time::monotonic_now() >= deadline {
                return Ok(Default::default());
            }
            peri_time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    /// ACP 会话任务管理器；不承担 Workspace Bash 的所有权。
    pub(crate) fn task_manager(&self) -> Arc<dyn peri_acp_types::tasks::TaskManager> {
        Arc::clone(&self.task_manager)
    }

    /// 把会话级 `WorkflowRuntime` 绑定进 builtin `workflow` 实例槽位（P5：运行时
    /// 由宿主创建，不再经旧链 middleware 端口），两条面共享同一份状态。
    ///
    /// 返回 `None`：本环境未注入 workflow 实例槽位（该部署形态没有 builtin 实例面）。
    /// 返回 `Some` 时由调用方按 [`peri_mcp_workflow::BindOutcome`] 裁决：`Conflict`
    /// 是装配异常，按会话初始化失败处理（design §6.2），不降级为警告。
    ///
    /// 同点把 session 级 `TaskManager` 注入运行时（bg registry：workflow run 的
    /// 取消面），与运行时创建同一处收口（旧链时期在每次执行的装配里注入）。
    #[cfg(not(target_os = "emscripten"))]
    pub(crate) fn bind_workflow_runtime(
        &self,
        runtime: &Arc<peri_mcp_workflow::WorkflowRuntime>,
    ) -> Option<peri_mcp_workflow::BindOutcome> {
        let input = self.workflow_input.as_ref()?;
        runtime.set_bg_registry(self.task_manager());
        Some(input.bind(Arc::clone(runtime)))
    }

    /// 槽位绑定的 `WorkflowRuntime`（双轨期由会话旧链构造并绑定；`None` = 未绑定）。
    #[cfg(not(target_os = "emscripten"))]
    pub(crate) fn workflow_runtime(&self) -> Option<Arc<peri_mcp_workflow::WorkflowRuntime>> {
        self.workflow_input
            .as_ref()
            .and_then(|input| input.runtime())
            .cloned()
    }

    /// 启动宿主侧 workflow 任务消费者（design §5.5；P3 就绪与订阅接线）。
    ///
    /// 前置：槽位已绑定（[`Self::bind_workflow_runtime`] 成功）。消费者在后台
    /// 建立 `workflow://tasks` 订阅并完成基线读取后才置 `consumer_ready`；就绪前
    /// `Workflow` 工具拒绝新启动（就绪屏障），订阅失败回落 not-ready 而非带病放行。
    ///
    /// 返回 `false`：本部署形态没有 workflow 实例面（无池 / 未注入槽位 / 未绑定），
    /// 无需消费者，不是失败。
    #[cfg(not(target_os = "emscripten"))]
    pub(crate) fn start_workflow_consumer(
        &self,
        executor: Arc<dyn peri_acp_types::workflow::AgentExecutor>,
    ) -> bool {
        let Some(pool) = self.cfg.mcp_pool.clone() else {
            return false;
        };
        let Some(runtime) = self.workflow_runtime() else {
            return false;
        };
        let consumer = crate::host::workflow_consumer::WorkflowTaskConsumer::start(
            pool,
            runtime,
            executor,
            self.session_id.clone(),
        );
        // 重复调用（异常路径）时旧消费者随赋值 drop → 取消，不残留双消费者。
        *self.workflow_consumer.lock() = Some(consumer);
        true
    }

    /// 停止消费者：取消订阅与在飞执行并有界等待收口（会话关闭路径；幂等）。
    #[cfg(not(target_os = "emscripten"))]
    async fn stop_workflow_consumer(&self) {
        // 锁在本语句内收口（guard 不跨 await），返回后 await 收口。
        let consumer = self.workflow_consumer.lock().take();
        if let Some(consumer) = consumer {
            consumer.stop().await;
        }
    }

    /// 本环境装配时派生的 A24 关闭集（订阅建立门的唯一输入）。
    ///
    /// `cfg(test)`：这是**测试观察面**——验证关闭集来自哪份 frozen（而不是当轮配置）。
    #[cfg(test)]
    pub(crate) fn builtin_closed(&self) -> &std::collections::BTreeSet<String> {
        &self.builtin_closed
    }

    /// `cfg(test)`：canonical idle 执行的取消父（service **实际持有**的那根令牌）
    /// 是否已取消。
    ///
    /// 装配期必须为 `false`（否则 App 回调 / Host invoke 在审批前即被 `biased`
    /// select 判中断，生产装配下全不可用），[`Self::shutdown`] 之后必须为 `true`。
    /// `None` = 本环境未构造该入口（装配失败前不可达）。
    #[cfg(test)]
    pub(crate) fn session_invocation_cancelled(&self) -> Option<bool> {
        self.session_invocation
            .as_ref()
            .map(|service| service.session_cancelled())
    }

    pub(crate) async fn shutdown(&self) -> bool {
        #[cfg(test)]
        SHUTDOWN_OBSERVATIONS
            .get_or_init(|| std::sync::Mutex::new(Vec::new()))
            .lock()
            .unwrap()
            .push(self.session_id.clone());
        // 会话终结的**取消权威**：canonical idle 执行的取消父在此取消，在飞的
        // App 回调 / Host invoke 有界收尾（幂等；`session/close` 与失败路径排空
        // 都经此处）。放在排空之前：后续步骤不再接受新执行。
        self.session_lifetime.cancel();
        // Apps 域：销毁本层持有的 Host 操作域（已销毁的 namespace 不得重建；
        // 附件 namespace 随环境丢弃）。**不**取消已 dispatch 的调用。
        if let Some(facade) = self.cfg.mcp_apps_facade.as_ref() {
            facade.on_session_closing();
        }
        // 会话终结即撤销本会话声明的 MCP-over-ACP 路由与池条目：声明只在该
        // 会话存续期间有效，会话不再存活后既没有归属也不会有入站消息；处置必须
        // 在池关闭之前完成，否则 close_session 已无出站通道可用。实现幂等，
        // 关闭重试重复调用是安全的。
        if let Some(port) = self.cfg.acp_mcp.as_ref() {
            port.close_session(&self.session_id).await;
        }
        if !self.finish_session_end().await {
            return false;
        }
        // 关闭顺序第 ① 步（design §5.5 / §4.4）：密封运行时准入（不再接受新 run 与
        // 新任务发布；F6 `CloseGate` 的宿主接线点），随后停止消费者——停止领取新任务、
        // 取消在飞执行并有界等待收口（②③）；订阅随消费者句柄 drop 注销，均先于
        // 池关闭与订阅释放（④⑤由下方 task_owner / pool 关闭承担）。
        #[cfg(not(target_os = "emscripten"))]
        {
            if let Some(runtime) = self.workflow_runtime() {
                runtime.mark_closed();
            }
            self.stop_workflow_consumer().await;
        }
        let mut tasks = self.task_owner.lock().await;
        tasks.begin_shutdown();
        if let Some(pool) = self.cfg.mcp_pool.as_ref() {
            pool.begin_shutdown();
        }
        if let Some(dynamic) = self.cfg.dynamic_mcp.as_ref() {
            dynamic.begin_shutdown();
        }
        let host = tasks.shutdown().await;
        let dynamic = match self.cfg.dynamic_mcp.as_ref() {
            Some(dynamic) => dynamic.shutdown().await,
            None => peri_acp_types::dynamic_mcp::DynamicMcpShutdownReport::Complete,
        };
        let mut mcp_tasks = self.mcp_owner.lock().await;
        mcp_tasks.begin_shutdown();
        let _ = mcp_tasks.shutdown().await;
        let pool = match self.cfg.mcp_pool.as_ref() {
            Some(pool) => pool.shutdown().await,
            None => peri_acp_types::ports::McpPoolShutdownReport::Complete {
                settled_services: 0,
                failed_services: 0,
            },
        };
        matches!(
            task_scope::HostTerminalShutdownReport::aggregate(host, dynamic, pool, 0),
            task_scope::HostTerminalShutdownReport::Complete { .. }
        )
    }

    /// Keep one terminal hook execution across close retries. Its cleanup scope is
    /// separate because ordinary session task admission has already closed.
    async fn finish_session_end(&self) -> bool {
        let mut state = self.end_hooks.lock().await;
        if matches!(*state, SessionEndState::Pending) {
            let hooks = self
                .cfg
                .hook_groups
                .iter()
                .flatten()
                .filter(|hook| hook.event == peri_acp_types::hooks::HookEvent::SessionEnd)
                .cloned()
                .collect::<Vec<_>>();
            if !self.activation.is_cancelled() || hooks.is_empty() {
                *state = SessionEndState::Finished;
            } else if let Err(error) =
                validate_expected(&self.cfg, &self.session_id, Some(&self.cwd)).await
            {
                // This hook never started. Invalid execution context must not
                // prevent draining resources that the session already owns.
                tracing::warn!(error = %error.message, "SessionEnd skipped: workspace binding is no longer valid");
                *state = SessionEndState::Skipped;
            } else {
                let cwd = self.cwd.clone();
                let session_id = self.session_id.clone();
                let model = self.cfg.provider.read().model_name().to_owned();
                let tasks = self.cleanup_tasks.clone();
                match self
                    .cleanup_tasks
                    .spawn_owned(assemble::build_session_end_task(
                        hooks, cwd, session_id, model, tasks,
                    )) {
                    Ok(handle) => *state = SessionEndState::Running(handle),
                    Err(error) => {
                        tracing::warn!(%error, "SessionEnd hook admission failed");
                        *state = SessionEndState::Failed;
                    }
                }
            }
        }
        if let SessionEndState::Running(handle) = &mut *state {
            // Timeout leaves the task and its process ownership in this environment;
            // the next close/EOF attempt joins the same invocation.
            match peri_time::timeout(std::time::Duration::from_secs(5), handle).await {
                Ok(Ok(())) => *state = SessionEndState::Finished,
                Ok(Err(error)) => {
                    tracing::warn!(%error, "SessionEnd hook task failed");
                    *state = SessionEndState::Failed;
                }
                Err(_) => return false,
            }
        }
        let joined = matches!(*state, SessionEndState::Finished | SessionEndState::Skipped);
        let cleanup = self.cleanup_tasks.shutdown().await;
        joined && cleanup == peri_acp_types::tasks::TaskShutdownReport::Complete
    }
}

/// 内容准入期的冻结运行环境（H3 / D1）。
///
/// 判定基于**有效 Workspace 来源**（`McpClientPool::workspace_source`：会话声明
/// 含持久 owner 装载的结果，以及部署/全局/项目/插件合并配置与 builtin overlay）
/// 并叠加准备输入的会话声明（initialize 前也可见）：
/// - 显式远端 Workspace（[`peri_acp_types::plugin::ConfigSource::WorkspaceRemote`]）
///   ⇒ `None`：工具在远端执行，计算宿主探测值不是它的运行环境——冻结为
///   unavailable 并显式标记，**不冒充**；
/// - builtin `workspace`（同进程实例）或无 `workspace` 面 ⇒ 计算宿主即选定执行
///   环境：准入期探测一次，随后随冻结持久化（ARC-FROZEN-001）。
pub(crate) fn frozen_runtime_env(
    host: &AcpServerConfig,
    session_mcp_servers: &std::collections::HashMap<
        String,
        peri_acp_types::plugin::McpServerConfig,
    >,
    cwd: &str,
) -> Option<crate::prompt::PromptRuntimeEnv> {
    use peri_acp_types::plugin::ConfigSource;
    let remote = matches!(
        session_mcp_servers
            .get("workspace")
            .and_then(|config| config.source.as_ref()),
        Some(ConfigSource::WorkspaceRemote)
    ) || host
        .mcp_pool
        .as_ref()
        .is_some_and(|pool| pool.workspace_source() == Some(ConfigSource::WorkspaceRemote));
    if remote {
        tracing::warn!(
            cwd = %cwd,
            "显式远端 Workspace：宿主探测值不是该执行环境，冻结运行环境标记为 unavailable"
        );
        return None;
    }
    Some(crate::prompt::PromptRuntimeEnv::detect(cwd))
}

pub(crate) fn workspace_error(error: impl Into<anyhow::Error>) -> AcpError {
    let error = error.into();
    AcpError::new(-32010, error.to_string())
}

/// 门面行为失败 → ACP 错误：本机 workspace 语义保留领域分类，
/// 其余按行为失败上报，不把失败伪装成 workspace 问题。
pub(crate) fn resource_error(error: SessionResourceError) -> AcpError {
    match error.workspace_error() {
        Some(workspace) => workspace_error(workspace.clone()),
        None => AcpError::new(-32010, error.to_string()),
    }
}

/// 绑定复核强度。
///
/// 准入边界（协议读请求、`session/new` | `load` | `resume` | `fork` 的入口）复核完整
/// 发现快照；同一次准入内的后续检查只复核已记录证据——准入已经观测过完整快照，为
/// 同一件事再跑一轮 Git 发现不会带来新证据，只会把 Git 的等待叠加到这次准入的每一步。
#[derive(Clone, Copy)]
pub(crate) enum BindingCheck {
    Full,
    Recorded,
}

/// 一次准入的权威检查：复核完整发现快照，并比对调用方给出的执行目录。
///
/// 一次准入只应调用一次（准入以本次复核为判定依据）；准入内的后续检查用
/// [`reassert_expected`]。
pub(crate) async fn validate_expected(
    cfg: &AcpServerConfig,
    session_id: &str,
    expected: Option<&str>,
) -> Result<ResolvedWorkspace, AcpError> {
    check_expected(cfg, session_id, expected, BindingCheck::Full).await
}

/// 同一次准入内的检查：只复核已记录证据，不重新执行 Git 发现。
///
/// 绑定不存在、关系不一致、目录被替换或换位仍然失败；省去的只有「重新执行 Git
/// 发现」——准入已经观测过完整快照，重复发现只是把 Git 的等待叠加到同一次准入。
pub(crate) async fn reassert_expected(
    cfg: &AcpServerConfig,
    session_id: &str,
    expected: Option<&str>,
) -> Result<ResolvedWorkspace, AcpError> {
    check_expected(cfg, session_id, expected, BindingCheck::Recorded).await
}

/// 按会话 identity 复核绑定（门面投影，只读、不改绑、不取执行权）。
async fn check_expected(
    cfg: &AcpServerConfig,
    session_id: &str,
    _expected: Option<&str>,
    check: BindingCheck,
) -> Result<ResolvedWorkspace, AcpError> {
    let id = ThreadId::from(session_id.to_owned());
    let recheck = match check {
        BindingCheck::Full => BindingRecheck::Full,
        BindingCheck::Recorded => BindingRecheck::Recorded,
    };
    let workspace = cfg
        .controller
        .sessions()
        .validate_bound_workspace(&id, recheck)
        .await
        .map_err(resource_error)?;
    Ok(workspace)
}
