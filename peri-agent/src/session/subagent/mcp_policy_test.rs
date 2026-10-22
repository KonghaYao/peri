//! 父冻结 MCP 关闭策略在子初始化、spawn/resume 与 Reason 刷新中保持收敛。
use super::*;
use std::{
    collections::BTreeSet,
    sync::atomic::{AtomicUsize, Ordering},
};

use crate::error::AgentResult;
use crate::middleware::{capabilities, r#trait::Middleware};
use crate::session::tool_catalog::{
    SessionToolCatalog, StartupToolUpdate, ToolFilter, ToolFilterPolicy,
};
use crate::tools::{BaseTool, ToolContext};
use peri_acp_types::{
    mcp_control::SessionMcpCatalogSnapshot, meta_harness::MetaHarnessState,
    ports::SessionMcpCatalogPort,
};

struct SourceTool {
    name: &'static str,
    server: Option<&'static str>,
    builtin: Option<&'static str>,
}

#[async_trait::async_trait]
impl BaseTool for SourceTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        self.name
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    fn is_direct(&self) -> bool {
        true
    }
    fn mcp_server_name(&self) -> Option<&str> {
        self.server
    }
    fn mcp_tool_name(&self) -> Option<&str> {
        self.server.map(|_| self.name)
    }
    fn builtin_mcp_instance(&self) -> Option<&str> {
        self.builtin
    }
    async fn invoke(
        &self,
        _input: serde_json::Value,
        _ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok("core Read executed".into())
    }
}

fn core_tools() -> Vec<Arc<dyn BaseTool>> {
    ["Read", "CoreEcho"]
        .into_iter()
        .map(|name| {
            Arc::new(SourceTool {
                name,
                server: None,
                builtin: None,
            }) as Arc<dyn BaseTool>
        })
        .collect()
}

fn mcp_tools(late: bool) -> Vec<Arc<dyn BaseTool>> {
    let mut tools = vec![
        ("Write", "workspace", Some("workspace")),
        ("WebSearch", "web", Some("web")),
        ("artifact", "artifact", Some("artifact")),
        ("mcp__cron__cron_register", "cron", Some("cron")),
        ("mcp__workflow__Workflow", "workflow", Some("workflow")),
        // Workflow 的远端 Workspace：server 恰同名，但没有 builtin 身份。
        ("RemoteRead", "workspace", None),
        ("External", "external", None),
        ("ChildDenied", "external", None),
    ];
    if late {
        tools.extend([
            ("Edit", "workspace", Some("workspace")),
            ("WebFetch", "web", Some("web")),
            ("mcp__cron__cron_list", "cron", Some("cron")),
            (
                "mcp__workflow__WorkflowStatus",
                "workflow",
                Some("workflow"),
            ),
            ("LateExternal", "external", None),
            ("LateChildDenied", "external", None),
        ]);
    }
    tools
        .into_iter()
        .map(|(name, server, builtin)| {
            Arc::new(SourceTool {
                name,
                server: Some(server),
                builtin,
            }) as Arc<dyn BaseTool>
        })
        .collect()
}

struct MutableCatalog {
    current: RwLock<Arc<SessionMcpCatalogSnapshot>>,
    reads: AtomicUsize,
}

impl MutableCatalog {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            current: RwLock::new(Arc::new(SessionMcpCatalogSnapshot {
                generation: 1,
                tools: mcp_tools(false),
            })),
            reads: AtomicUsize::new(0),
        })
    }
    fn publish_next_generation(&self) {
        *self.current.write() = Arc::new(SessionMcpCatalogSnapshot {
            generation: 2,
            tools: mcp_tools(true),
        });
    }
}

impl SessionMcpCatalogPort for MutableCatalog {
    fn snapshot(&self) -> Arc<SessionMcpCatalogSnapshot> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.current.read().clone()
    }
}

const CASES: &[(&str, &[&str])] = &[
    ("WorkspaceMiddleware", &["Write", "Edit"]),
    ("WebMiddleware", &["WebSearch", "WebFetch"]),
    ("ArtifactMiddleware", &["artifact"]),
    (
        "CronMiddleware",
        &["mcp__cron__cron_register", "mcp__cron__cron_list"],
    ),
    (
        "WorkflowMiddleware",
        &["mcp__workflow__Workflow", "mcp__workflow__WorkflowStatus"],
    ),
    ("McpMiddleware", &[]),
];

fn expected_names(policy: &str, closed: &[&str], late: bool) -> BTreeSet<String> {
    let mut names = BTreeSet::from(["Read".to_string()]);
    if policy != "McpMiddleware" {
        names.extend(
            mcp_tools(late)
                .into_iter()
                .filter(|tool| {
                    !closed.contains(&tool.name())
                        && !["ChildDenied", "LateChildDenied"].contains(&tool.name())
                })
                .map(|tool| tool.name().to_string()),
        );
    }
    names
}

