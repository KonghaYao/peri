use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use peri_acp_types::mcp_control::{SessionMcpCatalogPort, SessionMcpCatalogSnapshot};
use serde_json::json;

use super::*;
use crate::tools::ToolContext;

struct NamedTool {
    name: String,
    description: String,
    aliases: &'static [&'static str],
}

impl NamedTool {
    fn new(name: &str, description: &str) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            aliases: &[],
        }
    }

    fn with_alias(name: &str, alias: &'static str) -> Self {
        Self {
            name: name.to_string(),
            description: name.to_string(),
            aliases: Box::leak(vec![alias].into_boxed_slice()),
        }
    }
}

#[async_trait]
impl BaseTool for NamedTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> serde_json::Value {
        json!({"type": "object"})
    }

    fn aliases(&self) -> &[&str] {
        self.aliases
    }

    async fn invoke(
        &self,
        _input: serde_json::Value,
        _ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.description.clone())
    }
}

struct MutableCapability(parking_lot::RwLock<Arc<SessionMcpCatalogSnapshot>>);

impl MutableCapability {
    fn publish(&self, snapshot: SessionMcpCatalogSnapshot) {
        *self.0.write() = Arc::new(snapshot);
    }
}

impl SessionMcpCatalogPort for MutableCapability {
    fn snapshot(&self) -> Arc<SessionMcpCatalogSnapshot> {
        Arc::clone(&self.0.read())
    }
}

#[test]
fn conflicting_base_aliases_are_reported_without_weakening_production_constructor() {
    let first: Arc<dyn BaseTool> = Arc::new(NamedTool::with_alias("first", "shared"));
    let second: Arc<dyn BaseTool> = Arc::new(NamedTool::with_alias("second", "shared"));
    let tools = BTreeMap::from([("first".to_string(), first), ("second".to_string(), second)]);

    assert!(matches!(
        SessionToolCatalog::try_new(tools.clone(), None),
        Err(CatalogRefreshError::AliasConflict)
    ));
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        SessionToolCatalog::new(tools, None)
    }))
    .is_err());
}

#[test]
fn request_local_tool_binding_does_not_mutate_session_publisher() {
    let old: Arc<dyn BaseTool> = Arc::new(NamedTool::new("example", "old"));
    let catalog = SessionToolCatalog::new(
        BTreeMap::from([("example".to_string(), Arc::clone(&old))]),
        None,
    );
    let published = catalog.snapshot();
    let replacement: Arc<dyn BaseTool> = Arc::new(NamedTool::new("example", "new"));

    let pinned = catalog
        .pin_working_tools(&BTreeMap::from([("example".to_string(), replacement)]))
        .unwrap();

    assert_eq!(published.tools["example"].tool.description(), "old");
    assert_eq!(pinned.tools["example"].tool.description(), "new");
    assert!(Arc::ptr_eq(&published, &catalog.snapshot()));
}

// ─── 启动闸门静态提交（B-05）─────────────────────────────────────────────────

/// 静态 MCP bridge 测试桩（名称与 `McpToolBridge` 同形）。
struct BridgeTool {
    name: String,
    server: Option<String>,
    direct: bool,
    aliases: &'static [&'static str],
}

impl BridgeTool {
    /// 名称与声明 server 可独立指定，用于构造"身份不一致"的越权候选。
    fn bridge(name: &str, server: &str) -> Self {
        Self {
            name: name.to_string(),
            server: Some(server.to_string()),
            direct: false,
            aliases: &[],
        }
    }

    fn direct(mut self) -> Self {
        self.direct = true;
        self
    }

    fn with_alias(mut self, alias: &'static str) -> Self {
        self.aliases = Box::leak(vec![alias].into_boxed_slice());
        self
    }
}

#[async_trait]
impl BaseTool for BridgeTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.name
    }

    fn parameters(&self) -> serde_json::Value {
        json!({"type": "object"})
    }

    fn aliases(&self) -> &[&str] {
        self.aliases
    }

    fn mcp_server_name(&self) -> Option<&str> {
        self.server.as_deref()
    }

    fn is_direct(&self) -> bool {
        self.direct
    }

    async fn invoke(
        &self,
        _input: serde_json::Value,
        _ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        Ok(String::new())
    }
}

