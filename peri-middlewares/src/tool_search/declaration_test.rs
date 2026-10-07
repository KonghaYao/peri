//! 声明收集器测试（design v2 §2.5.6：渲染完整性 / 稳定性 / 排序）。

use std::sync::Arc;

use super::*;

/// 局部测试工具：可配置 prompt_declaration / namespace / title。
struct DeclaringTool {
    name_str: String,
    desc_str: String,
    title_str: Option<String>,
    ns_str: Option<String>,
    declaration: Option<String>,
}

impl DeclaringTool {
    fn new(name: &str, desc: &str) -> Self {
        Self {
            name_str: name.to_string(),
            desc_str: desc.to_string(),
            title_str: None,
            ns_str: None,
            declaration: None,
        }
    }

    fn with_namespace(mut self, ns: &str) -> Self {
        self.ns_str = Some(ns.to_string());
        self
    }

    fn with_title(mut self, title: &str) -> Self {
        self.title_str = Some(title.to_string());
        self
    }

    fn with_declaration(mut self, declaration: &str) -> Self {
        self.declaration = Some(declaration.to_string());
        self
    }
}

#[async_trait::async_trait]
impl BaseTool for DeclaringTool {
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
        Ok("ok".to_string())
    }
    fn title(&self) -> Option<&str> {
        self.title_str.as_deref()
    }
    fn namespace(&self) -> Option<&str> {
        self.ns_str.as_deref()
    }
    fn prompt_declaration(&self) -> Option<String> {
        self.declaration.clone()
    }
}

fn tool(t: DeclaringTool) -> Arc<dyn BaseTool> {
    Arc::new(t)
}

// -- 渲染完整性 ----------------------------------------------------------------

/// [2.5.6-渲染完整性] 合法模板（仅 4 占位符）渲染后无 `{{` 残留。
#[test]
fn test_render_known_placeholders_no_residue() {
    let t = tool(
        DeclaringTool::new("Read", "Read a file from disk")
            .with_title("Read")
            .with_namespace("filesystem")
            .with_declaration(
                "Read a file → `{{name}}` ({{title}}) in [{{namespace}}]. {{description}}",
            ),
    );
    let rendered = collect_declarations(&[t]).unwrap();
    assert!(
        !rendered.contains("{{"),
        "合法模板渲染后不得残留占位符：{rendered}"
    );
    assert!(rendered.contains("`Read` (Read) in [filesystem]"));
    assert!(rendered.contains("Read a file from disk"));
}

/// [2.5.6-渲染完整性] 未识别占位符原样保留（design v2 §2.5.3 宽松保留）。
#[test]
fn test_render_unknown_placeholder_preserved() {
    let t =
        tool(DeclaringTool::new("Read", "desc").with_declaration("Use `{{name}}` via {{unknown}}"));
    let rendered = collect_declarations(&[t]).unwrap();
    assert_eq!(rendered, "Use `Read` via {{unknown}}");
}

/// [回归锁] description 值含字面 `{{ }}`（JSON/泛型示例）时不被二次替换。
///
/// 纪律：渲染必须单遍扫描——模板占位符仅从模板文本替换，插入的
/// description 值原样透传、永不被重新扫描（链式 `str::replace` 会在此
/// 场景二次替换，design v2 §2.5.3 行 258-259）。
#[test]
fn test_render_description_literal_braces_not_double_replaced() {
    let t = tool(
        DeclaringTool::new("Write", "JSON example: `{{x}}`; generic: `{{description}}`")
            .with_declaration("Use `{{name}}` — {{description}}"),
    );
    let rendered = collect_declarations(&[t]).unwrap();
    assert_eq!(
        rendered, "Use `Write` — JSON example: `{{x}}`; generic: `{{description}}`",
        "description 内的字面 {{x}}/{{description}} 必须原样保留；仅模板层占位符被替换"
    );
}