fn child_filter(wildcard: bool) -> ToolFilter {
    ToolFilterPolicy::canonical(
        wildcard.then(|| vec!["*".into()]),
        vec![
            "CoreEcho".into(),
            "ChildDenied".into(),
            "LateChildDenied".into(),
        ],
    )
}

fn policy_session(policy: &str) -> Arc<Session> {
    Session::new(
        Arc::from("/tmp/work"),
        FrozenContext::builder()
            .meta_harness(MetaHarnessState {
                disabled_middlewares: [policy.to_string()].into_iter().collect(),
                ..Default::default()
            })
            .build(),
        Some("mcp-policy-parent".into()),
    )
}

fn direct_context(
    policy: &str,
    wildcard: bool,
    source: Arc<dyn SessionMcpCatalogPort>,
) -> V2SubagentContext {
    build_v2_subagent_context(
        Some(policy_session(policy)),
        Box::new(NullReactLLM),
        Arc::new(MiddlewareChain::new()),
        core_tools().into_iter().chain(mcp_tools(false)).collect(),
        child_filter(wildcard),
        Some(source),
        "/tmp/work",
        CancellationToken::new(),
        None,
        None,
        None,
        None,
        None,
    )
}

#[test]
fn frozen_mcp_policy_filters_direct_context_initial_view_and_refresh() {
    for &(policy, closed) in CASES {
        for wildcard in [false, true] {
            let source = MutableCatalog::new();
            let built = direct_context(policy, wildcard, source.clone());
            let initial = expected_names(policy, closed, false);
            assert_eq!(
                built
                    .context
                    .runtime
                    .tools
                    .read()
                    .keys()
                    .cloned()
                    .collect::<BTreeSet<_>>(),
                initial,
                "initial working map: {policy}, wildcard={wildcard}"
            );
            let catalog = &built.context.runtime.tool_catalog;
            assert_eq!(
                catalog
                    .snapshot()
                    .tools
                    .keys()
                    .cloned()
                    .collect::<BTreeSet<_>>(),
                initial
            );
            source.publish_next_generation();
            assert_eq!(
                catalog
                    .refresh()
                    .unwrap()
                    .tools
                    .keys()
                    .cloned()
                    .collect::<BTreeSet<_>>(),
                expected_names(policy, closed, true),
                "refresh: {policy}, wildcard={wildcard}"
            );
            if policy == "McpMiddleware" {
                assert_eq!(source.reads.load(Ordering::SeqCst), 0);
            }
        }
    }
}

struct UnreadableCatalog;
impl SessionMcpCatalogPort for UnreadableCatalog {
    fn snapshot(&self) -> Arc<SessionMcpCatalogSnapshot> {
        panic!("disabled MCP source must not be consulted, even during refresh")
    }
}

#[test]
fn disabled_mcp_does_not_query_failed_source_or_block_core_tools() {
    let built = direct_context("McpMiddleware", true, Arc::new(UnreadableCatalog));
    let snapshot = built.context.runtime.tool_catalog.refresh().unwrap();
    assert_eq!(snapshot.tools.keys().cloned().collect::<Vec<_>>(), ["Read"]);
    assert!(built.context.runtime.tools.read().contains_key("Read"));
}

#[test]
fn closed_mcp_candidate_does_not_replace_same_named_core_during_initial_merge() {
    for policy in ["WorkspaceMiddleware", "McpMiddleware"] {
        let core = core_tools().remove(0);
        let closed: Arc<dyn BaseTool> = Arc::new(SourceTool {
            name: "Read",
            server: Some("workspace"),
            builtin: Some("workspace"),
        });
        let built = build_v2_subagent_context(
            Some(policy_session(policy)),
            Box::new(NullReactLLM),
            Arc::new(MiddlewareChain::new()),
            vec![core.clone(), closed],
            child_filter(true),
            None,
            "/tmp/work",
            CancellationToken::new(),
            None,
            None,
            None,
            None,
            None,
        );
        assert!(Arc::ptr_eq(
            &built.context.runtime.tools.read()["Read"],
            &core
        ));
        assert!(Arc::ptr_eq(
            &built.context.runtime.tool_catalog.refresh().unwrap().tools["Read"].tool,
            &core
        ));
    }
}

