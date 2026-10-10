//! Tests for mid_toolsearch

use super::*;
use async_trait::async_trait;
use peri_agent::middleware::r#trait::Middleware;

/// Helper: call prompt_contribution with concrete State type for testing.
fn contribution(mw: &ToolSearchMiddleware) -> Option<String> {
    Middleware::prompt_contribution(mw)
}

struct MockTool {
    name_str: String,
    desc_str: String,
    direct: bool,
    model_visible: bool,
    decl: Option<String>,
    mcp_source: Option<&'static str>,
    namespace: Option<&'static str>,
}

impl MockTool {
    fn new(name: &str, desc: &str) -> Self {
        Self {
            name_str: name.to_string(),
            desc_str: desc.to_string(),
            direct: false,
            model_visible: true,
            decl: None,
            mcp_source: None,
            namespace: None,
        }
    }

    /// 标记为 LLM 可见（direct）工具。
    fn with_direct(mut self) -> Self {
        self.direct = true;
        self
    }

    /// app-only：宿主可调用（dispatch/HITL 面不变），但不得投影给模型。
    fn with_model_invisible(mut self) -> Self {
        self.model_visible = false;
        self
    }

    /// 声明提示词层模板（design v2 §2.5.1 prompt_declaration）。
    fn with_prompt_declaration(mut self, declaration: &str) -> Self {
        self.decl = Some(declaration.to_string());
        self
    }

    fn with_mcp_source(mut self, server: &'static str) -> Self {
        self.mcp_source = Some(server);
        self
    }

    fn with_namespace(mut self, namespace: &'static str) -> Self {
        self.namespace = Some(namespace);
        self
    }
}