/// 模板无闭合 `}}` 时剩余文本原样保留（不 panic）。
#[test]
fn test_render_unclosed_placeholder_preserved() {
    let t = tool(DeclaringTool::new("Read", "desc").with_declaration("Use `{{name}}` and {{oops"));
    let rendered = collect_declarations(&[t]).unwrap();
    assert_eq!(rendered, "Use `Read` and {{oops");
}

// -- 排序 ----------------------------------------------------------------------

/// [2.5.6-排序] 乱序输入按 (namespace, name) 字典序输出；namespace None 按空串排最前。
#[test]
fn test_collect_declarations_sorted_by_namespace_then_name() {
    let tools = vec![
        tool(
            DeclaringTool::new("Read", "d")
                .with_namespace("web")
                .with_declaration("{{name}}:web"),
        ),
        tool(DeclaringTool::new("Agent", "d").with_declaration("{{name}}:none")),
        tool(
            DeclaringTool::new("Bash", "d")
                .with_namespace("web")
                .with_declaration("{{name}}:web"),
        ),
        tool(
            DeclaringTool::new("Grep", "d")
                .with_namespace("filesystem")
                .with_declaration("{{name}}:fs"),
        ),
        tool(
            DeclaringTool::new("Glob", "d")
                .with_namespace("filesystem")
                .with_declaration("{{name}}:fs"),
        ),
    ];
    let rendered = collect_declarations(&tools).unwrap();
    assert_eq!(
        rendered, "Agent:none\nGlob:fs\nGrep:fs\nBash:web\nRead:web",
        "namespace 字典序（None 最前）→ 组内 name 字典序"
    );
}

// -- 稳定性 --------------------------------------------------------------------

/// [2.5.6-稳定性] 同输入两次收集字节级相等（防排序/缓存回归）。
#[test]
fn test_collect_declarations_stable_across_calls() {
    let tools = vec![
        tool(
            DeclaringTool::new("Read", "d")
                .with_namespace("web")
                .with_declaration("{{name}} ({{title}})"),
        ),
        tool(
            DeclaringTool::new("Grep", "d")
                .with_namespace("filesystem")
                .with_declaration("{{name}} ({{title}})"),
        ),
    ];
    let first = collect_declarations(&tools).unwrap();
    let second = collect_declarations(&tools).unwrap();
    assert_eq!(first, second);
}

// -- 空集与默认行为 -------------------------------------------------------------

/// 无任何工具声明时返回 None（调用方保持无声明段语义）。
#[test]
fn test_collect_declarations_empty_returns_none() {
    let no_decl = tool(DeclaringTool::new("Read", "d"));
    assert_eq!(collect_declarations(&[no_decl]), None);
    assert_eq!(collect_declarations(&[]), None);
}

/// [回归测试] 原名 MCP 工具可与 builtin 同名；声明模板只属于实际 builtin bridge。
#[test]
fn test_external_raw_builtin_name_does_not_inherit_builtin_declaration() {
    let external = tool(DeclaringTool::new("WebSearch", "external search"));
    assert_eq!(collect_declarations(&[external]), None);
}

// -- 全量渲染守护（design v2 §2.5.5/2.5.6：全量迁移完成态） ---------------------

/// builtin 实例（web / artifact）的**真实 direct 桥**。
///
/// 与生产同源：假 pool（两个 builtin 实例的已连接 client）+ 类型化构造
/// `build_typed_tool_bridges`（IF-D13 生效点 ⇒ 声明为 direct 的工具带 `direct` 标记）。
/// 返回的是生产对象 `McpToolBridge`，其 `name()` 即 effective name。
fn builtin_direct_bridges() -> Vec<Arc<dyn BaseTool>> {
    /// 已连接的假 MCP client（工具名按注册表声明传入；不含真实连接与凭据）。
    fn connected_handle(server: &str, tools: &[&str]) -> Arc<crate::mcp::McpClientHandle> {
        Arc::new(crate::mcp::McpClientHandle {
            name: server.to_string(),
            version: None,
            cache_version: None,
            peer: None,
            tools: tools
                .iter()
                .map(|tool| {
                    serde_json::from_value(serde_json::json!({
                        "name": tool,
                        "description": "builtin tool",
                        "inputSchema": { "type": "object", "properties": {} }
                    }))
                    .unwrap()
                })
                .collect(),
            resources: vec![],
            status: crate::mcp::ClientStatus::Connected,
            oauth_status: Default::default(),
            source: Some(peri_acp_types::plugin::ConfigSource::Builtin {
                instance: server.to_string(),
            }),
            url: None,
            skills_capable: false,
        })
    }

    let pool = Arc::new(crate::mcp::McpClientPool::new_empty());
    for instance in peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES {
        let tool_names: Vec<&str> = instance
            .tools
            .iter()
            .map(|tool| tool.original_name)
            .collect();
        pool.clients.write().insert(
            instance.name.to_string(),
            connected_handle(instance.name, &tool_names),
        );
    }
    crate::mcp::tool_bridge::build_typed_tool_bridges(&pool)
        .into_iter()
        .map(|bridge| Arc::new(bridge) as Arc<dyn BaseTool>)
        .collect()
}

