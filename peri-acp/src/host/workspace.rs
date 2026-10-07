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
    task_owner: tokio::sync::Mutex<task_scope::HostTaskOwner>,
    mcp_owner: tokio::sync::Mutex<Box<dyn peri_acp_types::ports::McpTaskOwnerPort>>,
    session_id: String,
    cwd: String,
    end_hooks: tokio::sync::Mutex<SessionEndState>,
    cleanup_tasks: Arc<dyn peri_acp_types::tasks::TaskManager>,
    /// ACP 会话自身的后台任务管理器；Workspace Bash 使用 MCP 侧独立 owner。
    task_manager: Arc<dyn peri_acp_types::tasks::TaskManager>,
    /// 送进 builtin 实例上下文的那份 A24 关闭集（订阅建立门的唯一输入）。
    ///
    /// 生产运行不读它（关闭集已随上下文一次注入 pool），
    /// 保留副本只为让「装配派生自哪份 frozen」**可断言**——冷恢复的装配必须等于持久
    /// blob 的投影，而不是当轮配置/目录状态的投影（ARC-FROZEN-001）。
    #[cfg(test)]
    builtin_closed: std::collections::BTreeSet<String>,
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
        let skills_face_closed = disabled_middlewares.contains("SkillsMiddleware");
        #[cfg(test)]
        let closed_for_test = builtin_closed.clone();
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
            workspace_resources: Some(workspace_resources),
            builtin_closed,
            skills_face_closed,
            prepared_plugins: Some(plugins.clone()),
            session_mcp_servers: Some(session_mcp_servers.clone()),
        };
        let activation = tokio_util::sync::CancellationToken::new();
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
        cfg.execution_admission_port = host.execution_admission_port.clone();
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
        Ok(Some(Arc::new(Self {
            cfg,
            activation,
            task_owner: tokio::sync::Mutex::new(task_owner),
            mcp_owner: tokio::sync::Mutex::new(mcp_owner),
            session_id: session_id.to_owned(),
            cwd,
            end_hooks: tokio::sync::Mutex::new(SessionEndState::Pending),
            cleanup_tasks: Arc::new(peri_agent::agent::async_tasks::TaskManager::new()),
            task_manager,
            #[cfg(test)]
            builtin_closed: closed_for_test,
        })))
    }

    pub(crate) fn activate(&self) {
        self.activation.cancel();
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

    /// 本环境装配时派生的 A24 关闭集（订阅建立门的唯一输入）。
    ///
    /// `cfg(test)`：这是**测试观察面**——验证关闭集来自哪份 frozen（而不是当轮配置）。
    #[cfg(test)]
    pub(crate) fn builtin_closed(&self) -> &std::collections::BTreeSet<String> {
        &self.builtin_closed
    }

    pub(crate) async fn shutdown(&self) -> bool {
        // 会话终结即断开本会话声明的 MCP-over-ACP 连接：连接由 client 侧的
        // ACP 通道承载，会话不再存活后既没有归属也不会有入站消息；处置必须在
        // 池关闭之前完成，否则 `mcp/disconnect` 已无出站通道可用。实现幂等，
        // 关闭重试重复调用是安全的。
        if let Some(port) = self.cfg.acp_mcp.as_ref() {
            port.close_session(&self.session_id).await;
        }
        if !self.finish_session_end().await {
            return false;
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