#[test]
fn pool_catalog_replaces_mcp_entries_and_preserves_pinned_reason() {
    let old: Arc<dyn BaseTool> = Arc::new(BridgeTool::bridge("mcp__docs__lookup", "docs"));
    let replacement: Arc<dyn BaseTool> = Arc::new(BridgeTool::bridge("mcp__docs__lookup", "docs"));
    let source = Arc::new(MutableCapability(parking_lot::RwLock::new(Arc::new(
        SessionMcpCatalogSnapshot {
            generation: 1,
            tools: vec![old.clone()],
        },
    ))));
    let catalog = SessionToolCatalog::new(
        BTreeMap::from([(
            "Read".into(),
            Arc::new(NamedTool::new("Read", "core")) as Arc<dyn BaseTool>,
        )]),
        Some(source.clone()),
    );
    let pinned = catalog
        .pin_working_tools(&catalog.snapshot().tool_map())
        .unwrap();
    source.publish(SessionMcpCatalogSnapshot {
        generation: 2,
        tools: vec![],
    });
    let stopped = catalog.refresh().unwrap();
    assert!(
        stopped.tools.contains_key("Read"),
        "关闭 MCP 不得移除 core 工具"
    );
    assert!(
        !stopped.tools.contains_key(old.name()),
        "下一 Reason 不再发布已关闭工具"
    );
    assert!(
        Arc::ptr_eq(&pinned.tools[old.name()].tool, &old),
        "在途 Reason 固定旧对象"
    );
    source.publish(SessionMcpCatalogSnapshot {
        generation: 3,
        tools: vec![replacement.clone()],
    });
    let restarted = catalog.refresh().unwrap();
    assert!(Arc::ptr_eq(
        &restarted.tools[replacement.name()].tool,
        &replacement
    ));
    assert!(
        Arc::ptr_eq(&pinned.tools[old.name()].tool, &old),
        "重启不能重定向旧 dispatch"
    );
}

#[test]
fn pool_catalog_reapplies_child_filter_on_each_generation() {
    let source = Arc::new(MutableCapability(parking_lot::RwLock::new(Arc::new(
        SessionMcpCatalogSnapshot {
            generation: 1,
            tools: vec![],
        },
    ))));
    let catalog = SessionToolCatalog::with_filter(
        BTreeMap::new(),
        Some(source.clone()),
        ToolFilterPolicy::canonical(Some(vec!["mcp__docs__read".into()]), vec![]),
    );
    source.publish(SessionMcpCatalogSnapshot {
        generation: 2,
        tools: vec![
            Arc::new(BridgeTool::bridge("mcp__docs__read", "docs")),
            Arc::new(BridgeTool::bridge("mcp__docs__write", "docs")),
        ],
    });
    let published = catalog.refresh().unwrap();
    assert_eq!(
        published.tools.keys().collect::<Vec<_>>(),
        vec!["mcp__docs__read"]
    );
    source.publish(SessionMcpCatalogSnapshot {
        generation: 3,
        tools: vec![],
    });
    assert!(catalog.refresh().unwrap().tools.is_empty());
}

#[test]
fn pool_catalog_rejects_missing_source_without_partial_publication() {
    let source = Arc::new(MutableCapability(parking_lot::RwLock::new(Arc::new(
        SessionMcpCatalogSnapshot {
            generation: 1,
            tools: vec![],
        },
    ))));
    let catalog = SessionToolCatalog::new(BTreeMap::new(), Some(source.clone()));
    let before = catalog.snapshot();
    source.publish(SessionMcpCatalogSnapshot {
        generation: 2,
        tools: vec![Arc::new(NamedTool::new("mcp__docs__fake", "unbound"))],
    });
    assert_eq!(
        catalog.refresh().unwrap_err(),
        CatalogRefreshError::InvalidMcpSource {
            tool: "mcp__docs__fake".into(),
        }
    );
    assert!(
        Arc::ptr_eq(&before, &catalog.snapshot()),
        "非法来源不得部分发布目录"
    );
    source.publish(SessionMcpCatalogSnapshot {
        generation: 2,
        tools: vec![Arc::new(BridgeTool::bridge("mcp__docs__read", "docs"))],
    });
    assert!(
        catalog
            .refresh()
            .unwrap()
            .tools
            .contains_key("mcp__docs__read"),
        "相同代次可重试"
    );
}

fn startup_update(
    tools: Vec<Arc<dyn BaseTool>>,
    required: &[(&str, &str, &str)],
) -> StartupToolUpdate {
    StartupToolUpdate {
        tools,
        required: required
            .iter()
            .map(|(server, original, effective)| StartupRequiredTool {
                server_name: (*server).to_string(),
                original_tool_name: (*original).to_string(),
                effective_tool_name: (*effective).to_string(),
            })
            .collect(),
    }
}

/// static base 的可观察投影（提交失败时用于断言 base 未被部分修改）。
fn base_names(catalog: &SessionToolCatalog) -> Vec<String> {
    catalog.state.read().base_tools.keys().cloned().collect()
}

fn startup_catalog(
    base: BTreeMap<String, Arc<dyn BaseTool>>,
    capability: Option<Arc<dyn SessionMcpCatalogPort>>,
) -> SessionToolCatalog {
    SessionToolCatalog::new(base, capability)
}

