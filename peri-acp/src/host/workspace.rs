//! A session owns one verified execution environment and its resource lifetime.

use std::{path::Path, sync::Arc};

use super::{assemble, task_scope, AcpServerConfig, SessionState};
use crate::transport::types::AcpError;
use peri_acp_types::session_resources::{BindingRecheck, SessionResourceError};
use peri_acp_types::thread::ThreadId;
use peri_acp_types::workspace::{
    ReadOnlyAdmission, RecoveryRequiredDetails, ResolvedWorkspace, SessionExecutionLease,
    WorkspaceError, WorkspaceErrorData,
};

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
    /// 本会话环境的 per-session 后台任务管理器（AW3-11 第一成员）。
    ///
    /// 产生点在本模块的装配（`assemble` 开头，早于 builtin 上下文构造），随后**同一
    /// `Arc`** 经 `WorkspaceInstanceInput` 送进 builtin `workspace` 实例，并由三条会话
    /// 路径交给 `SessionManager::ensure_session_with_task_manager` 登记为
    /// `AcpSession::task_manager`——三处是同一份（`Arc::ptr_eq` 可观察）。
    task_manager: Arc<dyn peri_acp_types::tasks::TaskManager>,
    /// 装配面**送进 builtin `workspace` 实例上下文**的那份 [`WorkspaceInstanceInput`]
    /// （AW3-11 两名成员：per-session `TaskManager` + session 级 `on_bg_complete`）。
    ///
    /// 生产运行不读它（输入已随池的上下文一次注入），保留副本的唯一用途是让
    /// 「送进上下文的那份 == 环境持有的那份（= 会话持有的那份）」这条链**可断言**
    /// ——两个落点各自持值，断言才有内容；若只从同一个值派生，断言会退化成自比较。
    /// 因此本字段在非测试构建里无人读 ⇒ 收窄到 `cfg(test)`，而不是加 `#[allow(dead_code)]`
    /// （本 crate 禁放宽 lint）。
    #[cfg(test)]
    workspace_input: peri_mcp_workspace::WorkspaceInstanceInput,
    /// 送进 builtin 实例上下文的那份 A24 关闭集（订阅建立门的唯一输入）。
    ///
    /// 与 `workspace_input` 同一处置：生产运行不读它（关闭集已随上下文一次注入 pool），
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
        use peri_acp_types::ports::McpPoolPort as _;
        let Some(pool) = self.cfg.mcp_pool.as_ref() else {
            return Ok(Vec::new());
        };
        let Some(pool) = pool
            .as_any()
            .downcast_ref::<peri_middlewares::mcp::McpClientPool>()
        else {
            return Ok(Vec::new());
        };
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Some(handle) = pool.get_client("workspace") {
                if !matches!(
                    handle.status,
                    peri_middlewares::mcp::ClientStatus::Connected
                ) {
                    return Ok(Vec::new());
                }
                return pool
                    .read_builtin_workspace_skills()
                    .await
                    .map_err(|error| AcpError::new(-32603, error));
            }
            let phase = pool.snapshot()["initPhase"]
                .as_str()
                .unwrap_or("pending")
                .to_owned();
            if !matches!(phase.as_str(), "pending" | "initializing") {
                return Ok(Vec::new());
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(Vec::new());
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
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
        use peri_acp_types::ports::McpPoolPort as _;
        let Some(pool) = self.cfg.mcp_pool.as_ref() else {
            return Ok(Default::default());
        };
        let Some(pool) = pool
            .as_any()
            .downcast_ref::<peri_middlewares::mcp::McpClientPool>()
        else {
            return Ok(Default::default());
        };
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Some(handle) = pool.get_client("workspace") {
                if !matches!(
                    handle.status,
                    peri_middlewares::mcp::ClientStatus::Connected
                ) {
                    return Ok(Default::default());
                }
                let (main, local) = pool
                    .read_builtin_workspace_instructions()
                    .await
                    .map_err(|error| AcpError::new(-32603, error))?;
                return Ok(crate::session::executor::FrozenInstructions { main, local });
            }
            let phase = pool.snapshot()["initPhase"]
                .as_str()
                .unwrap_or("pending")
                .to_owned();
            if !matches!(phase.as_str(), "pending" | "initializing") {
                return Ok(Default::default());
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(Default::default());
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
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
        )
        .await
    }

    /// 装配期不第二次 `ConfigSource::load_at`、不第二次加载插件、不构建第二份 frozen。
    ///
    /// MCP / LSP / hooks 与 OAuth 消费者仍在本函数内创建（顺序不变）；调用方
    /// 必须已取得执行所有权，准备阶段本身不启动这些资源。
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
        )
        .await
    }

    /// 同源装配：frozen 与插件由调用方**显式给定**，装配期不构建 frozen。
    ///
    /// `frozen` 是唯一事实源：new/legacy 是本次准备产物，恢复路径是持久 blob 的解码视图
    /// （winner）；`plugins` 是同一份准备输入的插件聚合；`configuration` 是同一份配置
    /// 视图。MCP / LSP / hooks 与 OAuth 消费者仍在本函数内创建（顺序不变）；调用方必须
    /// 已取得执行所有权，准备阶段本身不启动这些资源。
    pub(crate) async fn assemble_with_frozen(
        host: &AcpServerConfig,
        cwd: &str,
        session_id: &str,
        frozen: Option<&crate::session::executor::FrozenSessionData>,
        plugins: &super::assemble::PreparedPlugins,
        configuration: &super::prepared::PreparedConfiguration,
    ) -> Result<Option<Arc<Self>>, AcpError> {
        let Some(source) = host.workspace_assembly.as_ref() else {
            return Ok(None);
        };
        // 配置视图与执行目录来自准备阶段定格的同一份输入：本函数不第二次
        // `ConfigSource::load_at`、不第二次解析 provider（`prepare_new` 的
        // `resolve_configuration` 已按「同目录复用 host 视图 / 异目录只读一次」定过格）。
        let cwd = cwd.to_owned();
        // ── AW3-11：session 级 seam 的两名成员在**构造 `HostAssemblyInput` 之前**
        //    产生 ──
        //
        // `TaskManager` 的产生点上提到这里（原来的产生点是 `ensure_session` →
        // `build_session`，晚于本函数，见主 plan §2 AW3-11 依据 2）：本环境随后把它
        // 经 builtin 上下文一次注入 pool（A33，早于 `run_initialize`），并在
        // `activate()` 之前由会话路径登记为 `AcpSession::task_manager`。
        //
        // `on_bg_complete` 是 session 级闭包：装配点 session **尚未注册**，
        // `session_inbox` 取不到是预期，闭包体内 lazy resolve（禁止急切取）。
        let task_manager = host.session_manager.new_session_task_manager();
        let on_bg_complete = peri_agent::session::bg_complete::session_bg_complete_callback(
            Arc::new(host.session_manager.clone()),
            session_id.to_owned(),
        );
        let workspace_input = peri_mcp_workspace::WorkspaceInstanceInput {
            task_manager: Some(Arc::clone(&task_manager)),
            on_bg_complete: Some(on_bg_complete),
        };
        // ── W4b：builtin `workspace` 实例的**真实资源面**输入 ──
        //
        // 技能来源（J5）整体归位 MCP 侧：本地三根（user / global `skillsDir` /
        // project）+ 插件根 + builtin 静态资产由该实例的 provider 读取，宿主侧不再
        // 有任何技能文件系统读取点。输入在 `run_initialize` **之前**随上下文一次
        // 注入（AW3-11 / W4a 已建立的节奏），装配期不第二次读配置：
        //
        // - `skill_roots`：F11（插件 manifest → 根 + 插件标签）与 F12（settings 的
        //   `skillsDir`）适配器的产物，只产出「路径 + scope/标签」，**不读技能
        //   内容**；Builtin 占位根不映射为资源根（资产是 provider 的内置静态面，
        //   由 `disable_bundled` 位控制）。
        // - `disable_bundled`：宿主配置的真实值（F12 的
        //   `load_disable_bundled_skills` 语义来源），不再是波次域隔离常量。
        // - 缺根（目录不存在）交给 provider 既有语义处理（缺失目录 = 空批）。
        // - agents 面不装（W5）；指令面（`peri-instruction://`）随 provider 装配
        //   被动上线（W4a 已记录，消费切换归 W5）。
        let workspace_resources = super::workspace_resources::workspace_resources_input(
            &cwd,
            plugins,
            configuration
                .config
                .config
                .claude_md_excludes
                .as_deref()
                .unwrap_or_default(),
        );
        // A24 关闭集：从**同一份 frozen snapshot** 派生（设计 §2.5：禁止回退当轮 config；
        // fork 复用 source 的 frozen 字节时两者可能不同）。它是订阅建立门的唯一输入，
        // 不改变 pool 级就绪与面板连接状态（ARC-CAPABILITY-CLOSURE-001）。
        //
        // W4b 收口：同一份 disabled 集合同批派生**宿主技能面关闭位**
        //（`"SkillsMiddleware" ∈ disabled`，与链槽装配的跳过判据同一字面量，见
        // `peri_middlewares/src/assembly.rs` 的 `ChainSlot::Skills` 分支）——链槽关闭
        // 时发现管线的 `core:{skill}` 裸名投影同批撤下（不得留幽灵路由）；workspace
        // 实例本身与 `{server}:{skill}` 面不受影响。两个位一次派生、随 builtin 实例
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
            // 会话级装配：这里不是部署 owner，拿不到也不持有全局关闭权。
            session_store_shutdown: None,
            cwd: cwd.clone(),
            bare: source.bare,
            drive_cron_tick: source.drive_cron_tick,
            workspace_input: Some(workspace_input.clone()),
            workspace_resources: Some(workspace_resources),
            builtin_closed,
            skills_face_closed,
            prepared_plugins: Some(plugins.clone()),
        };
        let activation = tokio_util::sync::CancellationToken::new();
        let mut cfg = assemble::assemble_server_config_with_mcp_profile(
            input,
            source.mcp_profile.clone(),
            true,
            Some(activation.clone()),
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
            workspace_input,
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
        use peri_acp_types::ports::McpPoolPort as _;
        let Some(pool) = self.cfg.mcp_pool.as_ref() else {
            return Ok(Default::default());
        };
        let Some(pool) = pool
            .as_any()
            .downcast_ref::<peri_middlewares::mcp::McpClientPool>()
        else {
            return Ok(Default::default());
        };
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Some(handle) = pool.get_client("workspace") {
                if !matches!(
                    handle.status,
                    peri_middlewares::mcp::ClientStatus::Connected
                ) {
                    return Ok(Default::default());
                }
                return pool
                    .read_builtin_workspace_meta(enabled_sections)
                    .await
                    .map_err(|error| AcpError::new(-32603, error));
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
            if tokio::time::Instant::now() >= deadline {
                return Ok(Default::default());
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    /// 本会话环境的 per-session 后台任务管理器（AW3-11 第一成员）。
    ///
    /// 产生点在本模块的装配（`assemble` 开头，早于 builtin 上下文构造），随后**同一
    /// `Arc`** 经 `WorkspaceInstanceInput` 送进 builtin `workspace` 实例，并由三条会话
    /// 路径交给 `SessionManager::ensure_session_with_task_manager` 登记为
    /// `AcpSession::task_manager`——三处是同一份（`Arc::ptr_eq` 可观察）。
    pub(crate) fn task_manager(&self) -> Arc<dyn peri_acp_types::tasks::TaskManager> {
        Arc::clone(&self.task_manager)
    }

    /// 装配面送进 builtin `workspace` 实例上下文的那份输入（AW3-11 两名成员）。
    ///
    /// 恒为 `Some`（本环境的构造前提就是 `workspace_assembly` 存在）；返回 `Option`
    /// 只为对齐上下文里 `workspace: Option<WorkspaceInstanceInput>` 的形状。
    ///
    /// `cfg(test)`：这是**测试观察面**（验证「送进上下文的那份 == 环境持有的那份 ==
    /// 会话持有的那份」），生产代码只经 [`Self::task_manager`] 取 manager。
    #[cfg(test)]
    pub(crate) fn workspace_input(&self) -> Option<&peri_mcp_workspace::WorkspaceInstanceInput> {
        Some(&self.workspace_input)
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
            match tokio::time::timeout(std::time::Duration::from_secs(5), handle).await {
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

pub(crate) fn workspace_error(error: impl Into<anyhow::Error>) -> AcpError {
    let error = error.into();
    let mut response = AcpError::new(-32010, error.to_string());
    if let Some(data) = error
        .downcast_ref::<WorkspaceError>()
        .and_then(WorkspaceErrorData::from_workspace_error)
    {
        response.data = Some(serde_json::to_value(data).expect("workspace error data serializes"));
    }
    response
}

/// 门面行为失败 → ACP 错误：本机 workspace 语义保留既有载荷（含恢复确认数据），
/// 其余按行为失败上报，不把失败伪装成 workspace 问题。
pub(crate) fn resource_error(error: SessionResourceError) -> AcpError {
    match error.workspace_error() {
        Some(workspace) => workspace_error(workspace.clone()),
        None => AcpError::new(-32010, error.to_string()),
    }
}

pub(crate) fn require_owner(state: &SessionState) -> Result<(), AcpError> {
    if state.closing {
        return Err(AcpError::new(-32010, "Session is closing"));
    }
    if state.execution_owner.is_none() {
        return Err(workspace_error(WorkspaceError::ExecutionLeaseRequired));
    }
    Ok(())
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
    expected: Option<&str>,
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
    if let Some(expected) = expected {
        expect_directory(expected, &workspace).await?;
    }
    Ok(workspace)
}

/// 调用方给出的执行目录必须与绑定指向同一目录。
///
/// 绑定的 cwd 在登记时已规范化，因此比对规范化后的路径即可：同一目录的等价路径
/// （符号链接、`/var` 与 `/private/var`）仍然一致，别的目录与绑定不符。比较不再
/// 解析登记——解析会为比较再跑一轮完整发现，也顺带登记一个与本次执行无关的目录。
pub(crate) async fn expect_directory(
    expected: &str,
    workspace: &ResolvedWorkspace,
) -> Result<(), AcpError> {
    let expected = tokio::fs::canonicalize(expected)
        .await
        .map_err(|_| workspace_error(WorkspaceError::Unavailable))?;
    if expected != workspace.cwd {
        return Err(workspace_error(WorkspaceError::ExecutionBindingMismatch));
    }
    Ok(())
}

/// 未加载会话的短时执行准入：按保存的 cwd 解析绑定目录后取得 owner。
///
/// 用于 `session/rename`、`session/delete` 这类「会话不在本进程会话表里」的显式
/// 生命周期行为：所有权是改标题/删除的前提，但不需要装配执行环境（不启动
/// MCP/LSP/hooks）。他处持有时原样拒绝，不降级、不抢占。
pub(crate) async fn acquire_transient_owner(
    cfg: &AcpServerConfig,
    session_id: &str,
) -> Result<Arc<dyn SessionExecutionLease>, AcpError> {
    let resources = &cfg.session_resources;
    let id = session_id.to_owned();
    let meta = resources
        .load_session_meta(&id)
        .await
        .map_err(resource_error)?;
    let workspace = resources
        .resolve_workspace(Path::new(&meta.cwd))
        .await
        .map_err(resource_error)?;
    resources
        .acquire_execution(&id, &workspace)
        .await
        .map_err(resource_error)
}

/// 一次加载准入的结果：绑定与执行目录已复核，执行所有权可能不可得。
///
/// 所有权不可得（`ExecutionBusy` / `RecoveryRequired` / `ExecutionLeaseRequired`）时
/// 仍返回已复核的 `workspace`，由调用方决定是降级为只读会话还是原样上报——绑定复核
/// 已经跑过一次完整发现，降级路径不能为了拿到同一个 `workspace` 再跑一轮。
pub(crate) struct LoadAdmission {
    pub(crate) workspace: ResolvedWorkspace,
    pub(crate) execution: ExecutionAdmission,
}

/// 执行所有权判定结果：取得所有权，或确认所有权不可得但仍可只读准入。
pub(crate) enum ExecutionAdmission {
    /// 本次准入持有执行所有权。
    Owned(Arc<dyn SessionExecutionLease>),
    /// 执行所有权不可得，但会话历史仍可只读访问；携带原因供调用方上报与降级。
    ///
    /// `RecoveryRequired` 对协商了 `peri.sessionRecoveryV1` 的连接是终局（它自己有确认
    /// 交互）；没有确认交互的连接会先按观测到的精确代际解除 dirty 再重取一次，因此这里
    /// 只承载重取之后仍未取得所有权的原因（见 [`acquire_lease_with_recovery`]）。
    Unavailable(ReadOnlyAdmission),
}

/// 加载/恢复/克隆准入的第一步：复核执行目录并取得执行所有权。
///
/// 这是准入入口，因此做完整复核；同一次准入内再取一次（如 `session/fork` 在
/// `prepare_existing` 之后）用 [`reacquire_for_load`]，不重复跑 Git 发现。
///
/// 未协商 `peri.sessionRecoveryV1` 的连接遇到 dirty 代际时不用停在只读：它没有确认
/// 交互（`peri/session_reset_dirty` 只对协商过该能力的连接开放），在这里由 host
/// 直接解除该精确代际后取得所有权。
pub(crate) async fn acquire_for_load(
    cfg: &AcpServerConfig,
    sessions: &std::collections::HashMap<String, SessionState>,
    session_id: &str,
    expected: Option<&str>,
) -> Result<LoadAdmission, AcpError> {
    acquire_for_load_with(cfg, sessions, session_id, expected, BindingCheck::Full).await
}

/// 同一次准入内再次取得执行目录与所有权：只复核已记录证据。
///
/// 这里不接受只读降级：调用方（`session/fork`）必须有执行所有权才能继续。
pub(crate) async fn reacquire_for_load(
    cfg: &AcpServerConfig,
    sessions: &std::collections::HashMap<String, SessionState>,
    session_id: &str,
    expected: Option<&str>,
) -> Result<(ResolvedWorkspace, Arc<dyn SessionExecutionLease>), AcpError> {
    let admission =
        acquire_for_load_with(cfg, sessions, session_id, expected, BindingCheck::Recorded).await?;
    match admission.execution {
        ExecutionAdmission::Owned(owner) => Ok((admission.workspace, owner)),
        ExecutionAdmission::Unavailable(reason) => Err(read_only_error(reason)),
    }
}

/// 只读降级原因还原为错误：不接受降级的调用方（如 `session/fork`）按原语义上报。
///
/// 只有门面明确给出的「所有权不可得」原因才进入这里；其他失败（IO、绑定复核、
/// schema 不支持）本来就不降级，避免把「读不了」伪装成「可以只读进入」。
pub(crate) fn read_only_error(reason: ReadOnlyAdmission) -> AcpError {
    let error = match reason {
        ReadOnlyAdmission::ExecutionBusy => WorkspaceError::ExecutionBusy,
        ReadOnlyAdmission::RecoveryRequired(details) => WorkspaceError::RecoveryRequired(details),
        ReadOnlyAdmission::ExecutionLeaseRequired => WorkspaceError::ExecutionLeaseRequired,
    };
    workspace_error(error)
}

async fn acquire_for_load_with(
    cfg: &AcpServerConfig,
    sessions: &std::collections::HashMap<String, SessionState>,
    session_id: &str,
    expected: Option<&str>,
    check: BindingCheck,
) -> Result<LoadAdmission, AcpError> {
    // 绑定与执行目录不符直接失败：只读降级只针对执行所有权不可得，不掩盖身份问题。
    let workspace = check_expected(cfg, session_id, expected, check).await?;
    // 已持有所有权直接复用；只读会话与冷会话都重新尝试取得——他处释放后再次准入
    // 才有机会升级回可执行，而不是一次只读就永久只读。
    let held = match sessions.get(session_id) {
        Some(state) if state.closing => return Err(AcpError::new(-32010, "Session is closing")),
        Some(state) => state.execution_owner.clone(),
        None => None,
    };
    let (owner, acquired_here) = match held {
        Some(owner) => (owner, false),
        None => match acquire_lease_with_recovery(cfg, session_id, &workspace).await? {
            ExecutionAdmission::Owned(owner) => (owner, true),
            ExecutionAdmission::Unavailable(reason) => {
                return Ok(LoadAdmission {
                    workspace,
                    execution: ExecutionAdmission::Unavailable(reason),
                });
            }
        },
    };
    // 取得所有权后复核的是同一件事：重新发现的证据与首次复核相同，这里只复核已记录证据。
    let workspace = match check_expected(cfg, session_id, expected, BindingCheck::Recorded).await {
        Ok(workspace) => workspace,
        Err(error) => {
            // 本次准入自己取得的代际必须收尾，否则会留下既无持有者又未标 clean 的运行。
            if acquired_here {
                let _ = owner.mark_clean().await;
            }
            return Err(error);
        }
    };
    if let Some(state) = sessions.get(session_id) {
        if Path::new(&state.cwd) != workspace.cwd {
            return Err(workspace_error(WorkspaceError::ExecutionBindingMismatch));
        }
    }
    Ok(LoadAdmission {
        workspace,
        execution: ExecutionAdmission::Owned(owner),
    })
}

/// 取执行所有权：先直接取一次，dirty 代际且本连接没有确认交互时由 host 解除后重取。
///
/// 解除只发生在观测到的精确 `(thread_id, generation)` 上；解除失败（例如并发的
/// CAS 错配）不升级为准入失败，仍按第一次尝试的只读原因收敛。
async fn acquire_lease_with_recovery(
    cfg: &AcpServerConfig,
    session_id: &str,
    workspace: &ResolvedWorkspace,
) -> Result<ExecutionAdmission, AcpError> {
    let first = try_acquire_lease(cfg, session_id, workspace).await?;
    let ExecutionAdmission::Unavailable(ReadOnlyAdmission::RecoveryRequired(ref target)) = first
    else {
        return Ok(first);
    };
    // 协商过恢复能力的客户端自己确认并调用 `peri/session_reset_dirty`：dirty 原因
    // 就是它要看的信号，host 不替它解除。
    if cfg.session_manager.negotiated_caps().session_recovery_v1 {
        return Ok(first);
    }
    if let Err(error) = clear_dirty_generation(cfg, session_id, target).await {
        tracing::warn!(
            session_id,
            thread_id = %target.thread_id,
            generation = target.generation,
            error = %error.message,
            "dirty generation was not cleared; admitting read-only"
        );
        return Ok(first);
    }
    let acquired = try_acquire_lease(cfg, session_id, workspace).await;
    // 重取失败（存储故障等不可降级的原因）此前到不了这里——那时这一步只会只读返回；
    // 判定不变（代际已解除，没有可收敛的只读原因），只补上诊断线索。
    if let Err(error) = &acquired {
        tracing::warn!(
            session_id,
            thread_id = %target.thread_id,
            generation = target.generation,
            error = %error.message,
            "dirty generation was cleared but ownership was not re-acquired"
        );
    }
    acquired
}

/// 取一次执行所有权：只有「所有权不可得」的三类原因才按只读收敛，其他失败原样上报。
async fn try_acquire_lease(
    cfg: &AcpServerConfig,
    session_id: &str,
    workspace: &ResolvedWorkspace,
) -> Result<ExecutionAdmission, AcpError> {
    match cfg
        .session_resources
        .acquire_execution(&session_id.to_owned(), workspace)
        .await
    {
        Ok(owner) => Ok(ExecutionAdmission::Owned(owner)),
        Err(error) => match error.read_only_admission() {
            Some(reason) => Ok(ExecutionAdmission::Unavailable(reason)),
            None => Err(resource_error(error)),
        },
    }
}

/// 未协商恢复能力的连接遇到 dirty 代际：host 直接解除该精确代际。
///
/// 这是拿风险换可用的裁决：停在只读等于 dirty 会话在这类客户端上永远不可用，而它
/// 没有确认交互可以承担这个选择。解除仍只针对观测到的精确 `(thread_id, generation)`
/// （稳定 OS 锁与代次 CAS 语义不变），旧执行是否已收尾依旧未知——解除不构成
/// 「上一代已正常结束」的证明，只是把进入放在已知风险之上。
async fn clear_dirty_generation(
    cfg: &AcpServerConfig,
    session_id: &str,
    target: &RecoveryRequiredDetails,
) -> Result<(), AcpError> {
    cfg.session_resources
        .reset_dirty_execution(&peri_acp_types::workspace::ResetDirtyRequest {
            target: target.clone(),
            // host 自动解除是「拿风险换可用」的既有裁决；门面要求调用方显式承担。
            accept_risk: true,
        })
        .await
        .map_err(resource_error)?;
    tracing::warn!(
        session_id,
        thread_id = %target.thread_id,
        generation = target.generation,
        "cleared dirty execution generation for a client without recovery interaction"
    );
    Ok(())
}
