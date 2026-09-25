use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use peri_acp_types::{
    dynamic_mcp::{
        DynamicMcpConfig, DynamicMcpInstanceKey, DynamicMcpLogicalKey, DynamicMcpServerProjection,
        SessionMcpCapabilitySnapshot,
    },
    ports::SessionMcpCapabilityPort,
};
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

struct FixedCapability(Arc<SessionMcpCapabilitySnapshot>);

impl SessionMcpCapabilityPort for FixedCapability {
    fn snapshot(&self) -> Arc<SessionMcpCapabilitySnapshot> {
        Arc::clone(&self.0)
    }
}

struct MutableCapability(parking_lot::RwLock<Arc<SessionMcpCapabilitySnapshot>>);

impl MutableCapability {
    fn publish(&self, snapshot: SessionMcpCapabilitySnapshot) {
        *self.0.write() = Arc::new(snapshot);
    }
}

impl SessionMcpCapabilityPort for MutableCapability {
    fn snapshot(&self) -> Arc<SessionMcpCapabilitySnapshot> {
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
fn dynamic_server_shadows_static_server_as_one_catalog_unit() {
    let core: Arc<dyn BaseTool> = Arc::new(NamedTool::new("Read", "core"));
    let static_tool: Arc<dyn BaseTool> = Arc::new(NamedTool::new("mcp__example__static", "static"));
    let dynamic_tool: Arc<dyn BaseTool> =
        Arc::new(NamedTool::new("mcp__example__dynamic", "dynamic"));
    let config = DynamicMcpConfig {
        command: Some("example-mcp".to_string()),
        ..Default::default()
    }
    .canonicalize()
    .unwrap();
    let instance_key = DynamicMcpInstanceKey {
        logical: DynamicMcpLogicalKey {
            session_id: "session-a".to_string(),
            server_name: "example".to_string(),
        },
        incarnation_id: Default::default(),
    };
    let capability = Arc::new(FixedCapability(Arc::new(SessionMcpCapabilitySnapshot {
        generation: 1,
        servers: BTreeMap::from([(
            "example".to_string(),
            DynamicMcpServerProjection {
                instance_key: instance_key.clone(),
                name: "example".to_string(),
                config,
                tool_count: 1,
                resource_count: 0,
            },
        )]),
        tools: BTreeMap::from([(
            "mcp__example__dynamic".to_string(),
            peri_acp_types::dynamic_mcp::DynamicMcpToolCapability {
                instance: instance_key,
                tool: dynamic_tool,
            },
        )]),
    })));
    let catalog = SessionToolCatalog::new(
        BTreeMap::from([
            ("Read".to_string(), core),
            ("mcp__example__static".to_string(), static_tool),
        ]),
        Some(capability),
    );

    let snapshot = catalog.refresh().unwrap();
    assert!(snapshot.tools.contains_key("Read"));
    assert!(snapshot.tools.contains_key("mcp__example__dynamic"));
    assert!(!snapshot.tools.contains_key("mcp__example__static"));
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

#[test]
fn filtered_catalog_reapplies_policy_across_load_and_unload() {
    let capability = Arc::new(MutableCapability(parking_lot::RwLock::new(Arc::new(
        SessionMcpCapabilitySnapshot::default(),
    ))));
    let dynamic: Arc<dyn BaseTool> = Arc::new(NamedTool::new("mcp__example__lookup", "dynamic"));
    let instance = DynamicMcpInstanceKey {
        logical: DynamicMcpLogicalKey {
            session_id: "session-a".to_string(),
            server_name: "example".to_string(),
        },
        incarnation_id: Default::default(),
    };
    let catalog = SessionToolCatalog::with_filter(
        BTreeMap::new(),
        Some(capability.clone()),
        Arc::new(|name| name != "mcp__example__lookup"),
    );

    capability.publish(SessionMcpCapabilitySnapshot {
        generation: 1,
        servers: BTreeMap::from([(
            "example".to_string(),
            DynamicMcpServerProjection {
                instance_key: instance.clone(),
                name: "example".to_string(),
                config: DynamicMcpConfig {
                    command: Some("example-mcp".to_string()),
                    ..Default::default()
                }
                .canonicalize()
                .unwrap(),
                tool_count: 1,
                resource_count: 0,
            },
        )]),
        tools: BTreeMap::from([(
            "mcp__example__lookup".to_string(),
            peri_acp_types::dynamic_mcp::DynamicMcpToolCapability {
                instance,
                tool: dynamic,
            },
        )]),
    });
    let loaded = catalog.refresh().unwrap();

    capability.publish(SessionMcpCapabilitySnapshot {
        generation: 2,
        ..Default::default()
    });
    let unloaded = catalog.refresh().unwrap();

    assert_eq!(loaded.generation, 1);
    assert!(!loaded.tools.contains_key("mcp__example__lookup"));
    assert_eq!(unloaded.generation, 2);
    assert!(!unloaded.tools.contains_key("mcp__example__lookup"));
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

fn server_projection(server: &str) -> DynamicMcpServerProjection {
    DynamicMcpServerProjection {
        instance_key: DynamicMcpInstanceKey {
            logical: DynamicMcpLogicalKey {
                session_id: "session-a".to_string(),
                server_name: server.to_string(),
            },
            incarnation_id: Default::default(),
        },
        name: server.to_string(),
        config: DynamicMcpConfig {
            command: Some(format!("{server}-mcp")),
            ..Default::default()
        }
        .canonicalize()
        .unwrap(),
        tool_count: 1,
        resource_count: 0,
    }
}

fn dynamic_capability(
    generation: u64,
    servers: &[&str],
    tools: &[(&str, &str, Arc<dyn BaseTool>)],
) -> SessionMcpCapabilitySnapshot {
    let mut server_map: BTreeMap<String, DynamicMcpServerProjection> = servers
        .iter()
        .map(|server| ((*server).to_string(), server_projection(server)))
        .collect();
    let mut tool_map = BTreeMap::new();
    for (name, server, tool) in tools {
        let projection = server_map
            .entry((*server).to_string())
            .or_insert_with(|| server_projection(server));
        tool_map.insert(
            (*name).to_string(),
            peri_acp_types::dynamic_mcp::DynamicMcpToolCapability {
                instance: projection.instance_key.clone(),
                tool: Arc::clone(tool),
            },
        );
    }
    SessionMcpCapabilitySnapshot {
        generation,
        servers: server_map,
        tools: tool_map,
    }
}

/// static base 的可观察投影（提交失败时用于断言 base 未被部分修改）。
fn base_names(catalog: &SessionToolCatalog) -> Vec<String> {
    catalog
        .dynamic_catalog_tools()
        .into_iter()
        .map(|tool| tool.name)
        .collect()
}

fn startup_catalog(
    base: BTreeMap<String, Arc<dyn BaseTool>>,
    capability: Option<Arc<dyn SessionMcpCapabilityPort>>,
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
    let catalog = startup_catalog(
        core_and_static_base(),
        Some(Arc::new(FixedCapability(Arc::new(
            SessionMcpCapabilitySnapshot {
                generation: 1,
                ..Default::default()
            },
        )))),
    );
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
        ToolSource::StaticMcp("system".to_string())
    );
    assert!(published.tools.contains_key("Read"), "core 工具不受影响");
    assert_eq!(
        published.tools["mcp__other__keep"].source,
        ToolSource::StaticMcp("other".to_string()),
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
fn startup_commit_survives_dynamic_generation_bump() {
    let capability = Arc::new(MutableCapability(parking_lot::RwLock::new(Arc::new(
        SessionMcpCapabilitySnapshot::default(),
    ))));
    let catalog = startup_catalog(core_and_static_base(), Some(capability.clone()));
    let prepared: Arc<dyn BaseTool> =
        Arc::new(BridgeTool::bridge("mcp__system__lookup", "system").direct());
    catalog
        .replace_static_mcp_tools(startup_update(
            vec![Arc::clone(&prepared)],
            &[("system", "lookup", "mcp__system__lookup")],
        ))
        .unwrap();

    capability.publish(SessionMcpCapabilitySnapshot {
        generation: 2,
        ..Default::default()
    });
    let refreshed = catalog.refresh().unwrap();

    assert_eq!(refreshed.generation, 2, "refresh 必须按新 generation 重建");
    assert!(
        Arc::ptr_eq(&refreshed.tools["mcp__system__lookup"].tool, &prepared),
        "dynamic overlay 重建必须以提交后的 static base 为事实源"
    );
    assert!(refreshed.tools.contains_key("Read"));
}

#[test]
fn dynamic_overlay_still_shadows_static_entries_after_startup_commit() {
    let capability = Arc::new(MutableCapability(parking_lot::RwLock::new(Arc::new(
        SessionMcpCapabilitySnapshot::default(),
    ))));
    let catalog = startup_catalog(core_and_static_base(), Some(capability.clone()));
    let prepared: Arc<dyn BaseTool> =
        Arc::new(BridgeTool::bridge("mcp__system__lookup", "system").direct());
    catalog
        .replace_static_mcp_tools(startup_update(
            vec![Arc::clone(&prepared)],
            &[("system", "lookup", "mcp__system__lookup")],
        ))
        .unwrap();

    let dynamic_tool: Arc<dyn BaseTool> =
        Arc::new(NamedTool::new("mcp__system__direct", "dynamic"));
    capability.publish(dynamic_capability(
        1,
        &["system"],
        &[("mcp__system__direct", "system", dynamic_tool)],
    ));
    let refreshed = catalog.refresh().unwrap();

    assert!(
        !refreshed.tools.contains_key("mcp__system__lookup"),
        "启动提交不得绕过动态实例对同名 logical server 的遮蔽"
    );
    assert!(refreshed.tools.contains_key("mcp__system__direct"));
    assert!(refreshed.tools.contains_key("Read"));
}

#[test]
fn startup_commit_rejects_required_tool_filtered_by_policy() {
    let catalog = SessionToolCatalog::with_filter(
        core_and_static_base(),
        None,
        Arc::new(|name| name != "mcp__system__lookup"),
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
fn startup_commit_rejects_required_tool_shadowed_by_dynamic_instance() {
    let catalog = startup_catalog(
        core_and_static_base(),
        Some(Arc::new(FixedCapability(Arc::new(dynamic_capability(
            1,
            &["system"],
            &[],
        ))))),
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

    assert!(matches!(
        error,
        CatalogRefreshError::RequiredToolUnavailable { ref tool, .. } if tool == "mcp__system__lookup"
    ));
    assert!(
        Arc::ptr_eq(&before, &catalog.snapshot()),
        "被动态遮蔽的必需项必须拒绝整批，不发布半成品"
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
    let error = catalog
        .replace_static_mcp_tools(startup_update(vec![declared_but_core_named], &[]))
        .unwrap_err();
    assert_eq!(
        error,
        CatalogRefreshError::InvalidStartupSource {
            tool: "Read".to_string()
        }
    );
    assert!(
        !catalog.snapshot().tools["Read"].tool.is_direct(),
        "启动提交不得把 core 工具身份换成 MCP bridge"
    );
}

#[test]
fn startup_commit_rejects_cross_server_takeover() {
    let catalog = startup_catalog(core_and_static_base(), None);
    let foreign: Arc<dyn BaseTool> =
        Arc::new(BridgeTool::bridge("mcp__system__lookup", "other").direct());
    let error = catalog
        .replace_static_mcp_tools(startup_update(vec![foreign], &[]))
        .unwrap_err();

    assert_eq!(
        error,
        CatalogRefreshError::StartupRegistrationRejected {
            tool: "mcp__system__lookup".to_string()
        }
    );
    assert_eq!(
        catalog.snapshot().tools["mcp__system__lookup"].source,
        ToolSource::StaticMcp("system".to_string()),
        "跨 server 覆盖必须被拒绝且不修改既有条目"
    );
}

#[test]
fn startup_commit_rejects_alias_conflict_without_partial_publish() {
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
    let error = catalog
        .replace_static_mcp_tools(startup_update(vec![conflicting], &[]))
        .unwrap_err();

    assert_eq!(error, CatalogRefreshError::AliasConflict);
    assert!(!catalog.snapshot().tools.contains_key("mcp__system__tool"));
    assert_eq!(
        base_names(&catalog),
        vec!["Read".to_string(), "mcp__other__keep".to_string()]
    );
}

#[test]
fn startup_commit_registers_collision_directory_before_publishing() {
    let catalog = startup_catalog(core_and_static_base(), None);
    let seen: Arc<parking_lot::Mutex<Option<Vec<String>>>> =
        Arc::new(parking_lot::Mutex::new(None));
    let captured = Arc::clone(&seen);
    catalog.set_startup_catalog_registration(Arc::new(move |tools| {
        *captured.lock() = Some(tools.into_iter().map(|tool| tool.name).collect());
        Ok(())
    }));

    let prepared: Arc<dyn BaseTool> =
        Arc::new(BridgeTool::bridge("mcp__system__lookup", "system").direct());
    catalog
        .replace_static_mcp_tools(startup_update(
            vec![Arc::clone(&prepared)],
            &[("system", "lookup", "mcp__system__lookup")],
        ))
        .unwrap();

    let registered = seen.lock().clone().expect("注册回调必须被调用");
    assert!(registered.contains(&"mcp__system__lookup".to_string()));
    assert!(registered.contains(&"Read".to_string()));

    catalog.set_startup_catalog_registration(Arc::new(|_| {
        Err(CatalogRefreshError::StartupRegistrationRejected {
            tool: "mcp__system__lookup".to_string(),
        })
    }));
    let rejected: Arc<dyn BaseTool> = Arc::new(BridgeTool::bridge("mcp__system__lookup", "system"));
    let error = catalog
        .replace_static_mcp_tools(startup_update(vec![rejected], &[]))
        .unwrap_err();

    assert!(matches!(
        error,
        CatalogRefreshError::StartupRegistrationRejected { .. }
    ));
    assert!(
        Arc::ptr_eq(
            &catalog.snapshot().tools["mcp__system__lookup"].tool,
            &prepared
        ),
        "碰撞目录拒绝后本地状态不得改变"
    );
}