#[test]
fn closed_builtin_does_not_shadow_allowed_remote_in_catalog_generations() {
    let source = MutableCatalog::new();
    let closed: Arc<dyn BaseTool> = Arc::new(SourceTool {
        name: "WebSearch",
        server: Some("web"),
        builtin: Some("web"),
    });
    let remote: Arc<dyn BaseTool> = Arc::new(SourceTool {
        name: "WebSearch",
        server: Some("remote"),
        builtin: None,
    });
    *source.current.write() = Arc::new(SessionMcpCatalogSnapshot {
        generation: 1,
        tools: vec![closed.clone(), remote.clone()],
    });
    let filter = ToolFilterPolicy::with_mcp_policy(
        ["WebMiddleware".into()].into_iter().collect(),
        child_filter(true),
    );
    let catalog = SessionToolCatalog::with_filter(Default::default(), Some(source.clone()), filter);
    assert!(Arc::ptr_eq(
        &catalog.snapshot().tools["WebSearch"].tool,
        &remote
    ));
    let refreshed_remote: Arc<dyn BaseTool> = Arc::new(SourceTool {
        name: "WebSearch",
        server: Some("new-remote"),
        builtin: None,
    });
    *source.current.write() = Arc::new(SessionMcpCatalogSnapshot {
        generation: 2,
        tools: vec![closed, refreshed_remote.clone()],
    });
    let refreshed = catalog.refresh().unwrap();
    assert!(Arc::ptr_eq(
        &refreshed.tools["WebSearch"].tool,
        &refreshed_remote
    ));
}

#[test]
fn startup_merge_ignores_closed_candidates_and_preserves_allowed_same_name() {
    let closed: Arc<dyn BaseTool> = Arc::new(SourceTool {
        name: "WebSearch",
        server: Some("web"),
        builtin: Some("web"),
    });
    let remote: Arc<dyn BaseTool> = Arc::new(SourceTool {
        name: "WebSearch",
        server: Some("remote"),
        builtin: None,
    });
    let filter = ToolFilterPolicy::with_mcp_policy(
        ["WebMiddleware".into()].into_iter().collect(),
        child_filter(true),
    );
    let catalog = SessionToolCatalog::with_filter(
        [("WebSearch".into(), closed.clone())].into_iter().collect(),
        None,
        filter,
    );
    assert!(catalog.snapshot().tools.is_empty());
    let snapshot = catalog
        .replace_static_mcp_tools(StartupToolUpdate {
            tools: vec![closed.clone(), remote.clone()],
            required: vec![],
        })
        .unwrap();
    assert!(Arc::ptr_eq(&snapshot.tools["WebSearch"].tool, &remote));
    let snapshot = catalog
        .replace_static_mcp_tools(StartupToolUpdate {
            tools: vec![closed],
            required: vec![],
        })
        .unwrap();
    assert!(Arc::ptr_eq(&snapshot.tools["WebSearch"].tool, &remote));
}

#[derive(Clone, Default)]
struct CatalogProbe {
    bound: Arc<RwLock<Vec<BTreeSet<String>>>>,
    initial: Arc<RwLock<Vec<BTreeSet<String>>>>,
    reasons: Arc<RwLock<Vec<BTreeSet<String>>>>,
}

#[async_trait::async_trait]
impl Middleware for CatalogProbe {
    fn name(&self) -> &str {
        "McpPolicyProbe"
    }
    async fn before_agent(
        &self,
        state: &mut dyn capabilities::BeforeAgentState,
    ) -> AgentResult<()> {
        self.initial.write().push(
            state
                .local_tools()
                .unwrap()
                .read()
                .keys()
                .cloned()
                .collect(),
        );
        Ok(())
    }
    async fn before_reason_catalog(
        &self,
        state: &mut dyn capabilities::CatalogState,
    ) -> AgentResult<()> {
        self.reasons.write().push(
            state
                .local_tools()
                .unwrap()
                .read()
                .keys()
                .cloned()
                .collect(),
        );
        Ok(())
    }
}

impl SubagentChainAssembler for CatalogProbe {
    fn assemble(&self, _ctx: &SubagentChainContext) -> MiddlewareChain {
        let mut chain = MiddlewareChain::new();
        chain.add(Box::new(self.clone()));
        chain
    }
    fn bind_tools(
        &self,
        _session: &Arc<Session>,
        tools: Vec<Arc<dyn BaseTool>>,
        _filter: ToolFilter,
    ) -> Vec<Arc<dyn BaseTool>> {
        self.bound
            .write()
            .push(tools.iter().map(|tool| tool.name().to_string()).collect());
        tools
    }
}

#[derive(Clone)]
struct RefreshModel {
    source: Arc<MutableCatalog>,
    requests: Arc<RwLock<Vec<BTreeSet<String>>>>,
}