#[async_trait]
impl BaseTool for MockTool {
    fn name(&self) -> &str {
        &self.name_str
    }
    fn description(&self) -> &str {
        &self.desc_str
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    fn is_direct(&self) -> bool {
        self.direct
    }
    fn visible_to_model(&self) -> bool {
        self.model_visible
    }
    fn mcp_server_name(&self) -> Option<&str> {
        self.mcp_source
    }
    fn namespace(&self) -> Option<&str> {
        self.namespace
    }
    fn prompt_declaration(&self) -> Option<String> {
        self.decl.clone()
    }
    async fn invoke(
        &self,
        _input: serde_json::Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok("mock".to_string())
    }
}

fn build_test_components() -> (
    Arc<ToolSearchIndex>,
    Arc<RwLock<BTreeMap<String, Arc<dyn BaseTool>>>>,
) {
    let index = Arc::new(ToolSearchIndex::new());
    index.build(vec![
        Arc::new(MockTool::new("CronRegister", "Register a cron task")),
        Arc::new(MockTool::new("mcp__slack__send", "Send Slack message")),
    ]);

    let mut shared = BTreeMap::new();
    shared.insert(
        "CronRegister".to_string(),
        Arc::new(MockTool::new("CronRegister", "Register a cron task")) as Arc<dyn BaseTool>,
    );
    shared.insert(
        "mcp__slack__send".to_string(),
        Arc::new(MockTool::new("mcp__slack__send", "Send Slack message")) as Arc<dyn BaseTool>,
    );

    (index, Arc::new(RwLock::new(shared)))
}

#[test]
fn test_collect_tools_returns_meta_tools() {
    let (index, shared) = build_test_components();
    let mw = ToolSearchMiddleware::new(index, shared);
    let tools = <ToolSearchMiddleware as Middleware>::collect_tools(&mw, "/tmp");

    assert_eq!(tools.len(), 2, "expected the two ToolSearch meta tools");
    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert!(names.contains(&"SearchExtraTools"));
    assert!(names.contains(&"ExecuteExtraTool"));
}

#[tokio::test]
async fn test_before_agent_caches_prompt_contribution() {
    let (index, shared) = build_test_components();
    let mw = ToolSearchMiddleware::new(index, shared);

    let mut state = peri_agent::agent::state::AgentState::new("/tmp");
    mw.before_agent(&mut state).await.unwrap();

    assert!(
        contribution(&mw).is_some(),
        "before_agent 应缓存 prompt 贡献"
    );
    let contribution = contribution(&mw).unwrap();
    assert!(
        contribution.contains("CronRegister"),
        "prompt 贡献应包含延迟工具列表"
    );
    // before_agent 不应再向 state 写入消息
    assert_eq!(state.messages().len(), 0);
}

#[tokio::test]
async fn test_second_before_agent_caches_same_contribution() {
    let (index, shared) = build_test_components();
    let mw = ToolSearchMiddleware::new(index, shared);

    let mut state1 = peri_agent::agent::state::AgentState::new("/tmp");
    mw.before_agent(&mut state1).await.unwrap();
    let first_content = contribution(&mw).unwrap();

    let mut state2 = peri_agent::agent::state::AgentState::new("/tmp");
    mw.before_agent(&mut state2).await.unwrap();
    assert_eq!(
        contribution(&mw).unwrap(),
        first_content,
        "第二轮缓存的贡献应与首轮完全一致"
    );
}

/// [回归测试] WorkflowTool 搜索面与注册/prompt gate 共用同一条件源（阶段 3）。
///
/// 历史背景（审计 prompt-sections-audit.md P1-5）：模型按 16_workflow 的指引
/// 先 SearchExtraTools 发现，若索引与注册不一致会出现"声明可用但搜不到"。
/// 修复后 workflow 注册（WorkflowMiddlewareAdaptor::collect_tools）、
/// deferred 搜索（本测试）与 prompt section（peri-acp Workflow gate）三面
/// 均由 `workflow_executor.is_some()` 同一条件源驱动。
#[tokio::test]
async fn test_deferred_workflow_tool_discoverable_after_before_agent() {
    // 模拟 workflow_executor=Some 时 builder 装配后的 shared_tools：
    // WorkflowTool 以 deferred 形式注册（不直接进 LLM tools）。
    let index = Arc::new(ToolSearchIndex::new());
    let mut shared = BTreeMap::new();
    shared.insert(
        "Workflow".to_string(),
        Arc::new(MockTool::new("Workflow", "Orchestrate multiple agents")) as Arc<dyn BaseTool>,
    );
    let shared = Arc::new(RwLock::new(shared));
    let mw = ToolSearchMiddleware::new(index.clone(), shared);
    let mut state = peri_agent::agent::state::AgentState::new("/tmp");
    mw.before_agent(&mut state).await.unwrap();

    let results = index.search("select:Workflow", 10);
    assert_eq!(
        results.len(),
        1,
        "已注册的 Workflow 应能被 SearchExtraTools 发现"
    );
    assert_eq!(results[0].name, "Workflow");
}

/// [回归测试] workflow_executor=None（print mode）时 Workflow 不可发现。
///
/// 历史背景：16_workflow 曾无条件渲染，即使 WorkflowTool 未注册。修复后
/// None 场景下 prompt section 不渲染、WorkflowTool 不注册、索引不可发现
/// ——三面同时关闭。此用例锁定搜索面（索引不含 Workflow）。
#[tokio::test]
async fn test_workflow_not_discoverable_when_not_registered() {
    let (index, shared) = build_test_components(); // 不含 Workflow
    let mw = ToolSearchMiddleware::new(index.clone(), shared);
    let mut state = peri_agent::agent::state::AgentState::new("/tmp");
    mw.before_agent(&mut state).await.unwrap();

    let results = index.search("select:Workflow", 10);
    assert!(
        results.is_empty(),
        "未注册的 Workflow 不应被 SearchExtraTools 发现（print mode 语义）"
    );
}

/// 可测试的 MiddlewareState：local_tools 返回每 turn 本地视图（v2 路径
/// `AgentContext::from_stage` 语义，`StageContext.runtime.tools`）。
struct LocalToolsState {
    local: peri_agent::agent::stages::SharedToolMap,
}

impl LocalToolsState {
    fn new(local: peri_agent::agent::stages::SharedToolMap) -> Self {
        Self { local }
    }
}

impl peri_agent::middleware::state::MiddlewareState for LocalToolsState {
    fn cwd(&self) -> &str {
        "/tmp"
    }
    fn messages(&self) -> &[peri_agent::messages::BaseMessage] {
        &[]
    }
    fn add_message(&mut self, _message: peri_agent::messages::BaseMessage) {}
    fn replace_message(&mut self, _message: peri_agent::messages::BaseMessage) -> bool {
        false
    }
    fn current_step(&self) -> usize {
        0
    }
    fn push_recall(&mut self, _item: String) {}
    fn drain_recall(&mut self) -> Vec<String> {
        vec![]
    }
    fn v2_queue(&self) -> &peri_agent::session::MessageQueue {
        unreachable!()
    }
    fn local_tools(&self) -> Option<&peri_agent::agent::stages::SharedToolMap> {
        Some(&self.local)
    }
}

#[tokio::test]
async fn external_mcp_meta_name_winners_survive_catalog_rebinds() {
    let index = Arc::new(ToolSearchIndex::new());
    let shared = Arc::new(RwLock::new(BTreeMap::new()));
    let middleware = ToolSearchMiddleware::new(index, shared);
    let external = ["SearchExtraTools", "ExecuteExtraTool"]
        .into_iter()
        .map(|name| {
            let tool: Arc<dyn BaseTool> = Arc::new(
                MockTool::new(name, "external system tool")
                    .with_direct()
                    .with_mcp_source("system-server")
                    .with_namespace("meta"),
            );
            (name.to_string(), tool)
        })
        .collect::<BTreeMap<_, _>>();
    let local: peri_agent::agent::stages::SharedToolMap = Arc::new(RwLock::new(external.clone()));

    middleware
        .before_agent(&mut LocalToolsState::new(Arc::clone(&local)))
        .await
        .unwrap();
    middleware
        .before_reason_catalog(&mut LocalToolsState::new(Arc::clone(&local)))
        .await
        .unwrap();

    let current = local.read();
    for (name, original) in external {
        let winner = current.get(&name).unwrap();
        assert!(
            Arc::ptr_eq(winner, &original),
            "{name} must keep the first winner"
        );
        assert_eq!(winner.mcp_server_name(), Some("system-server"));
    }
}

/// [回归测试] 生产路径：宿主级 shared_tools 恒为空（写入点归零），deferred
/// 工具经 `MiddlewareState::local_tools`（每 turn 本地视图）注入，before_agent
/// 必须据此构建索引（issue 2026-08-15-workflow-deferred-tool-missing，已归档至
/// `spec/history/2026-08.md`）。
#[tokio::test]
async fn test_before_agent_builds_index_from_local_tools_when_shared_empty() {
    let index = Arc::new(ToolSearchIndex::new());
    // 宿主级 shared_tools：生产路径下为空表（assemble.rs 建表后无写点）
    let shared = Arc::new(RwLock::new(BTreeMap::new()));
    let mw = ToolSearchMiddleware::new(index.clone(), Arc::clone(&shared));

    // 每 turn 本地视图：含 deferred Workflow 工具（stage_builder 产出语义）
    let mut local = BTreeMap::new();
    local.insert(
        "Workflow".to_string(),
        Arc::new(MockTool::new("Workflow", "Orchestrate multiple agents")) as Arc<dyn BaseTool>,
    );
    local.insert(
        "Read".to_string(),
        Arc::new(MockTool::new("Read", "Read a file").with_direct()) as Arc<dyn BaseTool>,
    );
    local.insert(
        "SearchExtraTools".to_string(),
        Arc::new(SearchExtraTools::new(Arc::clone(&index))) as Arc<dyn BaseTool>,
    );
    local.insert(
        "ExecuteExtraTool".to_string(),
        Arc::new(ExecuteExtraTool::new(Arc::clone(&shared))) as Arc<dyn BaseTool>,
    );
    let local_view: peri_agent::agent::stages::SharedToolMap = Arc::new(RwLock::new(local));

    let mut state = LocalToolsState::new(Arc::clone(&local_view));
    mw.before_agent(&mut state).await.unwrap();

    // 搜索面：Workflow 可被发现
    let results = index.search("select:Workflow", 10);
    assert_eq!(
        results.len(),
        1,
        "宿主表为空时也应从每 turn 本地视图构建 deferred 索引"
    );
    assert_eq!(results[0].name, "Workflow");

    let tools = local_view.read();
    for meta_name in ["SearchExtraTools", "ExecuteExtraTool"] {
        let description = tools.get(meta_name).unwrap().description();
        assert!(description.contains("Read"), "{meta_name}: {description}");
        assert!(
            !description.contains("Write") && !description.contains("WebSearch"),
            "未注册或被过滤的 core tool 不得出现在 direct tool 说明中：{meta_name}: {description}"
        );
        assert!(
            !description.contains("always available"),
            "不得静态宣称 core tools always available：{meta_name}: {description}"
        );
    }

    // prompt 贡献：deferred 列表 + 声明段（direct 工具来自本地视图）
    let contribution = contribution(&mw).unwrap();
    assert!(
        contribution.contains("Workflow"),
        "prompt 贡献应包含 deferred 工具列表"
    );
}

#[tokio::test]
async fn reason_refresh_rebinds_search_and_execute_to_same_dynamic_catalog() {
    use peri_acp_types::{
        dynamic_mcp::{
            DynamicMcpConfig, DynamicMcpInstanceKey, DynamicMcpLogicalKey,
            DynamicMcpServerProjection, DynamicMcpToolCapability, SessionMcpCapabilitySnapshot,
        },
        ports::SessionMcpCapabilityPort,
    };
    use peri_agent::{session::tool_catalog::SessionToolCatalog, tools::ToolContext};

    struct MutableCapability(RwLock<Arc<SessionMcpCapabilitySnapshot>>);
    impl SessionMcpCapabilityPort for MutableCapability {
        fn snapshot(&self) -> Arc<SessionMcpCapabilitySnapshot> {
            Arc::clone(&self.0.read())
        }
    }

    #[derive(Clone)]
    struct CatalogObservingLlm {
        search_result: Arc<StdRwLock<Option<String>>>,
        execute_result: Arc<StdRwLock<Option<String>>>,
    }

    impl CatalogObservingLlm {
        /// 测试直驱入口：用真实工具实例调用中间件注册的两个元工具。
        async fn run(&self, tools: &[&dyn BaseTool]) {
            let search = tools
                .iter()
                .find(|tool| tool.name() == "SearchExtraTools")
                .expect("Reason model request must contain SearchExtraTools");
            let execute = tools
                .iter()
                .find(|tool| tool.name() == "ExecuteExtraTool")
                .expect("Reason model request must contain ExecuteExtraTool");
            let ctx = ToolContext::new(&[], "/tmp");
            *self.search_result.write().unwrap() = Some(
                search
                    .invoke(serde_json::json!({"query": "select:mcp__echo__echo"}), ctx)
                    .await
                    .unwrap(),
            );
            *self.execute_result.write().unwrap() = Some(
                execute
                    .invoke(
                        serde_json::json!({
                            "tool_name": "mcp__echo__echo",
                            "params": {}
                        }),
                        ToolContext::new(&[], "/tmp"),
                    )
                    .await
                    .unwrap(),
            );
        }
    }

    // 本夹具只在测试内直驱（`run`）；不参与子链模型装配。
    #[async_trait]
    impl peri_model::Model for CatalogObservingLlm {
        fn capabilities(&self) -> peri_model::ModelCapabilities {
            peri_model::ModelCapabilities::default()
        }

        fn prepare_stream(
            &self,
            _request: peri_model::ModelRequest,
        ) -> peri_model::ModelResult<peri_model::PreparedModelCall> {
            unimplemented!("本夹具只在测试内直驱")
        }

        async fn stream(
            &self,
            _request: peri_model::ModelRequest,
            _cancellation: tokio_util::sync::CancellationToken,
        ) -> peri_model::ModelResult<peri_model::ModelStream> {
            unimplemented!("本夹具只在测试内直驱")
        }
    }

    let index = Arc::new(ToolSearchIndex::new());
    let shared = Arc::new(RwLock::new(BTreeMap::new()));
    let middleware = ToolSearchMiddleware::new(Arc::clone(&index), Arc::clone(&shared));
    let meta = <ToolSearchMiddleware as Middleware>::collect_tools(&middleware, "/tmp")
        .into_iter()
        .map(|tool| (tool.name().to_string(), Arc::from(tool)))
        .collect::<BTreeMap<String, Arc<dyn BaseTool>>>();
    let working = Arc::new(RwLock::new(meta.clone()));
    middleware
        .before_agent(&mut LocalToolsState::new(Arc::clone(&working)))
        .await
        .unwrap();
    assert!(index.search("select:mcp__echo__echo", 5).is_empty());

    let capability = Arc::new(MutableCapability(RwLock::new(Arc::new(
        SessionMcpCapabilitySnapshot::default(),
    ))));
    let catalog = Arc::new(SessionToolCatalog::new(meta, Some(capability.clone())));
    let instance = DynamicMcpInstanceKey {
        logical: DynamicMcpLogicalKey {
            session_id: "session-a".to_string(),
            server_name: "echo".to_string(),
        },
        incarnation_id: Default::default(),
    };
    *capability.0.write() = Arc::new(SessionMcpCapabilitySnapshot {
        generation: 1,
        servers: BTreeMap::from([(
            "echo".to_string(),
            DynamicMcpServerProjection {
                instance_key: instance.clone(),
                name: "echo".to_string(),
                config: DynamicMcpConfig {
                    command: Some("fixture".to_string()),
                    ..Default::default()
                }
                .canonicalize()
                .unwrap(),
                tool_count: 1,
                resource_count: 0,
            },
        )]),
        tools: BTreeMap::from([(
            "mcp__echo__echo".to_string(),
            DynamicMcpToolCapability {
                instance,
                tool: Arc::new(MockTool::new("mcp__echo__echo", "canonical echo")),
            },
        )]),
    });

    let search_result = Arc::new(StdRwLock::new(None));
    let execute_result = Arc::new(StdRwLock::new(None));
    let snapshot = catalog.refresh().unwrap();
    *working.write() = snapshot.tool_map();
    middleware
        .before_reason_catalog(&mut LocalToolsState::new(Arc::clone(&working)))
        .await
        .unwrap();
    let model = CatalogObservingLlm {
        search_result: Arc::clone(&search_result),
        execute_result: Arc::clone(&execute_result),
    };
    let tools: Vec<_> = working.read().values().cloned().collect();
    model
        .run(&tools.iter().map(|tool| tool.as_ref()).collect::<Vec<_>>())
        .await;

    let search_result = search_result.read().unwrap().clone().unwrap();
    assert!(search_result.contains("mcp__echo__echo"), "{search_result}");
    assert_eq!(
        execute_result.read().unwrap().as_deref(),
        Some("mock"),
        "ExecuteExtraTool must resolve the canonical dynamic target from this Reason snapshot"
    );
}

#[tokio::test]
async fn test_before_agent_binds_index_and_prompt_to_each_turn_snapshot() {
    let index = Arc::new(ToolSearchIndex::new());
    let shared = Arc::new(RwLock::new(BTreeMap::new()));
    let mw = ToolSearchMiddleware::new(Arc::clone(&index), shared);
    let local: peri_agent::agent::stages::SharedToolMap = Arc::new(RwLock::new(BTreeMap::new()));

    local.write().insert(
        "Alpha".to_string(),
        Arc::new(MockTool::new("Alpha", "first deferred tool")) as Arc<dyn BaseTool>,
    );
    mw.before_agent(&mut LocalToolsState::new(Arc::clone(&local)))
        .await
        .unwrap();
    assert_eq!(index.search("select:Alpha", 10).len(), 1);
    assert!(contribution(&mw).unwrap().contains("Alpha"));

    local.write().clear();
    mw.before_agent(&mut LocalToolsState::new(Arc::clone(&local)))
        .await
        .unwrap();
    assert!(index.search("select:Alpha", 10).is_empty());
    assert!(contribution(&mw).is_none());

    local.write().insert(
        "Beta".to_string(),
        Arc::new(MockTool::new("Beta", "replacement deferred tool")) as Arc<dyn BaseTool>,
    );
    mw.before_agent(&mut LocalToolsState::new(Arc::clone(&local)))
        .await
        .unwrap();
    assert!(index.search("select:Alpha", 10).is_empty());
    assert_eq!(index.search("select:Beta", 10).len(), 1);
    let prompt = contribution(&mw).unwrap();
    assert!(!prompt.contains("Alpha"));
    assert!(prompt.contains("Beta"));
}

#[tokio::test]
async fn test_before_agent_rebuilds_same_count_replacement() {
    let index = Arc::new(ToolSearchIndex::new());
    let shared = Arc::new(RwLock::new(BTreeMap::new()));
    let mw = ToolSearchMiddleware::new(Arc::clone(&index), shared);
    let local: peri_agent::agent::stages::SharedToolMap = Arc::new(RwLock::new(BTreeMap::new()));

    local.write().insert(
        "Alpha".to_string(),
        Arc::new(MockTool::new("Alpha", "first deferred tool")) as Arc<dyn BaseTool>,
    );
    mw.before_agent(&mut LocalToolsState::new(Arc::clone(&local)))
        .await
        .unwrap();

    local.write().clear();
    local.write().insert(
        "Beta".to_string(),
        Arc::new(MockTool::new("Beta", "replacement deferred tool")) as Arc<dyn BaseTool>,
    );
    mw.before_agent(&mut LocalToolsState::new(Arc::clone(&local)))
        .await
        .unwrap();

    assert!(index.search("select:Alpha", 10).is_empty());
    assert_eq!(index.search("select:Beta", 10).len(), 1);
    let prompt = contribution(&mw).unwrap();
    assert!(!prompt.contains("Alpha"));
    assert!(prompt.contains("Beta"));
}

/// 构造含声明工具的测试组件：deferred（CronRegister/mcp） + direct（Read）。
fn build_declaring_components() -> (
    Arc<ToolSearchIndex>,
    Arc<RwLock<BTreeMap<String, Arc<dyn BaseTool>>>>,
) {
    let (index, shared) = build_test_components();
    shared.write().insert(
        "Read".to_string(),
        Arc::new(
            MockTool::new("Read", "Read a file")
                .with_direct()
                .with_prompt_declaration(
                    "Read a file → `{{name}}` ({{title}}). Use `{{name}}` for file content, not `cat`/`head`/`tail`.",
                ),
        ) as Arc<dyn BaseTool>,
    );
    (index, shared)
}

/// [2.5.6-声明段] 声明段与 deferred 列表共存：deferred 在前、`\n\n` 分隔
/// （design v2 §2.5.2 合并策略），既有 deferred 列表提示不回归。
#[tokio::test]
async fn test_before_agent_merges_deferred_list_and_declarations() {
    let (index, shared) = build_declaring_components();
    let mw = ToolSearchMiddleware::new(index, shared);

    let mut state = peri_agent::agent::state::AgentState::new("/tmp");
    mw.before_agent(&mut state).await.unwrap();

    let contribution = contribution(&mw).unwrap();
    // deferred 列表保留（既有行为回归，middleware_test.rs:85-104 语义）
    assert!(
        contribution.contains("CronRegister"),
        "prompt 贡献应包含延迟工具列表"
    );
    // 声明段渲染：title 走 name 派生路径 → "Read"
    assert!(
        contribution.contains(
            "Read a file → `Read` (Read). Use `Read` for file content, not `cat`/`head`/`tail`."
        ),
        "声明段应渲染占位符：{contribution}"
    );
    // 拼接顺序：deferred 列表在前、声明段在后
    let list_pos = contribution.find("CronRegister").unwrap();
    let decl_pos = contribution.find("Read a file").unwrap();
    assert!(list_pos < decl_pos, "deferred 列表应位于声明段之前");
}

/// [2.5.6-缓存保护] 注入不同 cwd 断言声明段输出不变（不引用会话数据）。
#[tokio::test]
async fn test_declaration_output_independent_of_cwd() {
    let (index, shared) = build_declaring_components();
    let mw = ToolSearchMiddleware::new(index, shared);

    let mut state1 = peri_agent::agent::state::AgentState::new("/tmp");
    mw.before_agent(&mut state1).await.unwrap();
    let first = contribution(&mw).unwrap();
    assert!(first.contains("Read a file"), "首轮应包含声明段");

    let mut state2 = peri_agent::agent::state::AgentState::new("/different");
    mw.before_agent(&mut state2).await.unwrap();
    assert_eq!(
        contribution(&mw).unwrap(),
        first,
        "cwd 变化不得影响声明段输出（design v2 §2.5.4 静态字段纪律）"
    );
}

/// [2.5.6-默认行为] 未实现 prompt_declaration 的工具不产生声明段；
/// deferred-only 工具集下贡献与既有行为一致（仅列表，无追加分隔）。
#[tokio::test]
async fn test_before_agent_no_declarations_without_prompt_declaration() {
    let (index, shared) = build_test_components(); // 全部 deferred，无声明
    let mw = ToolSearchMiddleware::new(index, shared);

    let mut state = peri_agent::agent::state::AgentState::new("/tmp");
    mw.before_agent(&mut state).await.unwrap();

    let contribution = contribution(&mw).unwrap();
    assert!(contribution.contains("CronRegister"));
    assert!(
        !contribution.contains("Read a file"),
        "未声明工具不得出现在声明段"
    );
}

/// [回归测试] H5：`visible_to_model() == false`（app-only）的工具不得进入
/// 任何模型面投影——deferred 索引 / deferred 列表 / direct 声明段 / 元工具描述。
///
/// 历史背景：direct 面已按 `is_direct() && visible_to_model()` 排除，但
/// ToolSearch 索引与元工具描述只按 `is_direct()` 分界，server 声明的
/// 「仅 app 可用」能力仍可被检索并执行（`tool_search/middleware.rs`）。
/// app-only 工具在宿主注册表中保持可派发（App 合法调用路径不由本修复改变），
/// 本用例只锁模型面不可见。
#[tokio::test]
async fn app_only_tools_are_absent_from_index_lists_and_declarations() {
    let index = Arc::new(ToolSearchIndex::new());
    let mut shared = BTreeMap::new();
    // app-only deferred：索引与 deferred 列表都必须看不到。
    shared.insert(
        "AppOnlyDeferred".to_string(),
        Arc::new(
            MockTool::new("AppOnlyDeferred", "app only deferred capability").with_model_invisible(),
        ) as Arc<dyn BaseTool>,
    );
    // app-only direct：direct 名单与声明段都必须看不到。
    shared.insert(
        "AppOnlyDirect".to_string(),
        Arc::new(
            MockTool::new("AppOnlyDirect", "app only direct capability")
                .with_direct()
                .with_model_invisible()
                .with_prompt_declaration("app-only-directive {{name}} AppOnlyDirectMarker"),
        ) as Arc<dyn BaseTool>,
    );
    // 正向对照：模型可见的 deferred / direct 必须照旧出现。
    shared.insert(
        "VisibleDeferred".to_string(),
        Arc::new(MockTool::new("VisibleDeferred", "model visible deferred")) as Arc<dyn BaseTool>,
    );
    shared.insert(
        "VisibleDirect".to_string(),
        Arc::new(
            MockTool::new("VisibleDirect", "model visible direct")
                .with_direct()
                .with_prompt_declaration("visible-directive {{name}} VisibleDirectMarker"),
        ) as Arc<dyn BaseTool>,
    );
    let shared = Arc::new(RwLock::new(shared));
    let mw = ToolSearchMiddleware::new(Arc::clone(&index), Arc::clone(&shared));
    // 生产顺序：宿主先 merge `collect_tools` 的 meta 工具，再在 rebind 时替换。
    for tool in <ToolSearchMiddleware as Middleware>::collect_tools(&mw, "/tmp") {
        let name = tool.name().to_string();
        shared.write().insert(name, Arc::from(tool));
    }

    let mut state = peri_agent::agent::state::AgentState::new("/tmp");
    mw.before_agent(&mut state).await.unwrap();

    // 1) 检索投影：精确名（select:）不得命中 app-only 工具；关键词查询即使
    // 匹配其描述/正文也不得返回（关键词面按分数排序返回候选，不保证空集）。
    for query in ["select:AppOnlyDeferred", "select:AppOnlyDirect"] {
        assert!(
            index.search(query, 10).is_empty(),
            "app-only 工具不得进入检索投影（query={query}）"
        );
    }
    let keyword_hits: Vec<String> = index
        .search("app only capability", 10)
        .into_iter()
        .map(|result| result.name)
        .collect();
    assert!(
        !keyword_hits
            .iter()
            .any(|name| name == "AppOnlyDeferred" || name == "AppOnlyDirect"),
        "关键词检索不得返回 app-only 工具: {keyword_hits:?}"
    );
    assert_eq!(
        index.search("select:VisibleDeferred", 10).len(),
        1,
        "模型可见的 deferred 工具必须保持可检索（正向用例）"
    );

    // 2) deferred 列表与 direct 声明段：app-only 名字一律不出现。
    let prompt = contribution(&mw).expect("模型可见工具存在时必须有贡献");
    assert!(
        prompt.contains("VisibleDeferred") && prompt.contains("VisibleDirectMarker"),
        "正向对照工具必须仍在投影内: {prompt}"
    );
    for hidden in ["AppOnlyDeferred", "AppOnlyDirectMarker"] {
        assert!(
            !prompt.contains(hidden),
            "app-only 工具 {hidden} 不得出现在模型面投影: {prompt}"
        );
    }

    // 3) 元工具描述：direct 工具清单只含模型可见项。
    let search_description = shared.read()["SearchExtraTools"].description().to_string();
    assert!(
        search_description.contains("VisibleDirect"),
        "模型可见 direct 工具应出现在 SearchExtraTools 描述: {search_description}"
    );
    assert!(
        !search_description.contains("AppOnlyDirect"),
        "app-only direct 工具不得出现在元工具描述: {search_description}"
    );

    // 4) app-only 工具仍在宿主注册表内（App 合法调用路径不被本修复删除）。
    assert!(
        shared.read().contains_key("AppOnlyDeferred"),
        "app-only 工具必须保留在共享注册表中，不得从底层注册删除"
    );
}
