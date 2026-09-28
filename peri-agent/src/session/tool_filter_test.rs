use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use serde_json::json;

use super::{SessionToolCatalog, ToolFilterPolicy};
use crate::tools::{BaseTool, ToolContext};

struct SourceTool {
    name: &'static str,
    server: Option<&'static str>,
    builtin: Option<&'static str>,
}

#[async_trait]
impl BaseTool for SourceTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        self.name
    }
    fn parameters(&self) -> serde_json::Value {
        json!({"type": "object"})
    }
    fn mcp_server_name(&self) -> Option<&str> {
        self.server
    }
    fn mcp_tool_name(&self) -> Option<&str> {
        self.server.map(|_| self.name)
    }
    fn builtin_mcp_instance(&self) -> Option<&str> {
        self.builtin
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
fn test_explicit_builtin_allowlist_rejects_external_raw_impersonation() {
    let builtin = SourceTool {
        name: "Read",
        server: Some("workspace"),
        builtin: Some("workspace"),
    };
    let external = SourceTool {
        name: "Read",
        server: Some("external"),
        builtin: None,
    };
    let core = SourceTool {
        name: "Read",
        server: None,
        builtin: None,
    };
    let explicit = ToolFilterPolicy::canonical(Some(vec!["Read".into()]), vec![]);
    assert!(explicit(&builtin));
    assert!(explicit(&core));
    assert!(!explicit(&external));
    let legacy_spelling = SourceTool {
        name: "mcp__workspace__Read",
        server: Some("external"),
        builtin: None,
    };
    assert!(
        !explicit(&legacy_spelling),
        "legacy display normalization cannot authorize external impersonation"
    );
    assert!(ToolFilterPolicy::canonical(None, vec![])(&external));
    assert!(ToolFilterPolicy::canonical(Some(vec!["*".into()]), vec![])(
        &external
    ));
    assert!(!ToolFilterPolicy::canonical(None, vec!["Read".into()])(
        &external
    ));
}

#[test]
fn test_catalog_applies_source_filter_on_initial_snapshot() {
    let external: Arc<dyn BaseTool> = Arc::new(SourceTool {
        name: "Read",
        server: Some("external"),
        builtin: None,
    });
    let catalog = SessionToolCatalog::with_filter(
        BTreeMap::from([("Read".into(), external)]),
        None,
        ToolFilterPolicy::canonical(Some(vec!["Read".into()]), vec![]),
    );
    assert!(!catalog.snapshot().tools.contains_key("Read"));
}
