use super::*;

/// 宿主夹具：真门面（SQLite） + 已绑定/frozen 的父会话 + 生产 `SubagentHost`。
///
/// 子链装配从父会话的 `SubagentHost` 读取资源门面 / 任务通道 / 父线程身份，因此
/// 用例必须有一个与生产同形的父会话，而不是让工具字段兜底。`label` 只用于父会话
/// title（夹具标识），不参与任何生产判定。
pub(crate) struct HostFixture {
    pub(crate) _dir: Option<tempfile::TempDir>,
    pub(crate) fixture: SessionFixture,
    pub(crate) parent_id: ThreadId,
    pub(crate) cwd: String,
    parent_session: std::sync::Arc<peri_agent::session::Session>,
}

impl HostFixture {
    /// 在既有工作区目录上建立宿主夹具，并在装配前定制父 host（write-once：
    /// 通道 / langfuse bridge 等必须在装配时给定）。
    pub(crate) async fn open_in_with_host(
        dir: &std::path::Path,
        label: &str,
        configure: impl FnOnce(&mut peri_agent::session::subagent::SubagentHost),
    ) -> Self {
        let mut host = HostFixture::open_in(dir, label).await;
        let mut sub_host = host
            .parent_session
            .subagent_host()
            .as_deref()
            .cloned()
            .unwrap_or_default();
        configure(&mut sub_host);
        host.parent_session =
            rebuild_with_host(&host.cwd, &host.parent_id, &host.fixture, sub_host);
        host
    }

    /// 带调用方后台通道的宿主（父 host 的 TaskManager/bg 事件发送端）。
    ///
    /// `set_subagent_host` 是 write-once：通道必须在父 session 装配时给定，
    /// 不能事后替换（`use_background_channels` 只对未装配的 session 有效）。
    pub(crate) async fn open_with_background(
        label: &str,
        task_manager: std::sync::Arc<peri_agent::agent::async_tasks::TaskManager>,
        bg_event_sender: tokio::sync::mpsc::UnboundedSender<
            peri_agent::agent::events::ExecutorEvent,
        >,
    ) -> Self {
        let mut host = Self::open(label).await;
        let mut session = Arc::clone(&host.parent_session);
        let mut sub_host = session
            .subagent_host()
            .as_deref()
            .cloned()
            .unwrap_or_default();
        sub_host.task_manager = Some(task_manager);
        sub_host.bg_event_sender = Some(bg_event_sender);
        session = rebuild_with_host(&host.cwd, &host.parent_id, &host.fixture, sub_host);
        host.parent_session = session;
        host
    }

    pub(crate) async fn open_in_with_background(
        dir: &std::path::Path,
        label: &str,
        task_manager: std::sync::Arc<peri_agent::agent::async_tasks::TaskManager>,
        bg_event_sender: tokio::sync::mpsc::UnboundedSender<
            peri_agent::agent::events::ExecutorEvent,
        >,
    ) -> Self {
        let mut host = Self::open_in(dir, label).await;
        let mut sub_host = host
            .parent_session
            .subagent_host()
            .as_deref()
            .cloned()
            .unwrap_or_default();
        sub_host.task_manager = Some(task_manager);
        sub_host.bg_event_sender = Some(bg_event_sender);
        host.parent_session =
            rebuild_with_host(&host.cwd, &host.parent_id, &host.fixture, sub_host);
        host
    }

    /// 临时目录宿主（目录由夹具持有）。`label` 只写入父会话 title。
    pub(crate) async fn open(label: &str) -> Self {
        let dir = tempdir().unwrap();
        let fixture = SessionFixture::open_in(dir.path()).await;
        Self::assemble(fixture, label, Some(dir)).await
    }

    /// 在调用方已有的工作区目录上建立宿主夹具（agent 定义查找路径与调用 cwd 一致，
    /// 目录生命周期由调用方持有）。
    pub(crate) async fn open_in(dir: &std::path::Path, label: &str) -> Self {
        let fixture = SessionFixture::open_in(dir).await;
        Self::assemble(fixture, label, None).await
    }

    /// 父会话句柄 + 生产 host 装配（`open` / `open_in` 共用）。
    async fn assemble(
        fixture: SessionFixture,
        label: &str,
        dir: Option<tempfile::TempDir>,
    ) -> Self {
        let cwd = fixture.workspace_cwd();
        let mut meta = ThreadMeta::new_at(cwd.clone(), peri_time::now_wall());
        meta.title = Some(label.to_string());
        let parent_id = fixture.create_thread(meta).await.expect("建立父会话失败");
        let parent_session = build_parent_session(&cwd, &parent_id, &fixture);
        Self {
            _dir: dir,
            fixture,
            parent_id,
            cwd,
            parent_session,
        }
    }

    /// 把真门面、父会话（含 SubagentHost）、SDK admission 端口装到工具上。
    ///
    /// 生产子链的宿主来自父 session 的 `SubagentHost`；夹具的父 session 在
    /// `open/open_in` 时建立一次并挂生产 host，多个工具共享同一父会话 token
    /// （取消语义与生产一致）。
    pub(crate) fn bind(&self, tool: SubAgentTool) -> SubAgentTool {
        let mut tool = tool
            .with_session_resources(self.fixture.facade())
            .with_parent_thread_id(self.parent_id.clone())
            .with_parent_session(std::sync::Arc::clone(&self.parent_session));
        // 工具默认 cwd 与父会话绑定工作区一致，避免 ExecutionBindingMismatch。
        tool.parent_cwd = self.cwd.clone();
        tool
    }

