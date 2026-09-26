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
            source: None,
            url: None,
            skills_capable: false,
            channel_capable: false,
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

    use crate::middleware::{FilesystemMiddleware, TerminalMiddleware};
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
    // 6 filesystem：Read/Write/Edit/Glob/Grep/folder_operations
    for t in FilesystemMiddleware::build_tools("/tmp") {
        tools.push(Arc::from(t));
    }
    // 1 execution：Bash
    for t in TerminalMiddleware::build_tools("/tmp") {
        tools.push(Arc::from(t));
    }
    // 2 web + 1 artifact：builtin 实例的真实 direct 桥（effective name，A9）
    tools.extend(builtin_direct_bridges());
    // 3 interaction：Agent/AskUserQuestion/TodoWrite
    tools.push(Arc::new(SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new(|_: Option<&str>| -> Box<dyn ReactLLM + Send + Sync> {
            unreachable!("声明测试不触发子 agent")
        }),
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
    tools.push(Arc::new(SkillTool::new(Arc::clone(&cached))));
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

/// [2.5.6-全量渲染] 真实 direct 工具集（14 Core + 3 Meta）声明渲染后无
/// 未识别占位符残留（含 `{{` 未闭合检测）。
#[test]
fn test_all_real_tool_declarations_render_without_placeholder_residue() {
    use crate::tool_search::core_tools::{
        EXECUTE_EXTRA_TOOL_NAME, SEARCH_EXTRA_TOOLS_NAME, TOOL_AGENT, TOOL_ASK_USER, TOOL_BASH,
        TOOL_DISCOVER_SKILLS, TOOL_EDIT, TOOL_FOLDER_OPS, TOOL_GLOB, TOOL_GREP, TOOL_READ,
        TOOL_SKILL, TOOL_TODO, TOOL_WRITE,
    };
    // 三个已迁移工具的模型面名字（迁移前是裸名 `WebFetch`/`WebSearch`/`artifact`）
    let web_fetch = declared("web", "WebFetch").effective_name;
    let web_search = declared("web", "WebSearch").effective_name;
    let artifact = declared("artifact", "artifact").effective_name;
    let tools = build_real_direct_tools();

    // 覆盖完整性：CORE_TOOL_NAMES 14 个 + 3 个 Meta 全部就位且全部声明
    let expected: &[&str] = &[
        TOOL_READ,
        TOOL_WRITE,
        TOOL_EDIT,
        TOOL_GLOB,
        TOOL_GREP,
        TOOL_FOLDER_OPS,
        TOOL_BASH,
        web_fetch,
        web_search,
        TOOL_AGENT,
        TOOL_ASK_USER,
        TOOL_TODO,
        TOOL_SKILL,
        TOOL_DISCOVER_SKILLS,
        SEARCH_EXTRA_TOOLS_NAME,
        EXECUTE_EXTRA_TOOL_NAME,
        artifact,
    ];
    for name in expected {
        let tool = tools
            .iter()
            .find(|t| t.name() == *name)
            .unwrap_or_else(|| panic!("direct 工具集缺少 {name}"));
        assert!(
            tool.prompt_declaration().is_some() || builtin_declaration(name).is_some(),
            "{name} 必须有声明模板（工具实现或 builtin 声明表，全量迁移完成）"
        );
    }

    let rendered = collect_declarations(&tools).expect("真实工具集声明段非空");
    assert!(
        !rendered.contains("{{"),
        "声明段不得残留占位符（含 {{ 未闭合）：\n{rendered}"
    );

    // A9（迁移守护）：三个 builtin 工具的声明条目必须仍在，且渲染出的名字是
    // effective name —— 裸名不得作为声明条目出现。
    //
    // 现场证据（`--nocapture` 可见，供 V-04 抄录）：迁移后声明段里的 builtin 条目。
    println!("[S-02 A9 现场] 迁移后声明段中的 builtin 条目（effective name）：");
    for line in rendered.lines().filter(|l| l.contains("mcp__")) {
        println!("  {line}");
    }
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
        assert!(
            !rendered.contains(&format!("`{bare}`")),
            "声明条目不得再以裸名 {bare} 出现：\n{rendered}"
        );
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
