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