fn core_and_static_base() -> BTreeMap<String, Arc<dyn BaseTool>> {
    BTreeMap::from([
        (
            "Read".to_string(),
            Arc::new(NamedTool::new("Read", "core")) as Arc<dyn BaseTool>,
        ),
        (
            "mcp__system__lookup".to_string(),
            Arc::new(BridgeTool::bridge("mcp__system__lookup", "system")) as Arc<dyn BaseTool>,
        ),
        (
            "mcp__other__keep".to_string(),
            Arc::new(BridgeTool::bridge("mcp__other__keep", "other")) as Arc<dyn BaseTool>,
        ),
    ])
}

#[test]
fn startup_commit_publishes_required_direct_tool_into_static_base() {
    let catalog = startup_catalog(core_and_static_base(), None);
    assert!(
        catalog
            .snapshot()
            .direct_definitions
            .iter()
            .all(|definition| definition.name != "mcp__system__lookup"),
        "提交前 required 未提升 direct"
    );

    let prepared: Arc<dyn BaseTool> =
        Arc::new(BridgeTool::bridge("mcp__system__lookup", "system").direct());
    let published = catalog
        .replace_static_mcp_tools(startup_update(
            vec![Arc::clone(&prepared)],
            &[("system", "lookup", "mcp__system__lookup")],
        ))
        .unwrap();

    assert!(
        published
            .direct_definitions
            .iter()
            .any(|definition| definition.name == "mcp__system__lookup"),
        "required 工具必须直接可达模型"
    );
    assert!(
        Arc::ptr_eq(&published.tools["mcp__system__lookup"].tool, &prepared),
        "整批替换必须用新 bridge 对象，而不是保留旧 deferred 对象"
    );
    assert_eq!(
        published.tools["mcp__system__lookup"].source,
        ToolSource::Mcp("system".to_string())
    );
    assert!(published.tools.contains_key("Read"), "core 工具不受影响");
    assert_eq!(
        published.tools["mcp__other__keep"].source,
        ToolSource::Mcp("other".to_string()),
        "未参与本次提交的 server 保持原条目"
    );
    assert!(
        catalog
            .refresh()
            .unwrap()
            .direct_definitions
            .iter()
            .any(|definition| definition.name == "mcp__system__lookup"),
        "Reason 边界 refresh 后 required 仍可达"
    );
}

#[test]
fn startup_commit_rejects_required_tool_filtered_by_policy() {
    let catalog = SessionToolCatalog::with_filter(
        core_and_static_base(),
        None,
        Arc::new(|tool| tool.name() != "mcp__system__lookup"),
    );
    let before = catalog.snapshot();
    let prepared: Arc<dyn BaseTool> =
        Arc::new(BridgeTool::bridge("mcp__system__lookup", "system").direct());
    let error = catalog
        .replace_static_mcp_tools(startup_update(
            vec![Arc::clone(&prepared)],
            &[("system", "lookup", "mcp__system__lookup")],
        ))
        .unwrap_err();

    assert_eq!(
        error,
        CatalogRefreshError::RequiredToolUnavailable {
            server: "system".to_string(),
            tool: "mcp__system__lookup".to_string(),
        }
    );
    assert!(
        Arc::ptr_eq(&before, &catalog.snapshot()),
        "整批失败不得有部分发布"
    );
    assert_eq!(
        base_names(&catalog),
        vec![
            "Read".to_string(),
            "mcp__other__keep".to_string(),
            "mcp__system__lookup".to_string()
        ]
    );
}

#[test]
fn startup_commit_rejects_tool_without_static_mcp_identity() {
    let catalog = startup_catalog(core_and_static_base(), None);

    let core_named: Arc<dyn BaseTool> = Arc::new(NamedTool::new("Read", "rogue"));
    let error = catalog
        .replace_static_mcp_tools(startup_update(vec![Arc::clone(&core_named)], &[]))
        .unwrap_err();
    assert_eq!(
        error,
        CatalogRefreshError::InvalidStartupSource {
            tool: "Read".to_string()
        }
    );

    let declared_but_core_named: Arc<dyn BaseTool> =
        Arc::new(BridgeTool::bridge("Read", "system").direct());
    catalog
        .replace_static_mcp_tools(startup_update(
            vec![declared_but_core_named],
            &[("system", "Read", "Read")],
        ))
        .unwrap();
    assert!(
        !catalog.snapshot().tools["Read"].tool.is_direct(),
        "启动提交不得把 core 工具身份换成 MCP bridge"
    );
}

