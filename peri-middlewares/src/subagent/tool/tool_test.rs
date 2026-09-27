use std::sync::Arc;

use parking_lot::RwLock;
use peri_acp_types::identity::AgentId;
use peri_acp_types::session_resources::{
    FrozenSnapshotBytes, NewSession, NewSessionMeta, SessionMetaPatch, SessionResources,
};
use peri_acp_types::store::PersistedPayload;
use peri_acp_types::thread::AgentStatus;
use peri_acp_types::workspace::{
    ResolvedWorkspace, SessionBinding, SessionExecutionLease, SESSION_BINDING_VERSION,
};
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
use crate::claude_agent_parser::ToolsValue;

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

#[test]
fn built_in_policy_rejects_new_built_in_definition() {
    let tool = tool_with_built_ins_disabled("/nonexistent");
    let error = tool.load_agent_def("coder", "/nonexistent").unwrap_err();
    assert!(error.contains("cannot find agent definition 'coder'"));
}

#[test]
fn built_in_policy_keeps_project_override_callable() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("coder.md"),
        "---\nname: project-coder\ndescription: Project override\n---\n\nProject agent.\n",
    )
    .unwrap();
    let cwd = dir.path().to_str().unwrap();
    let agent = tool_with_built_ins_disabled(cwd)
        .load_agent_def("coder", cwd)
        .unwrap();
    assert_eq!(agent.frontmatter.name, "project-coder");
}

#[test]
fn built_in_policy_keeps_plugin_definition_callable() {
    let dir = tempdir().unwrap();
    let plugin_dir = dir.path().join("plugin-agents");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(
        plugin_dir.join("plugin-reviewer.md"),
        "---\nname: plugin-reviewer\ndescription: Plugin agent\n---\n\nReview.\n",
    )
    .unwrap();
    let tool = tool_with_built_ins_disabled(dir.path().to_str().unwrap())
        .with_plugin_agent_dirs(Arc::new(vec![plugin_dir]));
    let agent = tool
        .load_agent_def("plugin-reviewer", dir.path().to_str().unwrap())
        .unwrap();
    assert_eq!(agent.frontmatter.name, "plugin-reviewer");
}

#[test]
fn plugin_definition_loader_rejects_traversal_agent_id() {
    let dir = tempdir().unwrap();
    let plugin_dir = dir.path().join("plugin-agents");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(
        dir.path().join("outside.md"),
        "---\nname: outside\ndescription: Must not load\n---\n\nOutside.\n",
    )
    .unwrap();
    let tool = make_subagent_tool(Vec::new()).with_plugin_agent_dirs(Arc::new(vec![plugin_dir]));
    let error = tool
        .load_agent_def("../outside", dir.path().to_str().unwrap())
        .unwrap_err();
    assert!(error.contains("invalid agent definition ID"));
}

#[test]
fn agent_suggestions_follow_the_invocation_cwd() {
    let a = tempdir().unwrap();
    let b = tempdir().unwrap();
    let agents = a.path().join(".claude/agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        agents.join("local-agent.md"),
        "---\nname: local-agent\ndescription: Local agent\n---\n\nLocal.\n",
    )
    .unwrap();
    let tool = make_subagent_tool(Vec::new());
    let error = "Error: cannot find agent definition 'local'";

    let from_b =
        tool.agent_error_with_suggestions(error, Some("local"), b.path().to_str().unwrap());
    assert!(!from_b.contains("local-agent"));
    let from_a =
        tool.agent_error_with_suggestions(error, Some("local"), a.path().to_str().unwrap());
    assert!(from_a.contains("local-agent"));
}

#[test]
fn agent_suggestions_track_files_added_and_removed_mid_session() {
    let dir = tempdir().unwrap();
    let agents = dir.path().join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    let path = agents.join("changing-agent.md");
    std::fs::write(
        &path,
        "---\nname: changing-agent\ndescription: Changing agent\n---\n\nChanging.\n",
    )
    .unwrap();
    let tool = make_subagent_tool(Vec::new());
    let error = "Error: cannot find agent definition 'changing'";
    let cwd = dir.path().to_str().unwrap();

    assert!(tool
        .agent_error_with_suggestions(error, Some("changing"), cwd)
        .contains("changing-agent"));
    std::fs::remove_file(path).unwrap();
    assert!(!tool
        .agent_error_with_suggestions(error, Some("changing"), cwd)
        .contains("changing-agent"));
}

