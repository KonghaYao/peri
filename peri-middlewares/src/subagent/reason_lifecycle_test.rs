use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use parking_lot::RwLock;
use peri_agent::agent::stages::SharedToolMap;
use peri_agent::middleware::capabilities::CatalogState;
use peri_agent::middleware::r#trait::Middleware;
use peri_agent::session::{FrozenContext, Session};

use super::SubAgentMiddleware;
use crate::hooks::types::{HookAction, HookEvent, HookInput, HookType, RegisteredHook};

struct ReasonCatalog {
    tools: SharedToolMap,
}

impl CatalogState for ReasonCatalog {
    fn local_tools(&self) -> Option<&SharedToolMap> {
        Some(&self.tools)
    }

    fn push_recall(&mut self, _item: String) {}
}

fn middleware(log: &std::path::Path) -> SubAgentMiddleware {
    let hooks = [HookEvent::SubagentStart, HookEvent::SubagentStop]
        .into_iter()
        .map(|event| RegisteredHook {
            hook: HookType::Command {
                command: format!("echo {event:?} >> '{}'", log.display()),
                shell: None,
                timeout: Some(10),
                status_message: None,
                once: true,
                async_run: false,
                async_rewake: false,
                matcher: None,
                condition: None,
            },
            event,
            matcher: None,
            plugin_name: "reason-once".to_string(),
            plugin_id: "reason-once".to_string(),
            plugin_source: None,
            plugin_root: std::env::temp_dir(),
            plugin_data_dir: std::env::temp_dir(),
            plugin_options: HashMap::new(),
        })
        .collect();
    let middleware = SubAgentMiddleware::new(
        Vec::new(),
        None,
        Arc::new(|_| panic!("lifecycle test does not call the model")),
    )
    .with_registered_hooks(hooks);
    middleware.set_parent_session(Session::new(
        Arc::from(std::env::temp_dir().to_str().unwrap()),
        FrozenContext::builder().build(),
        None,
    ));
    middleware
}

async fn reason_lifecycle(middleware: &SubAgentMiddleware, catalog: &mut ReasonCatalog) {
    let before = Arc::clone(catalog.tools.read().get("Agent").unwrap());
    middleware.before_reason_catalog(catalog).await.unwrap();
    let after = Arc::clone(catalog.tools.read().get("Agent").unwrap());
    assert!(!Arc::ptr_eq(&before, &after));
    let tool = middleware.build_tool(std::env::temp_dir().to_str().unwrap());
    let dispatcher = tool.lifecycle_dispatcher().unwrap();
    for event in [HookEvent::SubagentStart, HookEvent::SubagentStop] {
        let input = HookInput::session_start(
            "reason-once",
            "",
            std::env::temp_dir().to_str().unwrap(),
            "startup",
            "opus",
        );
        let action = dispatcher
            .fire_subagent_lifecycle(event, &input, "explorer")
            .await;
        assert!(matches!(action, HookAction::Allow));
    }
}

fn catalog(middleware: &SubAgentMiddleware) -> ReasonCatalog {
    ReasonCatalog {
        tools: Arc::new(RwLock::new(BTreeMap::from([(
            "Agent".to_string(),
            Arc::new(middleware.build_tool(std::env::temp_dir().to_str().unwrap()))
                as Arc<dyn peri_agent::tools::BaseTool>,
        )]))),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn lifecycle_once_survives_reason_catalog_replacement() {
    let directory = tempfile::tempdir().unwrap();
    let log = directory.path().join("lifecycle.log");
    let parent = middleware(&log);
    let mut catalog = catalog(&parent);
    reason_lifecycle(&parent, &mut catalog).await;
    reason_lifecycle(&parent, &mut catalog).await;
    let output = std::fs::read_to_string(log).unwrap();
    assert_eq!(
        output.lines().collect::<Vec<_>>(),
        ["SubagentStart", "SubagentStop"]
    );
}

#[cfg(unix)]
#[tokio::test]
async fn lifecycle_once_is_independent_between_parent_middlewares() {
    let directory = tempfile::tempdir().unwrap();
    let log = directory.path().join("lifecycle.log");
    let first = middleware(&log);
    let second = middleware(&log);
    reason_lifecycle(&first, &mut catalog(&first)).await;
    reason_lifecycle(&second, &mut catalog(&second)).await;
    assert_eq!(std::fs::read_to_string(log).unwrap().lines().count(), 4);
}
