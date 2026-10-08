use std::sync::Arc;

use parking_lot::RwLock;
use peri_acp_types::identity::AgentId;
use peri_acp_types::session_resources::{
    FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionMetaPatch, SessionResources,
};
use peri_acp_types::store::PersistedPayload;
use peri_acp_types::thread::AgentStatus;
use peri_acp_types::workspace::{ResolvedWorkspace, SessionBinding, SESSION_BINDING_VERSION};
use peri_agent::{
    agent::{
        events::ExecutorEvent,
        events_v2::ObserveEvent,
        react::{ReactLLM, Reasoning, StreamingContext},
        AgentCancellationToken,
    },
    messages::BaseMessage,
    thread::{ThreadId, ThreadMeta},
    tools::BaseTool,
};
use tempfile::tempdir;

use super::*;

// Mock LLM: returns final answer directly
struct EchoLLM;

#[async_trait::async_trait]
impl ReactLLM for EchoLLM {
    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        _tools: &[&dyn BaseTool],
        _streaming: Option<StreamingContext>,
    ) -> peri_agent::error::AgentResult<Reasoning> {
        let last = messages.last().map(|m| m.content()).unwrap_or_default();
        Ok(Reasoning::with_answer("", format!("echo: {}", last)))
    }
}

fn make_tool(name: &'static str) -> Arc<dyn BaseTool> {
    struct DummyTool(&'static str);

    #[async_trait::async_trait]
    impl BaseTool for DummyTool {
        fn name(&self) -> &str {
            self.0
        }
        fn description(&self) -> &str {
            "dummy"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        fn is_direct(&self) -> bool {
            true
        }
        async fn invoke(
            &self,
            _input: serde_json::Value,
            _ctx: peri_agent::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Ok(format!("{} result", self.0))
        }
    }

    Arc::new(DummyTool(name))
}

fn make_subagent_tool(parent_tools: Vec<Arc<dyn BaseTool>>) -> SubAgentTool {
    SubAgentTool::new(
        Arc::new(parent_tools),
        None,
        Arc::new(|_: Option<&str>| Box::new(EchoLLM) as Box<dyn ReactLLM + Send + Sync>),
        "/tmp".to_string(),
    )
}

/// W4b（J5）测试夹具：把会话级 MCP skill registry 接到子链装配器上
/// （子代理 `skills:` 预载的唯一来源；未接线 ⇒ 缺口，不回落磁盘）。
///
/// 条目用 legacy 形状（`content: Some(..)`、无 `resources[]` 绑定）：统一
/// activation 在该形状下使用发现期正文——本夹具因此不需要真实 peer。
pub(crate) fn with_skill_registry(
    tool: SubAgentTool,
    server: &str,
    skills: &[(&str, &str)],
) -> SubAgentTool {
    use peri_acp_types::mcp_skills::{HandleToken, McpSkillRegistry};
    use peri_acp_types::skills::{SkillMetadata, SkillOrigin, SkillSource};
    let reg = std::sync::Arc::new(McpSkillRegistry::new());
    let handle: HandleToken = std::sync::Arc::new(server.to_string());
    reg.mark_discovery_started(server, handle.clone());
    reg.mark_discovery_completed(
        server,
        handle,
        skills
            .iter()
            .map(|(name, body)| SkillMetadata {
                name: peri_acp_types::mcp_skills::mcp_skill_name(server, name),
                aliases: Vec::new(),
                description: format!("MCP skill {name}"),
                path: std::path::PathBuf::new(),
                source: SkillSource::Mcp,
                plugin_name: None,
                origin: Some(SkillOrigin::Mcp {
                    server: server.to_string(),
                    uri: format!("skill://{server}/{name}/SKILL.md"),
                }),
                content: Some((*body).to_string()),
                resources: Vec::new(),
                frontmatter: None,
            })
            .collect(),
    );
    tool.with_mcp_skills(Some(reg))
}

/// mock LangfuseBridgeLike：记录 forwarder 转发的全部 ObserveEvent
struct RecordingBridge {
    observes: Arc<std::sync::Mutex<Vec<ObserveEvent>>>,
}

impl peri_agent::agent::LangfuseBridgeLike for RecordingBridge {
    fn process_render_event(&self, _ev: &peri_agent::agent::events_v2::RenderEvent) {}

    fn process_observe_event(&self, ev: &ObserveEvent) {
        self.observes.lock().unwrap().push(ev.clone());
    }
}

/// 断言 Start/Stop 恰好一次且字段配对（agent_name / is_background / 父子 id 一致）
fn assert_start_stop_pair(evs: &[ObserveEvent], expected_name: &str, expected_bg: bool) {
    let starts: Vec<&ObserveEvent> = evs
        .iter()
        .filter(|e| matches!(e, ObserveEvent::SubagentStart { .. }))
        .collect();
    let stops: Vec<&ObserveEvent> = evs
        .iter()
        .filter(|e| matches!(e, ObserveEvent::SubagentStop { .. }))
        .collect();
    assert_eq!(starts.len(), 1, "SubagentStart 必须恰好一次: {:?}", evs);
    assert_eq!(stops.len(), 1, "SubagentStop 必须恰好一次: {:?}", evs);

    let (start_parent, start_child, start_name, start_bg) = match starts[0] {
        ObserveEvent::SubagentStart {
            agent_id,
            child_agent_id,
            agent_name,
            is_background,
            ..
        } => (agent_id, child_agent_id, agent_name, is_background),
        _ => unreachable!(),
    };
    let (stop_parent, stop_child, stop_name, stop_result, stop_err) = match stops[0] {
        ObserveEvent::SubagentStop {
            agent_id,
            child_agent_id,
            agent_name,
            result,
            is_error,
            ..
        } => (agent_id, child_agent_id, agent_name, result, is_error),
        _ => unreachable!(),
    };
    assert_eq!(start_name.as_str(), expected_name, "agent_name 不符");
    assert_eq!(*start_bg, expected_bg, "is_background 不符");
    assert_eq!(
        start_parent, stop_parent,
        "Start/Stop 父 agent_id 必须一致（同一次调用）"
    );
    assert_eq!(
        start_child, stop_child,
        "Start/Stop child_agent_id 必须配对（同一 subagent）"
    );
    assert_eq!(stop_name.as_str(), expected_name, "Stop agent_name 不符");
    assert!(!stop_result.is_empty() || *stop_err, "Stop 必须携带 result");
    assert!(
        uuid::Uuid::parse_str(&start_child.to_string()).is_ok(),
        "child_agent_id 必须是可解析 UUID（= child_thread_id）"
    );
}

fn write_test_agent(dir: &tempfile::TempDir) {
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("test-agent.md"),
        "---\nname: test-agent\ndescription: A test agent\n---\n\nYou are a test agent.\n",
    )
    .unwrap();
}

thread_local! {
    /// 夹具持有槽：`AgentFaceFixture` 自带 runtime 与线路，随测试线程存活
    /// （测试线程结束时按序关闭 service/supervisor）。
    static AGENT_FACE_FIXTURES: std::cell::RefCell<Vec<crate::mcp::agent_face_fixture::AgentFaceFixture>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// W5 夹具：为 `dir` 的真实 agent 资源面接线（**必须在写入定义文件之后调用**）。
///
/// 目录投影（`resources/list`）与正文激活（`resources/read`）都走生产链路
/// （`AgentFaceFixture`：真实 handler + 生产池句柄），不使用测试替身。
pub(crate) async fn with_agent_face(tool: SubAgentTool, dir: &std::path::Path) -> SubAgentTool {
    let fixture = crate::mcp::agent_face_fixture::AgentFaceFixture::connect(dir).await;
    let registry = std::sync::Arc::clone(&fixture.registry);
    AGENT_FACE_FIXTURES.with(|fixtures| fixtures.borrow_mut().push(fixture));
    tool.with_mcp_agents(Some(registry), None)
}

fn tool_with_built_ins_disabled(cwd: &str) -> SubAgentTool {
    let state = peri_acp_types::meta_harness::MetaHarnessState {
        built_in_subagents_enabled: false,
        ..Default::default()
    };
    let parent = peri_agent::session::Session::new(
        Arc::from(cwd),
        peri_agent::session::FrozenContext::builder()
            .meta_harness(state)
            .build(),
        None,
    );
    make_subagent_tool(Vec::new()).with_parent_session(parent)
}

/// W5 测试夹具：把会话级 MCP Agent registry 接到 SubAgentTool 上。
///
/// 用合成句柄（builtin `workspace` 实例的 `resources/list` 投影形状），不需要
/// 真实 wire——目录/来源/开关判定不读正文；正文激活由 provider 与 ACP 端到端
/// 用例覆盖。
pub(crate) fn with_agent_catalog(
    tool: SubAgentTool,
    entries: &[(
        peri_acp_types::workspace_resources::ResourceScope,
        &str,
        &str,
    )],
) -> SubAgentTool {
    use peri_acp_types::plugin::ConfigSource;
    use peri_acp_types::workspace_resources::{agent_uri, META_KEY_FRONTMATTER, META_KEY_SCOPE};
    use rmcp::model::{MetaObject, Resource};

    let mut resources = Vec::new();
    for (scope, id, frontmatter) in entries {
        let uri = agent_uri(*scope, None, id).expect("agent uri");
        let mut meta = serde_json::Map::new();
        meta.insert(
            META_KEY_SCOPE.to_string(),
            serde_json::Value::String(scope.as_str().to_string()),
        );
        meta.insert(
            META_KEY_FRONTMATTER.to_string(),
            serde_json::from_str(frontmatter).expect("frontmatter json"),
        );
        resources.push(
            Resource::new(uri, *id)
                .with_mime_type("text/markdown")
                .with_meta(MetaObject(meta)),
        );
    }
    let pool = Arc::new(crate::mcp::McpClientPool::new_pending());
    pool.clients.write().insert(
        "workspace".to_string(),
        Arc::new(crate::mcp::McpClientHandle {
            name: "workspace".to_string(),
            version: None,
            cache_version: None,
            peer: None,
            tools: Vec::new(),
            resources,
            status: crate::mcp::ClientStatus::Connected,
            oauth_status: Default::default(),
            source: Some(ConfigSource::Builtin {
                instance: "workspace".to_string(),
            }),
            url: None,
            skills_capable: false,
        }),
    );
    let registry = Arc::new(crate::mcp::McpAgentRegistry::new(pool));
    tool.with_mcp_agents(Some(registry), None)
}

const LOCAL_AGENT_FM: &str =
    r#"{"name":"local-agent","description":"Local agent","model":"sonnet"}"#;
const BUILTIN_AGENT_FM: &str =
    r#"{"name":"explorer","description":"Builtin explorer","model":"haiku"}"#;

#[tokio::test]
async fn agent_definition_without_a_resource_face_reports_a_gap() {
    // W5：Agent 定义只从 MCP 资源面读取；面未装配 ⇒ 明确缺口报告，
    // **不回落磁盘**（X4/J5）。
    let tool = tool_with_built_ins_disabled("/nonexistent");
    let error = tool.load_agent_def("coder").await.unwrap_err();
    assert!(
        error.contains("unavailable"),
        "必须报告面不可用而不是读盘：{error}"
    );
}

#[tokio::test]
async fn built_in_policy_is_enforced_by_the_registry_catalog() {
    use peri_acp_types::workspace_resources::ResourceScope;
    // 只有 builtin 来源的定义：关闭 built-in policy 时不可加载；resume 放开。
    let dir = tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap();
    let tool = with_agent_catalog(
        tool_with_built_ins_disabled(cwd),
        &[(ResourceScope::Builtin, "coder", BUILTIN_AGENT_FM)],
    );
    let error = tool.load_agent_def_for_resume("coder").await.unwrap_err();
    assert!(
        error.contains("cannot find agent definition 'coder'") || error.contains("not connected"),
        "resume 允许 builtin 来源参与解析（正文激活需真实 peer）：{error}"
    );
    // 目录层：关闭位下 builtin 不进候选（建议列表不含 explorer）。
    let suggestions = tool.agent_error_with_suggestions("Error", Some("explor"));
    assert!(!suggestions.contains("explorer"), "{suggestions}");
}

#[test]
fn agent_suggestions_come_from_the_registry_catalog() {
    use peri_acp_types::workspace_resources::ResourceScope;
    let tool = with_agent_catalog(
        make_subagent_tool(Vec::new()),
        &[(ResourceScope::Project, "local-agent", LOCAL_AGENT_FM)],
    );
    let error = "Error: cannot find agent definition 'local'";
    let with_suggestion = tool.agent_error_with_suggestions(error, Some("local"));
    assert!(with_suggestion.contains("local-agent"), "{with_suggestion}");

    // 无资源面 ⇒ 无候选，错误原文返回（不猜、不读盘）。
    let bare = make_subagent_tool(Vec::new());
    assert_eq!(
        bare.agent_error_with_suggestions(error, Some("local")),
        error
    );
}

#[tokio::test]
async fn invoke_resolves_agent_from_argument_cwd_before_starting_factory() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    // W5：Agent 定义来源是**会话绑定的资源面**（builtin `workspace` 实例），
    // 调用参数 `cwd` 只影响子代理的执行目录，**不影响定义来源**。本用例保留
    // 原有「失败不启动 factory」断言（锁 failed-before-spawn 顺序）。
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("actual-agent.md"),
        "---\nname: actual-agent\ndescription: Actual agent\n---\n\nActual.\n",
    )
    .unwrap();
    let factory_calls = Arc::new(AtomicUsize::new(0));
    let factory_calls_clone = Arc::clone(&factory_calls);
    let tool = with_agent_face(
        SubAgentTool::new(
            Arc::new(Vec::new()),
            None,
            Arc::new(move |_| {
                factory_calls_clone.fetch_add(1, Ordering::SeqCst);
                Box::new(EchoLLM) as Box<dyn ReactLLM + Send + Sync>
            }),
            dir.path().to_str().unwrap().to_string(),
        ),
        dir.path(),
    )
    .await;
    let cwd = dir.path().to_str().unwrap();

    // 1) 资源面命中的定义：即使 `cwd` 参数指向别处也能解析（来源由会话绑定）。
    let result = tool
        .invoke(
            serde_json::json!({
                "subagent_type": "actual-agent",
                "prompt": "from bound face",
                "cwd": cwd,
            }),
            peri_agent::tools::ToolContext::new(&[], cwd),
        )
        .await
        .unwrap();
    assert!(result.contains("echo: from bound face"));
    assert_eq!(factory_calls.load(Ordering::SeqCst), 1);

    // 2) 未命中：失败且**不启动 factory**（顺序契约不变）。
    let error = tool
        .invoke(
            serde_json::json!({
                "subagent_type": "actual",
                "prompt": "typo",
                "cwd": cwd,
            }),
            peri_agent::tools::ToolContext::new(&[], cwd),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("cannot find"), "{error}");
    assert_eq!(
        factory_calls.load(Ordering::SeqCst),
        1,
        "定义不可得时不得启动 factory"
    );
}