#[test]
fn agent_suggestions_validate_flat_nested_plugin_and_builtin_sources() {
    let dir = tempdir().unwrap();
    let agents = dir.path().join("agents");
    std::fs::create_dir_all(agents.join("nested")).unwrap();
    std::fs::write(
        agents.join("flat.md"),
        "---\nname: flat\ndescription: Flat agent\n---\n\nFlat.\n",
    )
    .unwrap();
    std::fs::write(
        agents.join("nested/agent.md"),
        "---\nname: nested\ndescription: Nested agent\n---\n\nNested.\n",
    )
    .unwrap();
    let cwd = dir.path().to_str().unwrap();
    let tool = make_subagent_tool(Vec::new());
    let error = "Error: cannot find agent definition";
    assert!(tool
        .agent_error_with_suggestions(error, Some("neste"), cwd)
        .contains("nested"));
    assert!(tool
        .agent_error_with_suggestions(error, Some("fla"), cwd)
        .contains("flat"));

    let plugin = tempdir().unwrap();
    std::fs::write(
        plugin.path().join("plugin-only.md"),
        "---\nname: plugin-only\ndescription: Plugin agent\n---\n\nPlugin.\n",
    )
    .unwrap();
    let plugin_tool = tool_with_built_ins_disabled(cwd)
        .with_plugin_agent_dirs(Arc::new(vec![plugin.path().to_path_buf()]));
    assert!(plugin_tool
        .agent_error_with_suggestions(error, Some("plugin"), cwd)
        .contains("plugin-only"));
    assert!(!plugin_tool
        .agent_error_with_suggestions(error, Some("explor"), cwd)
        .contains("explorer"));

    assert!(tool
        .agent_error_with_suggestions(error, Some("explor"), cwd)
        .contains("explorer"));
}