/// 真实装配面 direct 工具集：14 Core + 3 Meta（其中 2 web + 1 artifact 由 builtin
/// 实例的 MCP 桥提供）。
///
/// 与 ToolSearchMiddleware.before_agent 的声明段数据源同构（shared_tools 中
/// is_direct() = true 的工具；Meta 三件套与 Core 由同一装配面注册）。各工具
/// 使用真实构造器，保证声明模板即线上模板。
///
/// v4-part-2（A9）：`WebFetch` / `WebSearch` / `artifact` 不再是 middleware 静态工具，
/// 三者改为 **builtin 实例的真实 direct 桥**（effective name，见
/// [`builtin_direct_bridges`]）——声明段仍必须为它们产出条目。
fn build_real_direct_tools() -> Vec<Arc<dyn BaseTool>> {
    use std::collections::BTreeMap;

    use crate::skills::tools::{DiscoverSkillsTool, SkillTool};
    use crate::skills::SkillMetadata;
    use crate::subagent::SubAgentTool;
    use crate::tool_search::{ExecuteExtraTool, SearchExtraTools, ToolSearchIndex};
    use crate::tools::{AskUserTool, TodoWriteTool};
    use parking_lot::RwLock as PLRwLock;
    use peri_agent::agent::react::ReactLLM;
    use peri_agent::interaction::{InteractionContext, InteractionResponse, UserInteractionBroker};

    /// 声明测试不触发交互——request 永不调用。
    struct NoopBroker;
    #[async_trait::async_trait]
    impl UserInteractionBroker for NoopBroker {
        async fn request(&self, _ctx: InteractionContext) -> InteractionResponse {
            unreachable!("声明测试不触发用户交互")
        }
    }

    let mut tools: Vec<Arc<dyn BaseTool>> = Vec::new();
    // 7 workspace：Read/Write/Edit/Glob/Grep/folder_operations/Bash —— v4-part-4 W3-C1
    // 后裸名工具实现**不再**是 direct 提供面（`FilesystemMiddleware` / `TerminalMiddleware`
    // 已删除），声明条目由下面的 builtin 真实 direct 桥以 `mcp__workspace__*` 提供。
    // 2 web + 1 artifact + 7 workspace：builtin 实例的真实 direct 桥（effective name，A9）
    tools.extend(builtin_direct_bridges());
    // 3 interaction：Agent/AskUserQuestion/TodoWrite
    tools.push(Arc::new(SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new(
            |_: Option<&str>| -> peri_agent::session::subagent::SubagentLlmSource {
                unreachable!("声明测试不触发子 agent")
            },
        ),
        "/tmp".to_string(),
    )));
    tools.push(Arc::new(AskUserTool::new(Arc::new(NoopBroker))));
    let (tx, _rx) = tokio::sync::mpsc::channel::<Vec<crate::tools::TodoItem>>(8);
    tools.push(Arc::new(TodoWriteTool::new(
        tx,
        Arc::new(tokio::sync::Mutex::new(crate::tools::TodoState::default())),
    )));
    // 2 skills：SkillTool/DiscoverSkillsTool
    let cached: Arc<std::sync::RwLock<Option<Vec<SkillMetadata>>>> =
        Arc::new(std::sync::RwLock::new(None));
    tools.push(Arc::new(SkillTool::new(Arc::clone(&cached), None)));
    tools.push(Arc::new(DiscoverSkillsTool::new(cached)));
    // 3 meta：SearchExtraTools/ExecuteExtraTool（artifact 由 builtin 桥提供，见上）
    let index = Arc::new(ToolSearchIndex::new());
    let shared: Arc<PLRwLock<BTreeMap<String, Arc<dyn BaseTool>>>> =
        Arc::new(PLRwLock::new(BTreeMap::new()));
    tools.push(Arc::new(SearchExtraTools::new(Arc::clone(&index))));
    tools.push(Arc::new(ExecuteExtraTool::new(Arc::clone(&shared))));
    tools
}

