use super::*;
use crate::session::FrozenContext;

#[test]
fn test_v2_context_has_null_llm_by_default() {
    let cwd: Arc<str> = Arc::from("/tmp");
    let frozen = FrozenContext::builder().build();
    let session = Session::new(cwd, frozen, None);
    let turn = session.start_turn();
    let ctx = StageContext::builder(turn, session.transcript(), session.queue().clone()).build();
    assert_eq!(ctx.runtime.llm.model_name(), "null");
}

/// MetaHarness（设计 §2.5 关闭语义防御面）：session/turn 级工具视图——
/// disabled session 的本地视图不得看到共享表中残留的 middleware 工具；
/// enabled session 视图不受影响。
///
/// 2026-08-15 职责拆分（spec/issues/2026-08-15-permission-hitl-split.md）：
/// AskUserQuestion 移入 HumanInTheLoopMiddleware 的 collect_tools 并纳入
/// `MIDDLEWARE_TOOL_NAMES` 剔除面；宿主级 shared_tools 生产路径写入点
/// 归零。本测试保留"共享表含 middleware 工具名"的人工防御面场景（模拟
/// 将来注册面变化），其中 AskUserQuestion 现与其他 middleware 工具同
/// 语义：disabled 链无持有者 → 剔除。
#[test]
fn test_build_session_tool_view_isolates_disabled_sessions() {
    use peri_acp_types::meta_harness::MIDDLEWARE_TOOL_NAMES;

    fn fake_tool(name: &'static str) -> Arc<dyn BaseTool> {
        Arc::new(NamedTool(name))
    }

    // 基础共享表：人工构造"共享表含 middleware 工具名"的防御面场景
    //（模拟将来注册面变化）+ AskUserQuestion（现同为 middleware 工具）。
    let base: Arc<RwLock<BTreeMap<String, Arc<dyn BaseTool>>>> =
        Arc::new(RwLock::new(BTreeMap::new()));
    {
        let mut map = base.write();
        map.insert("WebFetch".to_string(), fake_tool("WebFetch"));
        map.insert("WebSearch".to_string(), fake_tool("WebSearch"));
        map.insert("Bash".to_string(), fake_tool("Bash"));
        map.insert("AskUserQuestion".to_string(), fake_tool("AskUserQuestion"));
    }
    assert!(MIDDLEWARE_TOOL_NAMES.contains(&"WebFetch"));
    assert!(MIDDLEWARE_TOOL_NAMES.contains(&"Bash"));
    assert!(MIDDLEWARE_TOOL_NAMES.contains(&"AskUserQuestion"));

    // disabled session：当前链无 Web/提问工具 → 视图不得含残留条目
    let middleware_tools: Vec<Box<dyn BaseTool>> = vec![];
    let view = build_session_tool_view(&base, middleware_tools);
    let view_map = view.read();
    assert!(!view_map.contains_key("WebFetch"), "残留 WebFetch 泄漏");
    assert!(!view_map.contains_key("WebSearch"), "残留 WebSearch 泄漏");
    assert!(!view_map.contains_key("Bash"), "残留 Bash 泄漏");
    assert!(
        !view_map.contains_key("AskUserQuestion"),
        "残留 AskUserQuestion 泄漏（关闭提问通道后必须消失）"
    );
    drop(view_map);

    // enabled session：当前链含 Web/提问工具 → 视图含（覆盖为基础实例或新实例）
    let middleware_tools: Vec<Box<dyn BaseTool>> = vec![
        Box::new(NamedTool("WebFetch")),
        Box::new(NamedTool("AskUserQuestion")),
    ];
    let view = build_session_tool_view(&base, middleware_tools);
    assert!(view.read().contains_key("WebFetch"));
    assert!(view.read().contains_key("AskUserQuestion"));

    // 基础共享表不受视图构造影响（跨 session 隔离不改写全局表）
    assert!(base.read().contains_key("WebFetch"));
}

/// 测试桩工具（仅 name 有效）。
struct NamedTool(&'static str);
#[async_trait::async_trait]
impl BaseTool for NamedTool {
    fn name(&self) -> &str {
        self.0
    }
    fn description(&self) -> &str {
        ""
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::Value::Null
    }
    fn is_direct(&self) -> bool {
        true
    }
    async fn invoke(
        &self,
        _input: serde_json::Value,
        _ctx: crate::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok(String::new())
    }
}
