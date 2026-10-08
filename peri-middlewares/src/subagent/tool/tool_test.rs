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
use peri_agent::session::subagent::SubagentLlmSource;

// Mock LLM: returns final answer directly
#[derive(Clone)]
struct EchoLLM;

impl EchoLLM {
    async fn respond(
        &self,
        request: peri_model::ModelRequest,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
        use crate::subagent::test_support::*;
        let _ = &cancellation;
        let messages = base_messages(&request);
        let defined = defined_tools(&request);
        let tools: Vec<&dyn BaseTool> = defined.iter().map(|t| t as &dyn BaseTool).collect();

        let last = messages.last().map(|m| m.content()).unwrap_or_default();
        text_events(format!("echo: {}", last))
    }
}
crate::subagent::test_support::fixture_model_impl!(EchoLLM);

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
        Arc::new(|_: Option<&str>| {
            crate::subagent::test_support::fixture_source(
                std::sync::Arc::new(EchoLLM),
                "fixture-scripted",
            )
        }),
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
    let host = DurableHost::open_in(dir.path(), "fixture-tool-test-resolve").await;
    let tool = host.bind(
        with_agent_face(
            SubAgentTool::new(
                Arc::new(Vec::new()),
                None,
                Arc::new(move |_| {
                    factory_calls_clone.fetch_add(1, Ordering::SeqCst);
                    crate::subagent::test_support::fixture_source(
                        std::sync::Arc::new(EchoLLM),
                        "fixture-scripted",
                    )
                }),
                dir.path().to_str().unwrap().to_string(),
            ),
            dir.path(),
        )
        .await,
    );
    let cwd = dir.path().to_str().unwrap();

    // 1) 资源面命中的定义：即使 `cwd` 参数指向别处也能解析（来源由会话绑定）。
    let result = tool
        .invoke(
            serde_json::json!({
                "subagent_type": "actual-agent",
                "prompt": "from bound face",
                "cwd": cwd,
            }),
            host.context(&[]),
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
            host.context(&[]),
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
    let (bg_tx, _bg_rx) =
        tokio::sync::mpsc::unbounded_channel::<peri_agent::agent::events::ExecutorEvent>();
    let registry = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    let host = DurableHost::open_in_with_background(
        dir.path(),
        "fixture-tool-test-bg",
        Arc::clone(&registry),
        bg_tx,
    )
    .await;
    let tool = host.bind(
        with_agent_face(
            SubAgentTool::new(
                Arc::new(Vec::new()),
                None,
                Arc::new(move |_| {
                    factory_calls_clone.fetch_add(1, Ordering::SeqCst);
                    crate::subagent::test_support::fixture_source(
                        std::sync::Arc::new(EchoLLM),
                        "fixture-scripted",
                    )
                }),
                dir.path().to_str().unwrap().to_string(),
            ),
            dir.path(),
        )
        .await,
    );

    let result = tool
        .invoke(
            serde_json::json!({
                "subagent_type": "background-agent",
                "prompt": "bg",
                "run_in_background": true,
                "cwd": dir.path().to_str().unwrap(),
            }),
            host.context(&[]),
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
            host.context(&[]),
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

    /// 在父会话 work state 登记一条受信 invocation（durable 委派的前置事实）。
    ///
    /// 与生产同源的 WorkAction：`PrepareInvocation` 建立 intent，`BindResourceOwners`
    /// 记录授权引用；revision 冲突按真实语义重试。
    pub(crate) async fn prepare_invocation(&self, parent_id: &str, invocation_id: &str) {
        use peri_acp_types::session_resources::work::{
            InvocationIntent, WorkAction, WorkCommand, WorkDecision, WorkQuery, WorkRejection,
        };
        use sha2::{Digest, Sha256};

        let mut commands = Vec::new();
        let arguments = "{}".to_owned();
        let digest = format!("{:x}", Sha256::digest(arguments.as_bytes()));
        commands.push(WorkAction::PrepareInvocation {
            expected_revision: 0,
            intent: InvocationIntent {
                invocation_id: invocation_id.into(),
                tool_call_id: format!("model-call:{invocation_id}"),
                tool_name: "Agent".into(),
                arguments_json: arguments.clone(),
                arguments_digest: digest.clone(),
                effective_tool_name: "Agent".into(),
                effective_arguments_json: arguments,
                effective_arguments_digest: digest,
                owner_identity: "fixture-agent-owner".into(),
                scope_id: parent_id.into(),
                scope_epoch: None,
                authorization_ref: "fixture-delegation-authorization".into(),
                recovery_locator: format!("fixture-delegation:{invocation_id}"),
            },
        });
        commands.push(WorkAction::BindResourceOwners {
            expected_revision: 0,
            connections_json: "{}".into(),
            authorization_ref: "fixture-delegation-authorization".into(),
        });

        for (index, mut action) in commands.into_iter().enumerate() {
            loop {
                let snapshot = self
                    .resources
                    .load_session_work(&WorkQuery {
                        session_id: parent_id.to_owned(),
                        limit: 1,
                    })
                    .await
                    .unwrap();
                match &mut action {
                    WorkAction::PrepareInvocation {
                        expected_revision, ..
                    }
                    | WorkAction::BindResourceOwners {
                        expected_revision, ..
                    } => *expected_revision = snapshot.state.revision,
                    _ => unreachable!("fixture invocation actions"),
                }
                let mut command = WorkCommand {
                    session_id: parent_id.to_owned(),
                    recipient_lifecycle: snapshot.control.lifecycle,
                    mutation_id: format!("fixture-invocation:{invocation_id}:{index}"),
                    action: action.clone(),
                };
                command.mutation_id =
                    format!("{}:{}", command.mutation_id, command.digest().unwrap());
                let receipt = self.resources.apply_work_mutation(&command).await.unwrap();
                assert_eq!(receipt.session_id, command.session_id);
                match receipt.decision {
                    WorkDecision::Accepted => break,
                    WorkDecision::Rejected {
                        reason: WorkRejection::StaleRevision,
                    } => continue,
                    decision => panic!("fixture invocation mutation rejected: {decision:?}"),
                }
            }
        }
    }

    /// 生命周期变更（close→Reopen）后把子运行时 metadata 重新绑定到新生命周期。
    ///
    /// 生产由宿主在重新绑定执行时为新生命周期持久化子身份/授权事实；夹具按同一
    /// 契约复制既有 metadata，不改写内容、不伪造身份。
    pub(crate) async fn rebind_child_resume_metadata(
        &self,
        child_id: &str,
        from_lifecycle: u64,
        to_lifecycle: u64,
    ) {
        use peri_acp_types::session_resources::work::{
            WorkAction, WorkCommand, WorkDecision, WorkQuery,
        };
        let snapshot = self
            .resources
            .load_session_work(&WorkQuery {
                session_id: child_id.to_string(),
                limit: 1,
            })
            .await
            .unwrap();
        let raw = snapshot
            .state
            .child_resume_metadata
            .get(&from_lifecycle)
            .cloned()
            .expect("fixture child resume metadata present for previous lifecycle");
        // metadata 是生命周期作用域数据：新生命周期必须携带自己的 recipient_lifecycle
        // （生产由宿主在新绑定时写入同一身份的当轮记录）。
        let mut metadata: peri_agent::session::subagent::ChildResumeMetadata =
            serde_json::from_str(&raw).expect("fixture child resume metadata decodes");
        metadata.recipient_lifecycle = to_lifecycle;
        let raw = serde_json::to_string(&metadata).unwrap();
        let command = WorkCommand {
            session_id: child_id.to_string(),
            recipient_lifecycle: to_lifecycle,
            mutation_id: format!("fixture-rebind-child-metadata:{child_id}:{to_lifecycle}"),
            action: WorkAction::BindChildResumeMetadata {
                expected_revision: snapshot.state.revision,
                metadata_json: raw,
            },
        };
        let receipt = self.resources.apply_work_mutation(&command).await.unwrap();
        assert!(matches!(receipt.decision, WorkDecision::Accepted));
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
                workspace_id: self.workspace.execution_registration_id,
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

/// 测试用 SDK admission 端口：从子会话真实 work snapshot 签发一次性 ticket
/// （与生产 `ExecutionAdmissionPort` 契约同形；不预置 admission、不绕过登记）。
pub(crate) struct TestAdmissionPort(pub(crate) Arc<dyn SessionResources>);

#[async_trait::async_trait]
impl peri_acp_types::execution_admission::ExecutionAdmissionPort for TestAdmissionPort {
    async fn admit(
        &self,
        request: peri_acp_types::execution_admission::AdmissionRequest,
    ) -> Result<
        peri_acp_types::execution_admission::AdmissionOutcome,
        peri_acp_types::execution_admission::ExecutionAdmissionError,
    > {
        use peri_acp_types::execution_admission::AdmissionOutcome;
        use peri_acp_types::session_resources::work::WorkAdmission;
        let snapshot = request.snapshot;
        if snapshot.control.attempt.is_some() {
            return Ok(AdmissionOutcome::Busy);
        }
        let Some(candidate) = snapshot.candidates.first() else {
            return Ok(AdmissionOutcome::Blocked {
                reason: "fixture has no actionable durable work".into(),
            });
        };
        Ok(AdmissionOutcome::Admitted {
            admission: WorkAdmission {
                session_id: snapshot.session_id,
                admission_id: request.request_id,
                instance_id: "fixture-sdk-instance".into(),
                generation_id: "fixture-sdk-generation".into(),
                lifecycle: snapshot.control.lifecycle,
                control_generation: snapshot.control.control_generation,
                work_id: candidate.work_id.clone(),
                work_revision: candidate.work_revision,
                execution: peri_acp_types::session_resources::ControlAttempt {
                    turn_id: peri_acp_types::session::TurnId::new(),
                    attempt_id: peri_acp_types::identity::AttemptId::new(),
                },
            },
        })
    }

    async fn entered(
        &self,
        request: peri_acp_types::execution_admission::EntryRequest,
    ) -> Result<
        peri_acp_types::execution_admission::EntryOutcome,
        peri_acp_types::execution_admission::ExecutionAdmissionError,
    > {
        use peri_acp_types::execution_admission::{EntryOutcome, EntryReceipt};
        use peri_acp_types::session_resources::work::WorkQuery;
        // 协作式 SDK 夹具：以真实 work state 校验 admission 身份与 entry 证据，
        // 合法时确认进入（相当于 SDK 侧 ACK），不预置状态、不跳过生产登记。
        let snapshot = self
            .0
            .load_session_work(&WorkQuery {
                session_id: request.admission.session_id.clone(),
                limit: 1,
            })
            .await
            .map_err(|error| {
                peri_acp_types::execution_admission::ExecutionAdmissionError::Protocol(
                    error.to_string(),
                )
            })?;
        let Some(registration) = snapshot
            .state
            .admissions
            .get(&request.admission.admission_id)
        else {
            return Ok(EntryOutcome::NotApplied);
        };
        if registration.admission != request.admission {
            return Err(
                peri_acp_types::execution_admission::ExecutionAdmissionError::Protocol(
                    "entry admission identity mismatch".into(),
                ),
            );
        }
        if let Some(receipt) = &registration.entering_receipt {
            if receipt.mutation_id != request.entry_evidence_id {
                return Err(
                    peri_acp_types::execution_admission::ExecutionAdmissionError::Protocol(
                        "entry evidence does not match registered admission".into(),
                    ),
                );
            }
        }
        Ok(EntryOutcome::Applied {
            receipt: EntryReceipt {
                admission: request.admission,
                entry_evidence_id: request.entry_evidence_id,
            },
        })
    }

    async fn settle(
        &self,
        request: peri_acp_types::execution_admission::SettlementRequest,
    ) -> Result<
        peri_acp_types::execution_admission::SettlementOutcome,
        peri_acp_types::execution_admission::ExecutionAdmissionError,
    > {
        use peri_acp_types::execution_admission::{SettlementOutcome, SettlementReceipt};
        Ok(SettlementOutcome::Applied {
            receipt: SettlementReceipt {
                admission: request.admission,
                evidence_id: request.proof.evidence_id().to_string(),
            },
        })
    }
}

/// 耐久宿主：真门面 + 已绑定/frozen 的父会话 + 已登记的可信 invocation。
///
/// 生产 `publish_work_delegation` 要求：父工具有会话资源、父工作区有 Active 控制记录、
/// 父 work state 中存在受信 invocation，且子会话 work state 可接纳——本夹具按真实
/// 门面（SQLite）建立这些事实，不放宽生产准入、不引入测试替身绕过。
pub(crate) struct DurableHost {
    pub(crate) _dir: Option<tempfile::TempDir>,
    pub(crate) fixture: SessionFixture,
    pub(crate) parent_id: ThreadId,
    pub(crate) invocation_id: String,
    pub(crate) cwd: String,
    parent_session: std::sync::Arc<peri_agent::session::Session>,
}

impl DurableHost {
    /// 在既有工作区目录上建立耐久宿主，并在装配前定制父 host（write-once：
    /// 通道 / langfuse bridge 等必须在装配时给定）。
    pub(crate) async fn open_in_with_host(
        dir: &std::path::Path,
        invocation_id: &str,
        configure: impl FnOnce(&mut peri_agent::session::subagent::SubagentHost),
    ) -> Self {
        let mut host = DurableHost::open_in(dir, invocation_id).await;
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
        invocation_id: &str,
        task_manager: std::sync::Arc<peri_agent::agent::async_tasks::TaskManager>,
        bg_event_sender: tokio::sync::mpsc::UnboundedSender<
            peri_agent::agent::events::ExecutorEvent,
        >,
    ) -> Self {
        let mut host = Self::open(invocation_id).await;
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
        invocation_id: &str,
        task_manager: std::sync::Arc<peri_agent::agent::async_tasks::TaskManager>,
        bg_event_sender: tokio::sync::mpsc::UnboundedSender<
            peri_agent::agent::events::ExecutorEvent,
        >,
    ) -> Self {
        let mut host = Self::open_in(dir, invocation_id).await;
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

    pub(crate) async fn open(invocation_id: &str) -> Self {
        let dir = tempdir().unwrap();
        let fixture = SessionFixture::open_in(dir.path()).await;
        let cwd = fixture.workspace_cwd();
        let parent_id = fixture
            .create_thread(ThreadMeta::new_at(cwd.clone(), peri_time::now_wall()))
            .await
            .expect("建立父会话失败");
        fixture.prepare_invocation(&parent_id, invocation_id).await;
        let parent_session = build_parent_session(&cwd, &parent_id, &fixture);
        Self {
            _dir: Some(dir),
            fixture,
            parent_id,
            invocation_id: invocation_id.to_string(),
            cwd,
            parent_session,
        }
    }

    /// 在调用方已有的工作区目录上建立耐久宿主（agent 定义查找路径与调用 cwd 一致，
    /// 目录生命周期由调用方持有）。
    pub(crate) async fn open_in(dir: &std::path::Path, invocation_id: &str) -> Self {
        let fixture = SessionFixture::open_in(dir).await;
        let cwd = fixture.workspace_cwd();
        let parent_id = fixture
            .create_thread(ThreadMeta::new_at(cwd.clone(), peri_time::now_wall()))
            .await
            .expect("建立父会话失败");
        fixture.prepare_invocation(&parent_id, invocation_id).await;
        let parent_session = build_parent_session(&cwd, &parent_id, &fixture);
        Self {
            _dir: None,
            fixture,
            parent_id,
            invocation_id: invocation_id.to_string(),
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

    /// 父 session 句柄（同库第二个工具/断言用）。
    pub(crate) fn parent_session(&self) -> std::sync::Arc<peri_agent::session::Session> {
        std::sync::Arc::clone(&self.parent_session)
    }

    /// 带可信 invocation 的调用上下文（durable dispatch 语义）。
    pub(crate) fn context<'a>(
        &'a self,
        messages: &'a [BaseMessage],
    ) -> peri_agent::tools::ToolContext<'a> {
        self.context_with(messages, self.invocation_id.clone())
    }

    /// 指定 invocation 的调用上下文（同一父会话内多次委派必须使用各自的
    /// 可信 invocation：一个 invocation 只能绑定一个委派任务）。
    pub(crate) fn context_with<'a>(
        &'a self,
        messages: &'a [BaseMessage],
        invocation_id: String,
    ) -> peri_agent::tools::ToolContext<'a> {
        let mut ctx = peri_agent::tools::ToolContext::new(messages, &self.cwd);
        ctx.invocation_id = Some(invocation_id);
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
            execution_admission_port: Some(Arc::new(TestAdmissionPort(self.fixture.facade()))),
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

    /// 在父会话登记一个新的可信 invocation（多次委派用例；id 唯一）。
    pub(crate) async fn fresh_invocation(&self, label: &str) -> String {
        let invocation_id = format!("{}-{label}-{}", self.invocation_id, uuid::Uuid::now_v7());
        self.fixture
            .prepare_invocation(&self.parent_id, &invocation_id)
            .await;
        invocation_id
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
    if host.execution_admission_port.is_none() {
        host.execution_admission_port = Some(Arc::new(TestAdmissionPort(fixture.facade())));
    }
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
        execution_admission_port: Some(Arc::new(TestAdmissionPort(fixture.facade()))),
        task_manager: Some(Arc::new(peri_agent::agent::async_tasks::TaskManager::new())),
        ..Default::default()
    };
    host.parent_thread_id = Some(parent_id.to_string());
    session.set_subagent_host(host);
    session
}

/// resume 用例的调用上下文：携带 preset 子会话对应的受信 invocation
/// （`preset_resumable_child` 以 `fixture-resume-invocation:{child}` 登记）。
pub(crate) fn preset_child_ctx(
    child_id: &str,
    cwd: &str,
) -> peri_agent::tools::ToolContext<'static> {
    static EMPTY: [BaseMessage; 0] = [];
    // cwd 由调用方给出（通常为 `"."`，静态字面量），与空消息切片一样拥有 'static。
    let cwd: &'static str = Box::leak(cwd.to_string().into_boxed_str());
    let mut ctx = peri_agent::tools::ToolContext::new(&EMPTY, cwd);
    ctx.invocation_id = Some(format!("fixture-resume-invocation:{child_id}"));
    ctx
}

/// 委派（spawn / resume）用例的调用上下文：在父会话登记一个新的可信委派
/// invocation，并把它作为本次调用的 invocation 传入。
///
/// 生产路径的 invocation 来自 SDK 准入的委派意图（tool_call_id/authorization_ref/
/// scope 一起登记）；夹具按同一契约登记后再调用，不构造未登记的 id。
pub(crate) async fn preset_delegation_ctx(
    store: &SessionFixture,
    parent_id: &str,
    label: &str,
    cwd: &str,
) -> peri_agent::tools::ToolContext<'static> {
    static EMPTY: [BaseMessage; 0] = [];
    let invocation_id = format!(
        "fixture-delegation-invocation:{label}:{}",
        uuid::Uuid::now_v7()
    );
    store.prepare_invocation(parent_id, &invocation_id).await;
    let cwd: &'static str = Box::leak(cwd.to_string().into_boxed_str());
    let mut ctx = peri_agent::tools::ToolContext::new(&EMPTY, cwd);
    ctx.invocation_id = Some(invocation_id);
    ctx
}

/// 给 resume 用例自有的父 session 挂生产 host（资源门面 + SDK 端口 + 父线程 id）。
///
/// 子链宿主来自 owning parent session（`parent.subagent_host()`），因此端口与
/// 资源必须装在父 session 上；工具的 host 字段只在父 session 无 host 时生效。
pub(crate) fn install_parent_host(
    store: &SessionFixture,
    parent: &std::sync::Arc<peri_agent::session::Session>,
) {
    build_parent_host(store, parent, |_| {});
}

/// 同 [`install_parent_host`]，并附加观测 bridge。
///
/// bridge 与资源/端口一样属于父 session host（`set_subagent_host` write-once），
/// 工具的 bridge 字段在父 session 已有 host 时被遮蔽；v2 Start/Stop 观测必须
/// 装在父 host 上（与 `DurableHost::open_in_with_host` 同一契约）。
pub(crate) fn install_parent_host_with_bridge(
    store: &SessionFixture,
    parent: &std::sync::Arc<peri_agent::session::Session>,
    bridge: Arc<dyn peri_agent::agent::LangfuseBridgeLike>,
) {
    build_parent_host(store, parent, move |host| {
        host.langfuse_bridge = Some(bridge)
    });
}

fn build_parent_host(
    store: &SessionFixture,
    parent: &std::sync::Arc<peri_agent::session::Session>,
    customize: impl FnOnce(&mut peri_agent::session::subagent::SubagentHost),
) {
    use peri_agent::session::subagent::SubagentHost;
    let mut host = parent
        .subagent_host()
        .as_deref()
        .cloned()
        .unwrap_or_default();
    host.session_resources = Some(store.facade());
    host.execution_admission_port = Some(Arc::new(TestAdmissionPort(store.facade())));
    if host.task_manager.is_none() {
        host.task_manager = Some(Arc::new(peri_agent::agent::async_tasks::TaskManager::new()));
    }
    host.parent_thread_id = parent.store().thread_id.clone();
    customize(&mut host);
    parent.set_subagent_host(host);
}

/// 给 resume 用例自有父 session 挂生产 host，并使用调用方的后台通道。
pub(crate) fn install_parent_host_with_channels(
    store: &SessionFixture,
    parent: &std::sync::Arc<peri_agent::session::Session>,
    task_manager: std::sync::Arc<peri_agent::agent::async_tasks::TaskManager>,
    bg_event_sender: tokio::sync::mpsc::UnboundedSender<peri_agent::agent::events::ExecutorEvent>,
) {
    use peri_agent::session::subagent::SubagentHost;
    let mut host = parent
        .subagent_host()
        .as_deref()
        .cloned()
        .unwrap_or_default();
    host.session_resources = Some(store.facade());
    host.execution_admission_port = Some(Arc::new(TestAdmissionPort(store.facade())));
    host.task_manager = Some(task_manager);
    host.bg_event_sender = Some(bg_event_sender);
    host.parent_thread_id = parent.store().thread_id.clone();
    parent.set_subagent_host(host);
}

/// 给工具宿主补 SDK admission 端口（父 session 未挂 host 时的回退路径）。
pub(crate) fn install_admission_port(tool: SubAgentTool, fixture: &SessionFixture) -> SubAgentTool {
    let mut tool = tool;
    tool.host.execution_admission_port = Some(Arc::new(TestAdmissionPort(fixture.facade())));
    tool
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
///
/// 空 `tool_ceiling` 表示「该委派当年没有工具面」——生产 spawn 记录的是
/// 过滤后工具名集合（`factory/spawn.rs` 的 `tool_ceiling`/`tool_origins`），
/// 恢复时按 `ceiling ∩ 本次重建工具` 求交（`factory/resume.rs`）。因此要断言
/// 恢复工具面的用例必须用 [`preset_resumable_thread_with_ceiling`] 传入与原
/// 委派一致的工具面，不能依赖空集（空集恢复出来的工具面必然为空）。
async fn preset_resumable_thread(
    fixture: &SessionFixture,
    id: &str,
    title: &str,
    parent_thread_id: Option<&str>,
    msgs: Vec<BaseMessage>,
) {
    preset_resumable_child(fixture, id, title, parent_thread_id, None, msgs).await;
}

/// 预置可恢复 thread，并按生产事实写入原委派的工具面（`tool_ceiling` +
/// `tool_origins`，本地工具 origin 为 null，与 spawn 写入一致）。
async fn preset_resumable_thread_with_ceiling(
    fixture: &SessionFixture,
    id: &str,
    title: &str,
    parent_thread_id: Option<&str>,
    ceiling: &[&str],
    msgs: Vec<BaseMessage>,
) {
    preset_resumable_child_with_ceiling(fixture, id, title, parent_thread_id, None, ceiling, msgs)
        .await;
}

/// 预置可恢复子线程：绑定/frozen 会话 + 已登记父 invocation + v1 运行时 metadata。
///
/// production resume 要求保存的 `ChildResumeMetadata` 与父会话可信 invocation 同时
/// 存在（frozen digest、授权引用、任务绑定都按真实事实校验）；夹具按同一契约写入，
/// 不跳过任何生产校验。
async fn preset_resumable_child(
    fixture: &SessionFixture,
    id: &str,
    title: &str,
    parent_thread_id: Option<&str>,
    invocation_id: Option<&str>,
    msgs: Vec<BaseMessage>,
) {
    preset_resumable_child_with_ceiling(
        fixture,
        id,
        title,
        parent_thread_id,
        invocation_id,
        &[],
        msgs,
    )
    .await;
}

async fn preset_resumable_child_with_ceiling(
    fixture: &SessionFixture,
    id: &str,
    title: &str,
    parent_thread_id: Option<&str>,
    invocation_id: Option<&str>,
    ceiling: &[&str],
    msgs: Vec<BaseMessage>,
) {
    let id = id.to_string();
    let mut meta = peri_agent::thread::ThreadMeta::new_at("/tmp/work", peri_time::now_wall());
    meta.id = id.clone();
    meta.title = Some(title.to_string());
    meta.parent_thread_id = parent_thread_id.map(|s| s.to_string());
    meta.hidden = true;
    fixture.create_thread(meta).await.unwrap();
    // 先初始化子会话的 work ledger，再写历史：真实门面只有在「已有 canonical
    // 历史但还没有 durable 处理检查点」时才标记 legacy_unknown（pre-ledger-history），
    // 该标记会让后续委派按生产规则要求显式 reconciliation。夹具按生产顺序建立
    // 事实（ledger 先于历史），而不是放宽该门。
    fixture
        .prepare_invocation(&id, &format!("fixture-child-ledger:{id}"))
        .await;
    if !msgs.is_empty() {
        fixture.append_messages(&id, &msgs).await.unwrap();
    }
    fixture.update_thread_status(&id, "done").await.unwrap();

    if let Some(parent_id) = parent_thread_id {
        use peri_acp_types::session_resources::work::{
            WorkAction, WorkCommand, WorkDecision, WorkQuery, WorkStage, WorkTarget,
        };
        use sha2::{Digest, Sha256};
        let invocation_id = invocation_id
            .map(str::to_string)
            .unwrap_or_else(|| format!("fixture-resume-invocation:{id}"));
        fixture.prepare_invocation(parent_id, &invocation_id).await;
        let metadata = peri_agent::session::subagent::ChildResumeMetadata {
            version: 1,
            child_session_id: id.clone(),
            recipient_lifecycle: 1,
            agent_name: title.to_string(),
            model_name: "fixture-scripted".into(),
            direct_initiator_session_id: parent_id.to_string(),
            direct_initiator_lifecycle: 1,
            delegation_invocation_id: invocation_id,
            delegation_task_id: id.clone(),
            authorization_ref: "fixture-delegation-authorization".into(),
            frozen_digest: format!("{:x}", Sha256::digest(b"{\"version\":1,\"fixture\":true}")),
            tool_ceiling: ceiling.iter().map(|name| name.to_string()).collect(),
            tool_origins: ceiling
                .iter()
                .map(|name| (name.to_string(), None))
                .collect(),
            skill_names: Vec::new(),
            max_iterations: 200,
            persona: Some("fixture-child-identity".into()),
            system_prompt: String::new(),
            identity_system: None,
            runtime_env: None,
            claude_md: "frozen-claude".into(),
            claude_local_md: None,
            skill_summary: "frozen-skills".into(),
            date: "2026-08-05".into(),
            language: None,
            section_overrides: Default::default(),
            disabled_middlewares: Default::default(),
            built_in_subagents_enabled: true,
        };
        // 可恢复子会话的历史处理已结清：未结算的 work 会让 FollowUp 委派按
        // 生产规则拒绝（requires reconciliation）。夹具按真实 SettleWork 命令结清。
        loop {
            let snapshot = fixture
                .resources
                .load_session_work(&WorkQuery {
                    session_id: id.clone(),
                    limit: 64,
                })
                .await
                .unwrap();
            let unresolved = snapshot
                .state
                .works
                .values()
                .find(|work| {
                    !matches!(work.stage, WorkStage::Settled | WorkStage::Abandoned)
                        && snapshot.state.work_lifecycle(&work.work_id) == Some(1)
                })
                .cloned();
            let Some(work) = unresolved else { break };
            let command = WorkCommand {
                session_id: id.clone(),
                recipient_lifecycle: 1,
                mutation_id: format!("fixture-child-settle:{}", work.work_id),
                action: WorkAction::SettleWork {
                    expected_revision: snapshot.state.revision,
                    target: WorkTarget {
                        work_id: work.work_id.clone(),
                        expected_work_revision: work.revision,
                    },
                },
            };
            let receipt = fixture
                .resources
                .apply_work_mutation(&command)
                .await
                .unwrap();
            assert!(matches!(receipt.decision, WorkDecision::Accepted));
        }
        let snapshot = fixture
            .resources
            .load_session_work(&WorkQuery {
                session_id: id.clone(),
                limit: 1,
            })
            .await
            .unwrap();
        let command = WorkCommand {
            session_id: id.clone(),
            recipient_lifecycle: snapshot.control.lifecycle,
            mutation_id: format!("fixture-child-metadata:{id}"),
            action: WorkAction::BindChildResumeMetadata {
                expected_revision: snapshot.state.revision,
                metadata_json: serde_json::to_string(&metadata).unwrap(),
            },
        };
        let receipt = fixture
            .resources
            .apply_work_mutation(&command)
            .await
            .unwrap();
        assert!(matches!(receipt.decision, WorkDecision::Accepted));
    }
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
#[path = "tool_test/mock_model.rs"]
mod mock_model;
#[path = "tool_test/model_tier_test.rs"]
mod model_tier_test;
#[path = "tool_test/resume_failure_test.rs"]
mod resume_failure_test;
#[path = "tool_test/resume_integration_test.rs"]
mod resume_integration_test;
#[path = "tool_test/resume_test.rs"]
mod resume_test;
#[path = "tool_test/sections_parity_test.rs"]
mod sections_parity_test;
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