/// 声明表里某个已迁移工具的 effective name（字面量只在声明表声明一份）。
fn declared(
    instance: &str,
    original_name: &str,
) -> &'static peri_acp_types::builtin_mcp::BuiltinMcpTool {
    peri_acp_types::builtin_mcp::find(instance)
        .and_then(|declared| {
            declared
                .tools
                .iter()
                .find(|tool| tool.original_name == original_name)
        })
        .expect("builtin 声明表应声明该 (实例, 原始工具名)")
}

/// [2.5.6-全量渲染] 真实 direct 工具集（7 workspace + 3 Meta + 其余 Core）声明渲染后无
/// 未识别占位符残留（含 `{{` 未闭合检测）。
///
/// v4-part-4 W3-C1：7 个文件/终端工具的声明条目由裸名改为 builtin `workspace` 实例的
/// `mcp__workspace__*`（effective name）——本用例同时断言 XOR（裸名不得出现在 direct 面）。
#[test]
fn test_all_real_tool_declarations_render_without_placeholder_residue() {
    use crate::tool_search::core_tools::{
        EXECUTE_EXTRA_TOOL_NAME, SEARCH_EXTRA_TOOLS_NAME, TOOL_AGENT, TOOL_ASK_USER,
        TOOL_DISCOVER_SKILLS, TOOL_SKILL, TOOL_TODO,
    };
    // 三个已迁移工具的模型面名字（迁移前是裸名 `WebFetch`/`WebSearch`/`artifact`）
    let web_fetch = declared("web", "WebFetch").effective_name;
    let web_search = declared("web", "WebSearch").effective_name;
    let artifact = declared("artifact", "artifact").effective_name;
    // 7 个已迁移 workspace 工具的模型面名字（迁移前是裸名 `Read`/…/`Bash`），
    // 逐项从注册表派生（不写第二份清单）。
    let workspace = peri_acp_types::builtin_mcp::find("workspace")
        .expect("workspace 必须是已实现实例（W3-A 冻结注册表）");
    let tools = build_real_direct_tools();

    // 覆盖完整性：7 个 workspace + web/artifact 3 个 + 其余 Core 与 3 个 Meta 全部就位且全部声明
    let mut expected: Vec<&str> = workspace
        .tools
        .iter()
        .map(|tool| tool.effective_name)
        .collect();
    expected.extend([
        web_fetch,
        web_search,
        artifact,
        TOOL_AGENT,
        TOOL_ASK_USER,
        TOOL_TODO,
        TOOL_SKILL,
        TOOL_DISCOVER_SKILLS,
        SEARCH_EXTRA_TOOLS_NAME,
        EXECUTE_EXTRA_TOOL_NAME,
    ]);
    for name in &expected {
        let tool = tools
            .iter()
            .find(|t| t.name() == *name)
            .unwrap_or_else(|| panic!("direct 工具集缺少 {name}"));
        assert!(
            tool.prompt_declaration().is_some()
                || declaration_template(tool.as_ref(), name).is_some(),
            "{name} 必须有声明模板（工具实现或 builtin 声明表，全量迁移完成）"
        );
    }
    // system MCP 的七个 workspace 工具以原名进入 direct 工具面。
    for tool in workspace.tools {
        assert!(
            tools.iter().any(|t| t.name() == tool.original_name),
            "原名 {} 必须出现在 direct 工具集",
            tool.original_name
        );
    }

    let rendered = collect_declarations(&tools).expect("真实工具集声明段非空");
    assert!(
        !rendered.contains("{{"),
        "声明段不得残留占位符（含 {{ 未闭合）：\n{rendered}"
    );

    // 三个 builtin 工具仍贡献声明条目，声明使用原名。
    for migrated in [web_fetch, web_search, artifact] {
        assert!(
            rendered.contains(&format!("`{migrated}`")),
            "迁移后声明段必须仍含 {migrated} 的条目：\n{rendered}"
        );
    }
    for migrated_declared in [
        declared("web", "WebFetch"),
        declared("web", "WebSearch"),
        declared("artifact", "artifact"),
    ] {
        let bare = migrated_declared.original_name;
        // 声明文本必须来自声明表模板：模板里最长的**非占位符片段**逐字出现在渲染结果中
        // （若有人就地硬编码别的文本，这里立刻红）。
        let template = migrated_declared
            .prompt_declaration
            .expect("声明表必须携带迁移工具的声明模板（A9）");
        let fragment = longest_literal_fragment(template);
        assert!(
            rendered.contains(fragment),
            "{bare} 的声明文本必须来自声明表模板（片段 {fragment:?} 缺失）:\n{rendered}"
        );
    }
}