impl RefreshModel {
    async fn respond(
        &self,
        request: peri_model::ModelRequest,
        _cancellation: CancellationToken,
    ) -> Vec<peri_model::ModelResult<peri_model::ModelStreamEvent>> {
        let first = {
            let mut requests = self.requests.write();
            requests.push(request.tools.iter().map(|tool| tool.name.clone()).collect());
            requests.len() == 1
        };
        if !first {
            return crate::session::test_resources::mock::model::text_events("done");
        }
        self.source.publish_next_generation();
        let response = peri_model::ModelResponse::new(
            peri_model::ModelMessage::assistant(
                vec![],
                vec![peri_model::ToolCall::new(
                    format!("core-read-{}", uuid::Uuid::now_v7()),
                    "Read",
                    peri_model::JsonObject::default(),
                )],
            ),
            peri_model::StopReason::ToolUse,
            None,
            None,
        )
        .unwrap();
        vec![Ok(peri_model::ModelStreamEvent::Completed(response))]
    }
}
crate::fixture_model_impl!(RefreshModel);

fn spawn_config(
    store: Arc<MockSessionResources>,
    model: RefreshModel,
    probe: CatalogProbe,
    wildcard: bool,
) -> SubagentSpawnConfig {
    SubagentSpawnConfig {
        agent_name: "mcp-policy-child".into(),
        prompt: "exercise the core Read tool and refresh".into(),
        parent_messages: vec![],
        cancel_policy: SubagentCancelPolicy::Independent,
        max_iterations: 3,
        fork_directive_kind: None,
        run_mode: SubagentRunMode::Sync,
        skill_names: vec![],
        llm: SubagentLlmSource::model(Arc::new(model), "policy-fixture"),
        chain_assembler: Arc::new(probe),
        tools: core_tools().into_iter().chain(mcp_tools(false)).collect(),
        tool_filter: child_filter(wildcard),
        system_prompt: None,
        tool_invocation_resolver: None,
        compact_config: None,
        context_budget: None,
        compact_llm: None,
        session_resources: Some(store),
        event_handler: None,
        bg_event_sender: None,
        task_manager: None,
        on_bg_complete: None,
        langfuse_bridge: None,
        on_subagent_start: None,
        on_subagent_stop: None,
        register_runtime: None,
        deregister_runtime: None,
        parent_agent_id: None,
        parent_tool_call_id: None,
        cancel_token: None,
        cwd: None,
        parent_thread_id: None,
        frozen_claude_md: None,
        frozen_claude_local_md: None,
        frozen_skill_summary: None,
        frozen_date: None,
    }
}

#[tokio::test]
async fn spawn_and_resume_keep_parent_mcp_policy_through_reason_refresh() {
    for &(policy, closed) in CASES {
        for wildcard in [false, true] {
            let store = MockSessionResources::new();
            store.register_bound_session("mcp-policy-parent", "/tmp/work");
            let mut child_id = None;
            for resume in [false, true] {
                // 每次执行创建宿主快照；Session 的宿主按生产契约只注入一次。
                let parent = policy_session(policy);
                let source = MutableCatalog::new();
                parent.set_subagent_host(SubagentHost {
                    session_mcp_catalog: Some(source.clone()),
                    ..Default::default()
                });
                let probe = CatalogProbe::default();
                let requests = Arc::new(RwLock::new(Vec::new()));
                let model = RefreshModel {
                    source: source.clone(),
                    requests: requests.clone(),
                };
                let spawned = if resume {
                    let mut config = resume_config(store.clone(), child_id.clone().unwrap());
                    config.prompt = Some("refresh again".into());
                    config.max_iterations = 3;
                    config.llm = SubagentLlmSource::model(Arc::new(model), "policy-fixture");
                    config.chain_assembler = Arc::new(probe.clone());
                    config.tools = core_tools().into_iter().chain(mcp_tools(false)).collect();
                    config.tool_filter = child_filter(wildcard);
                    SessionFactory::resume_subagent(Some(&parent), config)
                        .await
                        .unwrap()
                } else {
                    SessionFactory::spawn_subagent(
                        Some(&parent),
                        spawn_config(store.clone(), model, probe.clone(), wildcard),
                    )
                    .await
                    .unwrap()
                };
                child_id = Some(spawned.child_thread_id);
                assert!(!spawned.interrupted);
                let initial = expected_names(policy, closed, false);
                let refreshed = expected_names(policy, closed, true);
                assert_eq!(
                    probe.bound.read().as_slice(),
                    std::slice::from_ref(&initial),
                    "bind: {policy}, resume={resume}"
                );
                assert_eq!(
                    probe.initial.read().as_slice(),
                    std::slice::from_ref(&initial),
                    "initial: {policy}, resume={resume}"
                );
                assert_eq!(
                    *probe.reasons.read(),
                    [initial.clone(), refreshed.clone()],
                    "Reason: {policy}, resume={resume}"
                );
                assert_eq!(
                    *requests.read(),
                    [initial, refreshed],
                    "model: {policy}, resume={resume}"
                );
                if policy == "McpMiddleware" {
                    assert_eq!(source.reads.load(Ordering::SeqCst), 0);
                }
            }
        }
    }
}
