use serde_json::json;

use super::*;

struct MockTool {
    name_str: String,
    desc_str: String,
    params: serde_json::Value,
}

impl MockTool {
    fn new(name: &str, desc: &str) -> Self {
        Self {
            name_str: name.to_string(),
            desc_str: desc.to_string(),
            params: json!({"type": "object", "properties": {}}),
        }
    }
}

#[async_trait::async_trait]
impl BaseTool for MockTool {
    fn name(&self) -> &str {
        &self.name_str
    }
    fn description(&self) -> &str {
        &self.desc_str
    }
    fn parameters(&self) -> serde_json::Value {
        self.params.clone()
    }
    async fn invoke(
        &self,
        _input: serde_json::Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok("mock".to_string())
    }
}

fn make_mock_tools() -> Vec<Arc<dyn BaseTool>> {
    vec![
        Arc::new(MockTool::new(
            "CronRegister",
            "Register a cron scheduled task",
        )),
        Arc::new(MockTool::new("CronList", "List all cron tasks")),
        Arc::new(MockTool::new("CronRemove", "Remove a cron task by ID")),
        Arc::new(MockTool::new(
            "mcp__slack__send_message",
            "Send a message to Slack channel",
        )),
        Arc::new(MockTool::new(
            "mcp__github__create_issue",
            "Create a GitHub issue",
        )),
    ]
}

#[test]
fn test_build_index() {
    let index = ToolSearchIndex::new();
    let tools = make_mock_tools();
    index.build(tools);
    assert_eq!(index.list_names().len(), 5);
}

#[test]
fn test_keyword_search() {
    let index = ToolSearchIndex::new();
    let tools = make_mock_tools();
    index.build(tools);

    let results = index.search("cron create", 3);
    assert!(!results.is_empty());
    // CronRegister should rank high
    assert!(results[0].name.contains("Cron"));
}

#[test]
fn test_tfidf_search() {
    let index = ToolSearchIndex::new();
    let tools = make_mock_tools();
    index.build(tools);

    let results = index.search("schedule task", 3);
    assert!(!results.is_empty());
}

#[test]
fn test_hybrid_search() {
    let index = ToolSearchIndex::new();
    let tools = make_mock_tools();
    index.build(tools);

    let results = index.search("+slack message", 5);
    // Required word "slack" should filter to only slack tools
    assert!(results
        .iter()
        .all(|r| r.name.to_lowercase().contains("slack")));
}

#[test]
fn test_get_tool() {
    let index = ToolSearchIndex::new();
    let tools = make_mock_tools();
    index.build(tools);

    assert!(index.get_tool("CronRegister").is_some());
    assert!(index.get_tool("NonExistent").is_none());
}

#[test]
fn test_format_deferred_list() {
    let index = ToolSearchIndex::new();
    let tools = make_mock_tools();
    index.build(tools);

    let list = index.format_deferred_list();
    assert!(list.contains("CronRegister"));
    // MCP 工具不出现在 Deferred Tools 段（避免 system prompt 不稳定导致缓存失效）
    assert!(!list.contains("mcp__slack__send_message"));
    assert!(!list.contains("mcp__github__create_issue"));
}

#[test]
fn test_total_count() {
    let index = ToolSearchIndex::new();
    assert_eq!(index.total_count(), 0);

    let tools = make_mock_tools();
    index.build(tools);
    assert_eq!(index.total_count(), 5);
}

#[test]
fn test_select_exact_match() {
    let index = ToolSearchIndex::new();
    let tools = make_mock_tools();
    index.build(tools);

    let results = index.search("select:CronRegister,CronList", 10);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].name, "CronRegister");
    assert_eq!(results[1].name, "CronList");
}

#[test]
fn test_select_partial_miss() {
    let index = ToolSearchIndex::new();
    let tools = make_mock_tools();
    index.build(tools);

    let results = index.search("select:CronRegister,NonExistent", 10);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name, "CronRegister");
}

#[test]
fn test_select_empty_result() {
    let index = ToolSearchIndex::new();
    let tools = make_mock_tools();
    index.build(tools);

    let results = index.search("select:NonExistent", 10);
    assert!(results.is_empty());
}

/// 同名工具注册覆盖语义：后注册的实例覆盖先注册的。
/// build() 通过 HashMap::insert 注册工具，name 重复时静默覆盖。
#[test]
fn test_duplicate_name_overwrites() {
    let index = ToolSearchIndex::new();
    let tools: Vec<Arc<dyn BaseTool>> = vec![
        Arc::new(MockTool::new("CronRegister", "Version A - original")),
        Arc::new(MockTool::new("CronRegister", "Version B - overwritten")),
    ];
    index.build(tools);

    // 验证只有 1 个工具注册（非 2 个）
    assert_eq!(index.total_count(), 1, "同名工具应覆盖，total_count=1");

    // 验证 get_tool 返回后注册的实例（Version B）
    let tool = index
        .get_tool("CronRegister")
        .expect("get_tool 应能查找到 CronRegister");
    assert_eq!(
        tool.description(),
        "Version B - overwritten",
        "后注册的工具应覆盖前一个，description 应为 Version B"
    );
}