/// 模板中最长的非 `{{占位符}}` 片段（trim 后），用于断言文本来源。
fn longest_literal_fragment(template: &str) -> &str {
    let mut longest = "";
    let mut rest = template;
    loop {
        let Some(start) = rest.find("{{") else {
            if rest.trim().len() > longest.trim().len() {
                longest = rest;
            }
            break;
        };
        let fragment = &rest[..start];
        if fragment.trim().len() > longest.trim().len() {
            longest = fragment;
        }
        rest = match rest[start..].find("}}") {
            Some(end) => &rest[start + end + 2..],
            None => "",
        };
    }
    longest.trim()
}

// ─── v4-part-3 wave 2 声明段守护（V 矩阵第 12 行） ─────────────────────────────

/// wave 2 的**被验实例**（A4 的冻结对象）。
///
/// 只固定被验实例身份，工具清单/名字/数量一律由注册表条目派生——注册表增删工具时本用例
/// 自动跟随，不出现第二份清单。
const WAVE2_INSTANCES: [&str; 1] = ["cron"];

/// wave 1（web / artifact）三工具的模板**逐字快照**：任何字节漂移都必须在这里变红。
///
/// 快照值 = 注册表当前字面量（现场取自 `--nocapture` 的渲染输出，见 `[W2 declaration]`
/// 打印的同一次运行）；它不是"第二事实源"，而是"逐字不变"这条断言的比对基准。
const WAVE1_FROZEN_TEMPLATES: [(&str, &str, &str); 3] = [
    (
        "artifact",
        "artifact",
        "Share generated HTML/Markdown as a public link → `{{name}}` ({{title}}). \
         The link expires in 7 or 30 days; use for dashboards, reports, and prototypes you want to share.",
    ),
    (
        "web",
        "WebFetch",
        "Fetch a URL you have reason to trust → `{{name}}` ({{title}}). Retrieve URLs with `{{name}}`, never `curl` via Bash.",
    ),
    (
        "web",
        "WebSearch",
        "Look up current information beyond your knowledge → `{{name}}` ({{title}}). Query the web for recent or external facts.",
    ),
];

/// wave 1 三条 builtin 工具在**真实渲染段**里的条目（逐字快照，顺序 = (namespace, name)
/// 字典序：三条都无 namespace，故按 effective name 排序）。
const WAVE1_FROZEN_RENDERED_LINES: [&str; 3] = [
    "Fetch a URL you have reason to trust → `WebFetch` (Web Fetch). Retrieve URLs with `WebFetch`, never `curl` via Bash.",
    "Look up current information beyond your knowledge → `WebSearch` (Web Search). Query the web for recent or external facts.",
    "Share generated HTML/Markdown as a public link → `artifact` (Artifact). The link expires in 7 or 30 days; use for dashboards, reports, and prototypes you want to share.",
];

