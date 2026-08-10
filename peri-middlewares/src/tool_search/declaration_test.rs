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
