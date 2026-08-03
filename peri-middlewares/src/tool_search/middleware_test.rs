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
}

impl MockTool {
    fn new(name: &str, desc: &str) -> Self {
        Self {
            name_str: name.to_string(),
            desc_str: desc.to_string(),
        }
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