/// wave 2 工具（实例名, 声明）——由注册表派生，不含本文件硬编码的工具名。
fn wave2_declared_tools() -> Vec<(
    &'static str,
    &'static peri_acp_types::builtin_mcp::BuiltinMcpTool,
)> {
    WAVE2_INSTANCES
        .iter()
        .flat_map(|instance| {
            peri_acp_types::builtin_mcp::find(instance)
                .unwrap_or_else(|| panic!("{instance} 必须在 builtin 注册表内"))
                .tools
                .iter()
                .map(move |tool| (*instance, tool))
        })
        .collect()
}

/// wave 2 三工具在**生产桥形态**下的工具对象（`McpToolBridge`，`name()` = effective name）。
///
/// 夹具复用 [`builtin_direct_bridges`]（同一假 pool：注册表全部实例的已连接 client +
/// 生产 `build_typed_tool_bridges`），再按注册表冻结的 effective name 过滤——不重写
/// effective name 计算，也不另造一份假 client。
fn wave2_bridged_tools() -> Vec<Arc<dyn BaseTool>> {
    let names: Vec<&str> = wave2_declared_tools()
        .iter()
        .map(|(_, tool)| tool.effective_name)
        .collect();
    let mut bridges: Vec<Arc<dyn BaseTool>> = builtin_direct_bridges()
        .into_iter()
        .filter(|bridge| names.contains(&bridge.name()))
        .collect();
    bridges.sort_by_key(|bridge| bridge.name().to_string());
    bridges
}

