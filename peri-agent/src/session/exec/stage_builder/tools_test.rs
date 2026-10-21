use super::*;
use async_trait::async_trait;

/// 测试桩工具（仅 `name` 有效）。
struct NamedTool(&'static str);

#[async_trait]
impl BaseTool for NamedTool {
    fn name(&self) -> &str {
        self.0
    }
    fn description(&self) -> &str {
        "stage builder predicate fixture"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    async fn invoke(
        &self,
        _input: serde_json::Value,
        _ctx: crate::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Err("fixture".into())
    }
}

/// v4-part-2（A7/IF-D7 B 节）+ v4-part-4（W3-C1）反向断言：**全部**已迁移裸名
/// （web / artifact 三枚、cron 三枚、workspace 七枚）**不再**被防御谓词剔除。
///
/// 迁移后这些裸名不再是 middleware 静态工具（`MIDDLEWARE_TOOL_NAMES` 已删除），
/// 因此共享表里出现的同名工具只能是**非 middleware 路径**注册的合法工具；
/// 当前链未注册同名工具时它们必须留在本地视图（而**仍在表内**的 middleware 静态工具
/// 照旧剔除——反例侧由 `TodoWrite` 承担，它仍是 `TodoMiddleware` 的静态工具）。
///
/// 名字面一律从 builtin 注册表派生（不写第二份清单）：注册表增删工具时本用例自动跟随。
#[test]
fn migrated_naked_names_are_no_longer_excluded() {
    use peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES;
    use peri_acp_types::meta_harness::MIDDLEWARE_TOOL_NAMES;

    // 反例侧：仍未迁移的 middleware 静态工具（TodoMiddleware）。
    const STILL_EXCLUDED: &str = "TodoWrite";

    let base: Arc<RwLock<BTreeMap<String, Arc<dyn BaseTool>>>> =
        Arc::new(RwLock::new(BTreeMap::new()));
    {
        let mut map = base.write();
        for original in BUILTIN_MCP_INSTANCES
            .iter()
            .flat_map(|instance| instance.tools.iter().map(|tool| tool.original_name))
        {
            map.insert(original.to_string(), Arc::new(NamedTool(original)));
        }
        map.insert(
            STILL_EXCLUDED.to_string(),
            Arc::new(NamedTool(STILL_EXCLUDED)),
        );
    }

    // 前提：注册表内全部裸名已不在剔除面内，反例名仍在（否则本测试没有区分力）。
    let declared: Vec<&str> = BUILTIN_MCP_INSTANCES
        .iter()
        .flat_map(|instance| instance.tools.iter().map(|tool| tool.original_name))
        .collect();
    assert!(
        declared.len() >= 13,
        "注册表应至少含 4 个实例的 13 项工具（含 workspace 7 项）: {declared:?}"
    );
    for original in &declared {
        assert!(
            !MIDDLEWARE_TOOL_NAMES.contains(original),
            "已迁移裸名 {original} 不应再出现在 MIDDLEWARE_TOOL_NAMES 内"
        );
    }
    assert!(
        MIDDLEWARE_TOOL_NAMES.contains(&STILL_EXCLUDED),
        "{STILL_EXCLUDED} 仍在剔除面内（反例侧）"
    );

    // 当前链无任何 middleware 工具：已迁移裸名全部保留，反例名被剔除。
    let view = build_session_tool_view(&base, vec![]);
    let view_map = view.read();
    for original in &declared {
        assert!(
            view_map.contains_key(*original),
            "非 middleware 路径注册的裸名 {original} 不得被本谓词剔除"
        );
    }
    assert!(
        !view_map.contains_key(STILL_EXCLUDED),
        "仍在剔除面内的 middleware 静态工具照旧被剔除"
    );
}

#[test]
fn session_view_keeps_first_injected_name_case_insensitively() {
    let first: Arc<dyn BaseTool> = Arc::new(NamedTool("Read"));
    let base = RwLock::new(BTreeMap::from([("Read".into(), first.clone())]));
    let view = build_session_tool_view(&base, vec![Box::new(NamedTool("read"))]);
    assert_eq!(view.read().len(), 1);
    assert!(Arc::ptr_eq(&view.read()["Read"], &first));
    let view = build_session_tool_view(
        &RwLock::new(BTreeMap::new()),
        vec![Box::new(NamedTool("lookup")), Box::new(NamedTool("LOOKUP"))],
    );
    assert_eq!(view.read().len(), 1);
    assert!(view.read().contains_key("lookup"));
}