#[tokio::test]
async fn background_invoke_uses_argument_cwd_for_loader_failure() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    // W5：Agent 定义来源是**会话绑定的资源面**（builtin `workspace` 实例的
    // project agent 根 = 会话 cwd 的 `.claude/agents`）；`cwd` 参数不再决定
    // 定义来源。本用例锁「定义不可得时后台路径报错且不启动 factory」。
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("background-agent.md"),
        "---\nname: background-agent\ndescription: Background agent\n---\n\nBackground.\n",
    )
    .unwrap();
    let factory_calls = Arc::new(AtomicUsize::new(0));
    let factory_calls_clone = Arc::clone(&factory_calls);
    let tool = with_agent_face(
        SubAgentTool::new(
            Arc::new(Vec::new()),
            None,
            Arc::new(move |_| {
                factory_calls_clone.fetch_add(1, Ordering::SeqCst);
                Box::new(EchoLLM) as Box<dyn ReactLLM + Send + Sync>
            }),
            dir.path().to_str().unwrap().to_string(),
        ),
        dir.path(),
    )
    .await;

    let result = tool
        .invoke(
            serde_json::json!({
                "subagent_type": "background-agent",
                "prompt": "bg",
                "run_in_background": true,
                "cwd": dir.path().to_str().unwrap(),
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("Background") || result.contains("task_id") || result.contains("echo: bg"),
        "资源面命中的定义必须可执行（无 task_manager 时同步回退）: {result}"
    );
    assert_eq!(factory_calls.load(Ordering::SeqCst), 1);

    // 未知 id：失败且不启动 factory（原有断言保留）。
    let calls_before = factory_calls.load(Ordering::SeqCst);
    let error = tool
        .invoke(
            serde_json::json!({
                "subagent_type": "does-not-exist",
                "prompt": "bg",
                "run_in_background": true,
                "cwd": dir.path().to_str().unwrap(),
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("cannot find"), "{error}");
    assert_eq!(factory_calls.load(Ordering::SeqCst), calls_before);
}

#[tokio::test]
async fn mcp_agent_suggestions_require_activation_and_connection() {
    use crate::mcp::{
        client::{ClientStatus, McpClientHandle, OAuthStatus},
        McpAgentRegistry, McpClientPool,
    };
    use rmcp::model::Resource;

    let empty_pool = Arc::new(McpClientPool::new_empty());
    let empty_registry = Arc::new(McpAgentRegistry::new(empty_pool));
    let empty_tool = make_subagent_tool(Vec::new()).with_mcp_agents(Some(empty_registry), None);
    let unactivated = empty_tool
        .invoke(
            serde_json::json!({
                "subagent_type": "mcp__offline__review",
                "prompt": "remote",
            }),
            peri_agent::tools::ToolContext::new(&[], "/tmp"),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(unactivated.contains("cannot find MCP agent definition"));
    assert!(!unactivated.contains("Suggestion"));
    assert!(!unactivated.contains("Available agent types"));

    let pool = Arc::new(McpClientPool::new_empty());
    pool.clients.write().insert(
        "offline".to_string(),
        Arc::new(McpClientHandle {
            name: "offline".to_string(),
            version: None,
            cache_version: None,
            peer: None,
            tools: Vec::new(),
            resources: vec![Resource::new(
                "agent://review/agent.md".to_string(),
                "Review agent",
            )],
            // The catalog can expose metadata while the runtime peer has already gone away.
            status: ClientStatus::Connected,
            oauth_status: OAuthStatus::None,
            source: None,
            url: None,
            skills_capable: false,
        }),
    );
    let registry = Arc::new(McpAgentRegistry::new(pool));
    let tool = make_subagent_tool(Vec::new()).with_mcp_agents(Some(registry), None);
    let disconnected = tool
        .invoke(
            serde_json::json!({
                "subagent_type": "mcp__offline__review",
                "prompt": "remote",
            }),
            peri_agent::tools::ToolContext::new(&[], "/tmp"),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        disconnected.contains("no active peer"),
        "unexpected disconnected activation error: {disconnected}"
    );
    assert!(!disconnected.contains("Suggestion"));
    assert!(!disconnected.contains("Available agent types"));
}

/// 真门面 fixture：临时 git 工作区 + 临时 SQLite + 已绑定会话。
///
/// 子 agent 的 resume/spawn 路径要求父会话与绑定真实存在，因此夹具
/// 不使用存储替身：会话、消息、状态都落在真实门面上，断言读回的是真实事实。
pub(crate) struct SessionFixture {
    pub(crate) resources: Arc<dyn SessionResources>,
    workspace: ResolvedWorkspace,
}

impl SessionFixture {
    /// 在给定目录建立真门面：目录本身即工作区（`git init` 提供仓库证据），
    /// 会话 cwd 与 agent 定义查找路径因此与用例的 fixture 目录一致。
    pub(crate) async fn open_in(dir: &std::path::Path) -> Self {
        git_init(dir);
        let resources: Arc<dyn SessionResources> = Arc::new(
            peri_resources::sessions::SessionResourcesImpl::open(dir.join("threads.db"))
                .await
                .unwrap(),
        );
        let workspace = resources.resolve_workspace(dir).await.unwrap();
        Self {
            resources,
            workspace,
        }
    }

    /// 门面句柄（`.with_session_resources(...)` / `SubagentHost` 注入用）。
    pub(crate) fn facade(&self) -> Arc<dyn SessionResources> {
        Arc::clone(&self.resources)
    }

    /// 夹具工作区的 canonical cwd（会话 cwd 与调用 cwd 必须一致）。
    pub(crate) fn workspace_cwd(&self) -> String {
        self.workspace.cwd.to_string_lossy().into_owned()
    }

    /// 建会话（真门面）：绑定与 frozen 一次落盘。
    pub(crate) async fn create_thread(
        &self,
        meta: peri_agent::thread::ThreadMeta,
    ) -> Result<ThreadId, anyhow::Error> {
        let session = NewSession {
            thread_id: meta.id.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
            meta: NewSessionMeta {
                title: meta.title.clone(),
                cwd: self.workspace.cwd.to_string_lossy().into_owned(),
                parent_thread_id: meta.parent_thread_id.clone(),
                hidden: meta.hidden,
                cancel_policy: meta.cancel_policy,
                snapshot_at_message_id: None,
            },
            binding: SessionBinding {
                schema_version: SESSION_BINDING_VERSION,
                revision: 1,
                project_id: self.workspace.project_id,
                workspace_id: self.workspace.workspace_id,
                cwd_relative_to_workspace: self.workspace.relative_cwd.clone(),
            },
            frozen: FrozenSnapshotBytes::new("{\"version\":1,\"fixture\":true}"),
        };
        self.resources
            .create_session(&session)
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        Ok(meta.id)
    }

    pub(crate) async fn append_messages(
        &self,
        id: &ThreadId,
        messages: &[BaseMessage],
    ) -> Result<(), anyhow::Error> {
        let payloads: Vec<PersistedPayload> = messages
            .iter()
            .cloned()
            .map(PersistedPayload::Message)
            .collect();
        self.resources
            .append_history(id, &payloads)
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))
    }

    pub(crate) async fn update_thread_status(
        &self,
        id: &ThreadId,
        status: &str,
    ) -> Result<(), anyhow::Error> {
        let status = match status {
            "done" => AgentStatus::Done,
            "cancelled" => AgentStatus::Cancelled,
            "error" => AgentStatus::Error,
            _ => AgentStatus::Active,
        };
        self.resources
            .update_session_meta(
                id,
                &SessionMetaPatch {
                    status: Some(status),
                    ..Default::default()
                },
            )
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))
    }

    pub(crate) async fn load_meta(&self, id: &ThreadId) -> Result<ThreadMeta, anyhow::Error> {
        self.resources
            .load_session_meta(id)
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))
    }

    pub(crate) async fn load_messages(
        &self,
        id: &ThreadId,
    ) -> Result<Vec<BaseMessage>, anyhow::Error> {
        Ok(self
            .resources
            .load_session_snapshot(id)
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?
            .payloads
            .into_iter()
            .filter_map(|payload| payload.as_message().cloned())
            .collect())
    }

    pub(crate) async fn list_session_threads(
        &self,
        id: &ThreadId,
    ) -> Result<Vec<ThreadMeta>, anyhow::Error> {
        self.resources
            .list_session_tree(id)
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))
    }
}

