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
    decl: Option<String>,
}

impl MockTool {
    fn new(name: &str, desc: &str) -> Self {
        Self {
            name_str: name.to_string(),
            desc_str: desc.to_string(),
            direct: false,
            decl: None,
        }
    }

    /// 标记为 LLM 可见（direct）工具。
    fn with_direct(mut self) -> Self {
        self.direct = true;
        self
    }

    /// 声明提示词层模板（design v2 §2.5.1 prompt_declaration）。
    fn with_prompt_declaration(mut self, declaration: &str) -> Self {
        self.decl = Some(declaration.to_string());
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

    assert!(
        tools.len() >= 3,
        "expected at least 3 tools (meta + deferred)"
    );
    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert!(names.contains(&"SearchExtraTools"));
    assert!(names.contains(&"ExecuteExtraTool"));
    assert!(names.contains(&"artifact"), "expected artifact tool");
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
