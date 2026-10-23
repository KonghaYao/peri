//! In-process standalone handler construction for `peri mcp-start`.
//!
//! Construction goes through [`super::dispatch::builtin_server_handler_with_env`], so
//! the standalone entry serves exactly the handlers the ACP host assembles. The
//! `workspace` arm is the exception: one server is built up front and cloned per
//! session because clones share the Bash task registry (including across HTTP
//! sessions).
//!
//! `workflow` is slot-only here: without an ACP host nothing binds a
//! `WorkflowRuntime`, so orchestration calls return the existing typed
//! "workflow runtime is not bound" error. This module does not fake execution.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};
use parking_lot::Mutex;
use peri_acp_types::builtin_mcp::BUILTIN_RESERVED_INSTANCE_NAMES;
use peri_mcp_cron::{CronScheduler, CronTrigger};
use peri_mcp_workflow::WorkflowInstanceInput;
use peri_mcp_workspace::{
    ResourceRoot, ResourceScope, WorkspaceMcpServer, WorkspaceResourcesInput,
};

use super::context::{BuiltinInstanceContext, CronInstanceInput};
use super::dispatch::{builtin_server_handler_with_env, BuiltinServerHandler};

/// Reusable per-session handler factory for one builtin instance.
pub(crate) struct StandaloneFactory {
    instance: String,
    env: HashMap<String, String>,
    context: BuiltinInstanceContext,
    /// Pre-built shared Workspace handler (`None` for every other instance).
    workspace: Option<WorkspaceMcpServer>,
}

impl StandaloneFactory {
    pub(crate) fn new(instance: &str, workspace: PathBuf) -> Result<Self> {
        if !BUILTIN_RESERVED_INSTANCE_NAMES.contains(&instance) {
            bail!(
                "unknown builtin MCP instance: {instance} (expected one of: {})",
                BUILTIN_RESERVED_INSTANCE_NAMES.join(", ")
            );
        }
        let cwd = workspace.to_string_lossy().into_owned();
        let mut context = BuiltinInstanceContext::new(cwd.clone());
        let mut workspace_server = None;
        match instance {
            "cron" => {
                let (trigger_tx, trigger_rx) = tokio::sync::mpsc::unbounded_channel();
                let scheduler = Arc::new(Mutex::new(CronScheduler::new(trigger_tx)));
                spawn_tick_loop(Arc::clone(&scheduler), trigger_rx);
                // The standalone process owns the only tick loop; a pooled host
                // must not add a second driver.
                context = context.with_cron(CronInstanceInput {
                    scheduler,
                    tick_enabled: false,
                });
            }
            "workspace" => {
                workspace_server = Some(
                    WorkspaceMcpServer::standalone(cwd, false)
                        .with_resources(default_workspace_resources(&workspace)),
                );
            }
            "workflow" => {
                context = context.with_workflow(WorkflowInstanceInput::slot());
            }
            _ => {}
        }
        Ok(Self {
            instance: instance.to_owned(),
            env: std::env::vars().collect(),
            context,
            workspace: workspace_server,
        })
    }

    pub(crate) fn create(&self) -> io::Result<BuiltinServerHandler> {
        if let Some(workspace) = &self.workspace {
            return Ok(BuiltinServerHandler::Workspace(workspace.clone()));
        }
        builtin_server_handler_with_env(&self.instance, &self.context, &self.env).ok_or_else(|| {
            io::Error::other(format!(
                "builtin MCP handler is not wired: {}",
                self.instance
            ))
        })
    }

    /// Shared handle for post-serve cleanup; workspace clones share the task registry.
    pub(crate) fn workspace_handle(&self) -> Option<WorkspaceMcpServer> {
        self.workspace.clone()
    }
}

/// Default resource roots for a standalone workspace: user skills plus the
/// project's skill/agent directories, matching the ACP host's defaults.
fn default_workspace_resources(workspace: &std::path::Path) -> WorkspaceResourcesInput {
    let mut input = WorkspaceResourcesInput::new();
    if let Some(home) = dirs::home_dir() {
        input = input.with_skill_root(ResourceRoot::new(
            home.join(".claude/skills"),
            ResourceScope::User,
        ));
    }
    input
        .with_skill_root(ResourceRoot::new(
            workspace.join(".claude/skills"),
            ResourceScope::Project,
        ))
        .with_agent_root(ResourceRoot::new(
            workspace.join(".claude/agents"),
            ResourceScope::Project,
        ))
        .with_agent_root(ResourceRoot::new(
            workspace.join("agents"),
            ResourceScope::Project,
        ))
}

fn spawn_tick_loop(
    scheduler: Arc<Mutex<CronScheduler>>,
    triggers: tokio::sync::mpsc::UnboundedReceiver<CronTrigger>,
) {
    tokio::spawn(async move {
        // Standalone mode has no trigger consumer; drain firings so the unbounded
        // channel cannot retain them after pending triggers are acknowledged.
        // `cron://triggers` remains readable.
        let mut triggers = triggers;
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        loop {
            ticker.tick().await;
            scheduler.lock().tick();
            while triggers.try_recv().is_ok() {}
        }
    });
}