    /// 让父会话 host 使用调用方的后台通道（**仅当父 session 尚未装配 host**；
    /// 已装配时 write-once 语义会忽略，改用 `open_with_background`）。
    ///
    /// 保留该方法用于尚未装配 host 的自有 session（如 resume 用例的 owning parent）。
    #[allow(dead_code)]
    ///
    /// 后台子链的 TaskManager/bg 事件来自父 session 的 `SubagentHost`，测试要观察
    /// 注册与事件就必须把同一通道装到父 host 上（工具字段级设置会被父 host 覆盖）。
    pub(crate) fn use_background_channels(
        &self,
        task_manager: std::sync::Arc<peri_agent::agent::async_tasks::TaskManager>,
        bg_event_sender: tokio::sync::mpsc::UnboundedSender<
            peri_agent::agent::events::ExecutorEvent,
        >,
    ) {
        let mut host = self
            .parent_session
            .subagent_host()
            .as_deref()
            .cloned()
            .unwrap_or_default();
        host.task_manager = Some(task_manager);
        host.bg_event_sender = Some(bg_event_sender);
        self.parent_session.set_subagent_host(host);
    }

    /// 取父会话当前 host 副本（用于在装配后重建宿主时定制字段）。
    pub(crate) fn parent_session_host(
        &self,
    ) -> Option<peri_agent::session::subagent::SubagentHost> {
        self.parent_session.subagent_host().as_deref().cloned()
    }

    /// 用给定 host 重建父会话（write-once host 的装配后定制入口）。
    pub(crate) fn with_rebuilt_host(
        mut self,
        host: peri_agent::session::subagent::SubagentHost,
    ) -> Self {
        self.parent_session = rebuild_with_host(&self.cwd, &self.parent_id, &self.fixture, host);
        self
    }

    /// 取消父会话 token（Cascade 子链随父取消；spawn/resume 的取消来源）。
    pub(crate) fn cancel_parent(&self) {
        self.parent_session.config().cancel_token.cancel();
    }

    /// 带本次工具调用身份的调用上下文（父侧 provenance 来源）。
    pub(crate) fn context<'a>(
        &'a self,
        messages: &'a [BaseMessage],
    ) -> peri_agent::tools::ToolContext<'a> {
        self.context_with(messages, self.fresh_tool_call_id("context"))
    }

    /// 指定 tool-call 身份的调用上下文（同一父会话内多次委派必须使用各自的身份——
    /// 否则子事件无法把发起项与父模型卡片对上）。
    pub(crate) fn context_with<'a>(
        &'a self,
        messages: &'a [BaseMessage],
        tool_call_id: String,
    ) -> peri_agent::tools::ToolContext<'a> {
        let mut ctx = peri_agent::tools::ToolContext::new(messages, &self.cwd);
        ctx.invocation_id = Some(tool_call_id.clone());
        ctx.tool_call_id = Some(tool_call_id);
        ctx
    }

    /// 给调用方自有的父会话挂生产 `SubagentHost`（资源门面 / admission 端口 /
    /// 任务通道 / 父线程 id）。resume 等路径必须以 owning parent session 为父，
    /// 不能换成夹具自己的 session；此时用本方法注入同一份耐久承载。
    pub(crate) fn attach_session_host(
        &self,
        session: &std::sync::Arc<peri_agent::session::Session>,
    ) {
        use peri_agent::session::subagent::SubagentHost;
        let mut host = SubagentHost {
            session_resources: Some(self.fixture.facade()),
            task_manager: Some(Arc::new(peri_agent::agent::async_tasks::TaskManager::new())),
            ..Default::default()
        };
        host.parent_thread_id = session
            .store()
            .thread_id
            .clone()
            .or_else(|| Some(self.parent_id.clone()));
        session.set_subagent_host(host);
    }

    /// 新一次委派的 tool-call 身份（多次委派用例；id 唯一）。
    pub(crate) fn fresh_tool_call_id(&self, label: &str) -> String {
        format!("fixture-tool-call:{label}:{}", uuid::Uuid::now_v7())
    }
}

/// 用给定 SubagentHost 重建 session（Write-once host 需要在装配前定稿通道）。
fn rebuild_with_host(
    cwd: &str,
    parent_id: &str,
    fixture: &SessionFixture,
    mut host: peri_agent::session::subagent::SubagentHost,
) -> std::sync::Arc<peri_agent::session::Session> {
    host.session_resources = Some(fixture.facade());
    if host.task_manager.is_none() {
        host.task_manager = Some(Arc::new(peri_agent::agent::async_tasks::TaskManager::new()));
    }
    host.parent_thread_id = Some(parent_id.to_string());
    let session = peri_agent::session::Session::new(
        std::sync::Arc::from(cwd),
        peri_agent::session::FrozenContext::builder().build(),
        Some(parent_id.to_string()),
    );
    session.set_subagent_host(host);
    session
}

/// 构造夹具父 session：cwd/父线程 id + 生产 SubagentHost（资源/端口/任务通道）。
fn build_parent_session(
    cwd: &str,
    parent_id: &str,
    fixture: &SessionFixture,
) -> std::sync::Arc<peri_agent::session::Session> {
    use peri_agent::session::subagent::SubagentHost;
    let session = peri_agent::session::Session::new(
        std::sync::Arc::from(cwd),
        peri_agent::session::FrozenContext::builder().build(),
        Some(parent_id.to_string()),
    );
    let mut host = SubagentHost {
        session_resources: Some(fixture.facade()),
        task_manager: Some(Arc::new(peri_agent::agent::async_tasks::TaskManager::new())),
        ..Default::default()
    };
    host.parent_thread_id = Some(parent_id.to_string());
    session.set_subagent_host(host);
    session
}