/// 覆盖后 search 仍然正常工作，不会 panic 或返回错误结果。
#[test]
fn test_duplicate_name_search_still_works() {
    let index = ToolSearchIndex::new();
    let tools: Vec<Arc<dyn BaseTool>> = vec![
        Arc::new(MockTool::new("CronRegister", "Register cron tasks v1")),
        Arc::new(MockTool::new("CronRegister", "Register cron tasks v2")),
        Arc::new(MockTool::new("CronList", "List all cron tasks")),
    ];
    index.build(tools);

    // search 不应 panic
    let results = index.search("cron", 5);
    assert_eq!(
        results.len(),
        2,
        "search 应返回 2 个结果（CronRegister + CronList），实际: {}",
        results.len()
    );

    // CronRegister 的描述应为覆盖后的版本
    let cron_reg = results
        .iter()
        .find(|r| r.name == "CronRegister")
        .expect("应能找到 CronRegister");
    assert!(
        cron_reg.description.contains("v2"),
        "search 结果应反映覆盖后的描述，实际: {}",
        cron_reg.description
    );
}

// ─── P2-2: content_version 版本号 ────────────────────────────────────────────

/// build() 应递增 content_version
#[test]
fn test_build_increments_content_version() {
    let index = ToolSearchIndex::new();
    assert_eq!(index.content_version(), 0, "初始版本应为 0");

    let tools: Vec<Arc<dyn BaseTool>> =
        vec![Arc::new(MockTool::new("CronRegister", "v1 description"))];
    index.build(tools);
    assert_eq!(index.content_version(), 1, "第一次 build 后版本应为 1");

    let tools2: Vec<Arc<dyn BaseTool>> =
        vec![Arc::new(MockTool::new("CronRegister", "v1 description"))];
    index.build(tools2);
    assert_eq!(
        index.content_version(),
        2,
        "第二次 build（即使内容相同）版本应递增到 2"
    );
}

/// P2-2: 同 count 但不同 content 应触发版本号变化
///
/// 这正是简单 count 比对会漏掉的场景：MCP 重连后工具数量相同但描述/schema 已变。
#[test]
fn test_cached_prompt_rebuilds_on_content_change_same_count() {
    let index = ToolSearchIndex::new();

    // 第一次构建：1 个工具，描述为 v1
    let tools_v1: Vec<Arc<dyn BaseTool>> =
        vec![Arc::new(MockTool::new("CronRegister", "v1 description"))];
    index.build(tools_v1);
    let version_after_v1 = index.content_version();
    assert_eq!(version_after_v1, 1);

    // 第二次构建：同 count（1 个工具），但描述改为 v2
    // 旧 count 比对会判定 count 没变、不重建——版本号能识别这种变化
    let tools_v2: Vec<Arc<dyn BaseTool>> = vec![Arc::new(MockTool::new(
        "CronRegister",
        "v2 description - schema changed",
    ))];
    index.build(tools_v2);
    let version_after_v2 = index.content_version();

    assert_ne!(
        version_after_v1, version_after_v2,
        "同 count 但 content 变化时，content_version 必须递增（否则 middleware 无法检测到变化）"
    );
}

/// set_cached_prompt 应记录当前的 content_version
#[test]
fn test_set_cached_prompt_records_version() {
    let index = ToolSearchIndex::new();

    assert_eq!(index.cached_prompt_version(), None, "初始时无 cached 版本");

    let tools: Vec<Arc<dyn BaseTool>> = vec![Arc::new(MockTool::new("CronRegister", "desc"))];
    index.build(tools);
    let version_after_build = index.content_version();

    index.set_cached_prompt("## Deferred Tools\n...".to_string());
    assert_eq!(
        index.cached_prompt_version(),
        Some(version_after_build),
        "set_cached_prompt 后版本号应与当前 content_version 一致"
    );
}

/// 重新 build 后，cached_prompt_version 应落后于 content_version
#[test]
fn test_cached_prompt_becomes_stale_after_rebuild() {
    let index = ToolSearchIndex::new();

    let tools_v1: Vec<Arc<dyn BaseTool>> = vec![Arc::new(MockTool::new("CronRegister", "v1"))];
    index.build(tools_v1);
    index.set_cached_prompt("v1 prompt".to_string());
    let cached_version = index.cached_prompt_version().expect("已设置 cached");

    // 再次 build（内容变化）—— cached_prompt 仍是 v1 的
    let tools_v2: Vec<Arc<dyn BaseTool>> =
        vec![Arc::new(MockTool::new("CronRegister", "v2 - changed"))];
    index.build(tools_v2);

    assert_ne!(
        index.content_version(),
        cached_version,
        "rebuild 后 content_version 应递增"
    );
    assert_eq!(
        index.cached_prompt_version(),
        Some(cached_version),
        "cached_prompt_version 仍停留在上一次 set_cached_prompt 的版本（middleware 负责检测并重建）"
    );
}