/// 把门面与父会话 id 一次装到工具上。
///
/// child 保存/认领要求父会话真实存在、调用 cwd 与父会话 cwd 一致；
/// 两者出自同一夹具。返回 canonical cwd——调用参数必须用它，否则 spawn 会按
/// 绑定不匹配拒绝（`/var` 与 `/private/var` 之类符号链接差异也算不匹配）。
pub(crate) async fn install_parent_session(
    tool: SubAgentTool,
    fixture: &SessionFixture,
) -> (SubAgentTool, String) {
    let cwd = fixture.workspace_cwd();
    // W5：Agent 定义只从会话绑定的资源面读取——夹具把真实 workspace 实例
    // （project agent 根 = 会话 cwd 的 `.claude/agents`）接到工具上。
    let tool = with_agent_face(tool, std::path::Path::new(&cwd)).await;
    let parent_id = fixture
        .create_thread(ThreadMeta::new_at(cwd.clone(), peri_time::now_wall()))
        .await
        .expect("建立父会话失败");
    let tool = tool
        .with_session_resources(fixture.facade())
        .with_parent_thread_id(parent_id);
    (tool, cwd)
}

/// 让目录成为 git 工作区（工作区发现需要真实仓库证据）。
pub(crate) fn git_init(directory: &std::path::Path) {
    for args in [
        vec!["init", "-q"],
        vec![
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-qm",
            "base",
        ],
    ] {
        let output = std::process::Command::new("git")
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", directory)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .arg("-C")
            .arg(directory)
            .args(&args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git fixture failed");
    }
}

/// 预置可恢复 thread：创建（title 决定工具集恢复路径）+ 写消息 + 置非 active。
async fn preset_resumable_thread(
    fixture: &SessionFixture,
    id: &str,
    title: &str,
    parent_thread_id: Option<&str>,
    msgs: Vec<BaseMessage>,
) {
    let id = id.to_string();
    let mut meta = peri_agent::thread::ThreadMeta::new_at("/tmp/work", peri_time::now_wall());
    meta.id = id.clone();
    meta.title = Some(title.to_string());
    meta.parent_thread_id = parent_thread_id.map(|s| s.to_string());
    meta.hidden = true;
    fixture.create_thread(meta).await.unwrap();
    if !msgs.is_empty() {
        fixture.append_messages(&id, &msgs).await.unwrap();
    }
    fixture.update_thread_status(&id, "done").await.unwrap();
}

// 本文件经 mod.rs 的 `#[path = "tool_test.rs"]` 挂载；此路径加载方式下，
// rustc 不会为聚合根派生 `tool_test/` 子目录，子模块需显式 `#[path]` 指向。
#[path = "tool_test/active_message_test.rs"]
mod active_message_test;

#[path = "tool_test/bg_register_cancel_test.rs"]
mod bg_register_cancel_test;
#[path = "tool_test/dynamic_mcp_subagent_test.rs"]
mod dynamic_mcp_subagent_test;
#[path = "tool_test/events_contract_test.rs"]
mod events_contract_test;
#[path = "tool_test/fork_test.rs"]
mod fork_test;
#[path = "tool_test/integration_v2_test.rs"]
mod integration_v2_test;
#[path = "tool_test/invoke_test.rs"]
mod invoke_test;
#[path = "tool_test/middleware_chain_test.rs"]
mod middleware_chain_test;
#[path = "tool_test/model_tier_test.rs"]
mod model_tier_test;
#[path = "tool_test/resume_failure_test.rs"]
mod resume_failure_test;
#[path = "tool_test/resume_integration_test.rs"]
mod resume_integration_test;
#[path = "tool_test/resume_test.rs"]
mod resume_test;
#[path = "tool_test/session_isolation_test.rs"]
mod session_isolation_test;

/// W5 关闭矩阵 E2E（生产链路）：agent 面关闭（`SubAgentMiddleware` 链槽关闭位 =
/// `SUB_AGENT_FACE_CLOSED_KEY`）⇒ 本地来源**不可发现、不可激活**，且**零磁盘兜底**
/// （定义文件仍在磁盘上也不得被读取）。
#[tokio::test]
async fn closed_agent_face_hides_and_blocks_activation_without_disk_fallback() {
    use peri_acp_types::workspace_resources::ResourceScope;

    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("matrix-agent.md"),
        "---\nname: matrix-agent\ndescription: Matrix agent\n---\n\nMatrix.\n",
    )
    .unwrap();

    // 默认（面开启）：资源面可见、可解析（定义文件存在即命中）。
    let open_fixture = crate::mcp::agent_face_fixture::AgentFaceFixture::connect(dir.path()).await;
    let open = open_fixture.registry.local_catalog(true);
    assert!(
        open.iter().any(|entry| entry.id == "matrix-agent"),
        "面开启时候选可见: {open:?}"
    );
    drop(open_fixture);

    // 关闭位（与 A24 关闭集同一份 `disabled_middlewares` 派生）：同一磁盘内容不可见。
    let closed_fixture =
        crate::mcp::agent_face_fixture::AgentFaceFixture::connect(dir.path()).await;
    let closed_registry =
        crate::mcp::McpAgentRegistry::new(std::sync::Arc::clone(&closed_fixture.pool))
            .with_local_face_closed(true);
    assert!(
        closed_registry.local_catalog(true).is_empty(),
        "关闭位下本地来源不可发现"
    );
    assert!(
        closed_registry.resolve_local("matrix-agent", true).is_err(),
        "关闭位下不可激活（零磁盘兜底）"
    );
    // 链槽关闭键是唯一事实源常量（不是散落字面量）。
    assert_eq!(
        crate::assembly::SUB_AGENT_FACE_CLOSED_KEY,
        "SubAgentMiddleware"
    );
    let _ = ResourceScope::Project;
}