/// [v4-part-3 A4 / V 矩阵第 12 行] wave 2 三工具**不得**对声明段有任何贡献：
/// 注册表 `prompt_declaration` 全 `None`、声明表的渲染查表全 `None`、真实 deferred 桥
/// 经 `collect_declarations` 渲染贡献为 0；同时 wave 1（web / artifact）模板与渲染段
/// **逐字不变**。
#[test]
fn wave2_has_no_prompt_declaration() {
    let wave2_declared = wave2_declared_tools();

    // ① 三个 None：计数由注册表派生（计划冻结值 = cron 三 = 3）。
    assert_eq!(
        wave2_declared.len(),
        3,
        "计划冻结：wave 2 恰三工具（cron 三）；实际 {}",
        wave2_declared.len()
    );
    let none_count = wave2_declared
        .iter()
        .filter(|(_, tool)| tool.prompt_declaration.is_none())
        .count();
    assert_eq!(
        none_count,
        wave2_declared.len(),
        "wave 2 工具一律无声明模板（A4：声明段零变化）"
    );
    for (instance, tool) in &wave2_declared {
        assert!(
            !tool.direct,
            "{instance}/{}: A4 冻结 wave 2 工具一律 deferred",
            tool.original_name
        );
        assert!(
            builtin_declaration(instance, tool.original_name).is_none(),
            "{}: 声明段的渲染查表必须为 None（贡献 0）",
            tool.effective_name
        );
    }

    // ② 真实桥形态的渲染贡献为 0（走 `declaration.rs` 的渲染函数本身）。
    let wave2 = wave2_bridged_tools();
    let mut bridged_names: Vec<&str> = wave2.iter().map(|bridge| bridge.name()).collect();
    bridged_names.sort_unstable();
    let mut declared_names: Vec<&str> = wave2_declared
        .iter()
        .map(|(_, tool)| tool.effective_name)
        .collect();
    declared_names.sort_unstable();
    assert_eq!(
        bridged_names, declared_names,
        "桥名字必须就是注册表冻结的三条 effective name（否则 ③ 的 None 是空转）"
    );
    assert!(
        wave2.iter().all(|bridge| !bridge.is_direct()),
        "wave 2 桥必须保持 deferred（A4）"
    );
    let added = collect_declarations(&wave2);
    assert!(
        added.is_none(),
        "wave 2 三工具对声明段的新增贡献必须为 0；实际: {added:?}"
    );

    // ③ 加法形态：把真实 wave 2 桥追加到真实 direct 工具面，声明段逐字节不变。
    let direct = build_real_direct_tools();
    let wave1_rendered = collect_declarations(&direct).expect("真实 direct 工具集声明段非空");
    let mut extended = build_real_direct_tools();
    extended.extend(wave2.iter().map(Arc::clone));
    let extended_rendered = collect_declarations(&extended).expect("追加 wave 2 后声明段仍非空");
    assert_eq!(
        extended_rendered, wave1_rendered,
        "追加 wave 2 三工具不得改变声明段任何字节"
    );

    // ④ wave 1 模板 + 渲染段逐字不变（快照比对）。
    for (instance, original_name, frozen) in WAVE1_FROZEN_TEMPLATES {
        let tool = declared(instance, original_name);
        assert_eq!(
            tool.prompt_declaration,
            Some(frozen),
            "{instance}/{original_name}: wave 1 声明模板必须逐字不变"
        );
    }
    // 快照比对只覆盖 wave 1（web / artifact）的两实例三条目：W3-C1 后声明段里还有
    // workspace 的 7 条 `mcp__workspace__*`（其模板正文含**交叉引用裸名**，属 S6 已登记
    // 项），因此按 wave 1 的 effective name 过滤后再逐字比对——既不放松 wave 1 的
    // 逐字强度，也不把 workspace 的条目混进快照。
    let wave1_effectives: Vec<&str> = WAVE1_FROZEN_TEMPLATES
        .iter()
        .map(|(instance, original_name, _)| declared(instance, original_name).effective_name)
        .collect();
    let wave1_lines: Vec<&str> = wave1_rendered
        .lines()
        .filter(|line| wave1_effectives.iter().any(|name| line.contains(name)))
        .collect();
    let wave1_equal = wave1_lines == WAVE1_FROZEN_RENDERED_LINES;
    assert!(
        wave1_equal,
        "wave 1 三条 builtin 声明条目必须逐字不变；\n实际: {wave1_lines:#?}\n期望: {WAVE1_FROZEN_RENDERED_LINES:#?}"
    );

    // 先 assert 再打印（`--nocapture` 抄录用）。
    println!("[W2 declaration] none={none_count} added=0 wave1_equal={wave1_equal}");
}

/// [2.5.6-全量稳定] 真实工具集两次收集字节级相同（防排序/缓存回归）。
#[test]
fn test_all_real_tool_declarations_byte_stable_across_calls() {
    let tools = build_real_direct_tools();
    let first = collect_declarations(&tools).unwrap();
    let second = collect_declarations(&tools).unwrap();
    assert_eq!(first, second);
}

/// [2.5.6-迁移守护] 声明段渲染输出与 05_using_tools.md 剩余内容无逐字重复行。
///
/// 全量迁移完成态：05 保留通用纪律、Bash discipline 与工具选择原则骨架小节
/// （"Tool selection principles"，不含工具名与逐工具细节），逐工具指引的
/// 单一事实源是声明段（工具代码）。
#[test]
fn test_declarations_no_verbatim_line_overlap_with_05() {
    const SECTION_05: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../peri-acp/prompts/sections/05_using_tools.md"
    ));
    // 05 无工具条目残留（删条守护）
    assert!(
        !SECTION_05.contains("## Choosing the right tool"),
        "05 不应残留工具条目小节（全量迁移完成）"
    );
    assert!(
        !SECTION_05.contains("**Read a file**"),
        "05 不应残留 Read 手写条目（全量迁移完成）"
    );

    let rendered = collect_declarations(&build_real_direct_tools()).unwrap();
    let decl_lines: Vec<&str> = rendered.lines().map(str::trim).collect();
    for line in SECTION_05.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        assert!(
            !decl_lines.contains(&trimmed),
            "05 行与声明段逐字重复（同一事实双份维护）：{trimmed:?}"
        );
    }
}