#[test]
fn startup_commit_skips_cross_server_takeover() {
    let catalog = startup_catalog(core_and_static_base(), None);
    let foreign: Arc<dyn BaseTool> =
        Arc::new(BridgeTool::bridge("mcp__system__lookup", "other").direct());
    catalog
        .replace_static_mcp_tools(startup_update(vec![foreign], &[]))
        .unwrap();

    assert_eq!(
        catalog.snapshot().tools["mcp__system__lookup"].source,
        ToolSource::Mcp("system".to_string()),
        "跨 server 覆盖必须被拒绝且不修改既有条目"
    );
}

#[test]
fn startup_commit_skips_alias_conflict() {
    let base = BTreeMap::from([
        (
            "mcp__other__keep".to_string(),
            Arc::new(BridgeTool::bridge("mcp__other__keep", "other").with_alias("shared"))
                as Arc<dyn BaseTool>,
        ),
        (
            "Read".to_string(),
            Arc::new(NamedTool::new("Read", "core")) as Arc<dyn BaseTool>,
        ),
    ]);
    let catalog = startup_catalog(base, None);
    let conflicting: Arc<dyn BaseTool> =
        Arc::new(BridgeTool::bridge("mcp__system__tool", "system").with_alias("shared"));
    catalog
        .replace_static_mcp_tools(startup_update(vec![conflicting], &[]))
        .unwrap();

    assert!(!catalog.snapshot().tools.contains_key("mcp__system__tool"));
    assert_eq!(
        base_names(&catalog),
        vec!["Read".to_string(), "mcp__other__keep".to_string()]
    );
}

#[test]
fn raw_system_names_keep_first_source_across_refresh_and_case_collisions() {
    let catalog = startup_catalog(BTreeMap::new(), None);
    let first: Arc<dyn BaseTool> = Arc::new(BridgeTool::bridge("lookup", "a").direct());
    let second: Arc<dyn BaseTool> = Arc::new(BridgeTool::bridge("LOOKUP", "b").direct());
    let snapshot = catalog
        .replace_static_mcp_tools(startup_update(
            vec![first.clone(), second.clone()],
            &[("a", "lookup", "lookup"), ("b", "LOOKUP", "LOOKUP")],
        ))
        .unwrap();
    assert_eq!(snapshot.tools.len(), 1);
    assert!(Arc::ptr_eq(&snapshot.tools["lookup"].tool, &first));
    let resolved = crate::tools::DirectToolInvocationResolver
        .resolve_target("LOOKUP", &snapshot.tool_map())
        .unwrap();
    assert!(Arc::ptr_eq(&resolved, &first));

    // A later refresh of the losing source cannot take over the existing name.
    let snapshot = catalog
        .replace_static_mcp_tools(startup_update(vec![second], &[("b", "LOOKUP", "LOOKUP")]))
        .unwrap();
    assert!(Arc::ptr_eq(&snapshot.tools["lookup"].tool, &first));
    let refreshed: Arc<dyn BaseTool> = Arc::new(BridgeTool::bridge("lookup", "a").direct());
    let snapshot = catalog
        .replace_static_mcp_tools(startup_update(
            vec![refreshed.clone()],
            &[("a", "lookup", "lookup")],
        ))
        .unwrap();
    assert!(Arc::ptr_eq(&snapshot.tools["lookup"].tool, &refreshed));
}

#[test]
fn raw_system_required_alias_collision_does_not_fail_startup() {
    let first: Arc<dyn BaseTool> = Arc::new(
        BridgeTool::bridge("execute", "first")
            .with_alias("Shell")
            .direct(),
    );
    let catalog = startup_catalog(BTreeMap::from([("execute".into(), first.clone())]), None);
    let second: Arc<dyn BaseTool> = Arc::new(BridgeTool::bridge("shell", "second").direct());
    let snapshot = catalog
        .replace_static_mcp_tools(startup_update(
            vec![second],
            &[("second", "shell", "shell")],
        ))
        .unwrap();
    assert_eq!(snapshot.tools.len(), 1);
    let resolved = crate::tools::DirectToolInvocationResolver
        .resolve_target("shell", &snapshot.tool_map())
        .unwrap();
    assert!(Arc::ptr_eq(&resolved, &first));
}

#[test]
fn duplicate_required_declaration_cannot_hide_a_filtered_winner() {
    let catalog = SessionToolCatalog::with_filter(BTreeMap::new(), None, Arc::new(|_| false));
    let first: Arc<dyn BaseTool> = Arc::new(BridgeTool::bridge("Read", "system").direct());
    let duplicate: Arc<dyn BaseTool> = Arc::new(BridgeTool::bridge("Read", "system").direct());
    let error = catalog
        .replace_static_mcp_tools(startup_update(
            vec![first, duplicate],
            &[("system", "Read", "Read")],
        ))
        .unwrap_err();
    assert!(matches!(
        error,
        CatalogRefreshError::RequiredToolUnavailable { .. }
    ));
    assert!(catalog.snapshot().tools.is_empty());
}
