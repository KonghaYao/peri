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