#[test]
fn agent_suggestions_skip_invalid_and_shadowed_definitions() {
    let dir = tempdir().unwrap();
    let agents = dir.path().join(".claude/agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(agents.join("broken.md"), "not valid frontmatter").unwrap();
    std::fs::write(agents.join("coder.md"), "not valid frontmatter").unwrap();
    let tool = make_subagent_tool(Vec::new());
    let cwd = dir.path().to_str().unwrap();
    let error = "Error: cannot find agent definition";

    assert!(!tool
        .agent_error_with_suggestions(error, Some("broke"), cwd)
        .contains("broken"));
    assert!(!tool
        .agent_error_with_suggestions(error, Some("code"), cwd)
        .contains("coder"));
}

#[tokio::test]
async fn invoke_resolves_agent_from_argument_cwd_before_starting_factory() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let a = tempdir().unwrap();
    let b = tempdir().unwrap();
    let agents = b.path().join(".claude/agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        agents.join("actual-agent.md"),
        "---\nname: actual-agent\ndescription: Actual agent\n---\n\nActual.\n",
    )
    .unwrap();
    let factory_calls = Arc::new(AtomicUsize::new(0));
    let factory_calls_clone = Arc::clone(&factory_calls);
    let tool = SubAgentTool::new(
        Arc::new(Vec::new()),
        None,
        Arc::new(move |_| {
            factory_calls_clone.fetch_add(1, Ordering::SeqCst);
            Box::new(EchoLLM) as Box<dyn ReactLLM + Send + Sync>
        }),
        a.path().to_str().unwrap().to_string(),
    );
    let a_cwd = a.path().to_str().unwrap();
    let b_cwd = b.path().to_str().unwrap();

    let result = tool
        .invoke(
            serde_json::json!({
                "subagent_type": "actual-agent",
                "prompt": "from B",
                "cwd": b_cwd,
            }),
            peri_agent::tools::ToolContext::new(&[], a_cwd),
        )
        .await
        .unwrap();
    assert!(result.contains("echo: from B"));
    assert_eq!(factory_calls.load(Ordering::SeqCst), 1);

    let typo = tool
        .invoke(
            serde_json::json!({
                "subagent_type": "actual",
                "prompt": "typo",
                "cwd": b_cwd,
            }),
            peri_agent::tools::ToolContext::new(&[], a_cwd),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(typo.contains("actual-agent"));
    assert_eq!(factory_calls.load(Ordering::SeqCst), 1);

    let stale = tool
        .invoke(
            serde_json::json!({
                "subagent_type": "actual",
                "prompt": "stale cwd",
                "cwd": a_cwd,
            }),
            peri_agent::tools::ToolContext::new(&[], a_cwd),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(!stale.contains("actual-agent"));
    assert_eq!(factory_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn background_invoke_uses_argument_cwd_for_loader_failure() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let a = tempdir().unwrap();
    let b = tempdir().unwrap();
    let agents = b.path().join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        agents.join("background-agent.md"),
        "---\nname: background-agent\ndescription: Background agent\n---\n\nBackground.\n",
    )
    .unwrap();
    let factory_calls = Arc::new(AtomicUsize::new(0));
    let factory_calls_clone = Arc::clone(&factory_calls);
    let tool = SubAgentTool::new(
        Arc::new(Vec::new()),
        None,
        Arc::new(move |_| {
            factory_calls_clone.fetch_add(1, Ordering::SeqCst);
            Box::new(EchoLLM) as Box<dyn ReactLLM + Send + Sync>
        }),
        a.path().to_str().unwrap().to_string(),
    )
    .with_task_manager(Arc::new(peri_agent::agent::async_tasks::TaskManager::new()));
    let a_cwd = a.path().to_str().unwrap();
    let b_cwd = b.path().to_str().unwrap();

    let typo = tool
        .invoke(
            serde_json::json!({
                "subagent_type": "background",
                "run_in_background": true,
                "prompt": "typo",
                "cwd": b_cwd,
            }),
            peri_agent::tools::ToolContext::new(&[], a_cwd),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(typo.contains("background-agent"));
    assert_eq!(factory_calls.load(Ordering::SeqCst), 0);

    let stale = tool
        .invoke(
            serde_json::json!({
                "subagent_type": "background",
                "run_in_background": true,
                "prompt": "stale cwd",
                "cwd": a_cwd,
            }),
            peri_agent::tools::ToolContext::new(&[], a_cwd),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(!stale.contains("background-agent"));
    assert_eq!(factory_calls.load(Ordering::SeqCst), 0);
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
            channel_capable: false,
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

/// 真门面 fixture：临时 git 工作区 + 临时 SQLite + 会话执行所有权。
///
/// 子 agent 的 resume/spawn 路径要求「根会话有活 owner」这一真实前置条件，因此夹具
/// 不使用存储替身：会话、消息、状态都落在真实门面上，断言读回的是真实事实。
pub(crate) struct SessionFixture {
    pub(crate) resources: Arc<dyn SessionResources>,
    workspace: ResolvedWorkspace,
    /// 执行所有权必须存活到会话生命周期结束（drop 即释放 owner）。
    leases: parking_lot::Mutex<Vec<Arc<dyn SessionExecutionLease>>>,
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
            leases: parking_lot::Mutex::new(Vec::new()),
        }
    }

    /// 门面句柄（`.with_session_resources(...)` / `SubagentHost` 注入用）。
    pub(crate) fn facade(&self) -> Arc<dyn SessionResources> {
        Arc::clone(&self.resources)
    }

    /// 最近一次建会话的执行所有权（child 保存的前置证明）。
    ///
    /// 夹具自己持有 lease 让 owner 保持活跃；调用方拿到的是同一份所有权句柄，
    /// 用于 `.with_execution_owner(...)`，不产生第二个 owner。
    pub(crate) fn execution_owner(&self) -> Arc<dyn SessionExecutionLease> {
        self.leases
            .lock()
            .last()
            .cloned()
            .expect("夹具尚未建立会话：先 create_thread")
    }

    /// 夹具工作区的 canonical cwd（会话 cwd 与调用 cwd 必须一致）。
    pub(crate) fn workspace_cwd(&self) -> String {
        self.workspace.cwd.to_string_lossy().into_owned()
    }

    /// 建会话（真门面）：绑定 + frozen + 执行代际一次落盘，owner 由夹具持有。
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
        let lease = self
            .resources
            .create_session(&session)
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        self.leases.lock().push(lease);
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

/// 把「门面 + 父会话 id + root owner」一次装到工具上。
///
/// child 保存/认领要求父会话真实存在、root owner 存活、调用 cwd 与父会话 cwd 一致；
/// 三者出自同一夹具。返回 canonical cwd——调用参数必须用它，否则 spawn 会按
/// 绑定不匹配拒绝（`/var` 与 `/private/var` 之类符号链接差异也算不匹配）。
pub(crate) async fn install_parent_session(
    tool: SubAgentTool,
    fixture: &SessionFixture,
) -> (SubAgentTool, String) {
    let cwd = fixture.workspace_cwd();
    let parent_id = fixture
        .create_thread(ThreadMeta::new(cwd.clone()))
        .await
        .expect("建立父会话失败");
    let tool = tool
        .with_session_resources(fixture.facade())
        .with_parent_thread_id(parent_id)
        .with_execution_owner(fixture.execution_owner());
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
    let mut meta = peri_agent::thread::ThreadMeta::new("/tmp/work");
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
#[path = "tool_test/resume_integration_test.rs"]
mod resume_integration_test;
#[path = "tool_test/resume_test.rs"]
mod resume_test;
#[path = "tool_test/tool_filter_test.rs"]
mod tool_filter_test;
