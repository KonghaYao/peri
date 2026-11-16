//! In-process serving for builtin MCP instances (`peri mcp-start`).
//!
//! `peri mcp-start <instance> --stdio|--http` serves the named legacy instance
//! from this binary. `peri mcp-start --capability ... --stdio|--http` serves the
//! v5 single exit (`peri`) through the capability router; the selection is
//! resolved by `peri-tui` before this module runs. HTTP mode is MCP Streamable
//! HTTP at `/mcp` and is unauthenticated: bind a non-loopback host only on an
//! isolated network.
//!
//! `workflow` is served slot-only: without an ACP host nothing binds a
//! `WorkflowRuntime`, so tool calls that need the runtime return the existing typed
//! "workflow runtime is not bound" error. Orphan tools/resources remain listable.

use std::path::PathBuf;

use anyhow::Result;
#[cfg(not(target_os = "emscripten"))]
use peri_mcp_common::standalone::{serve_http, serve_stdio};

#[cfg(not(target_os = "emscripten"))]
use super::builtin::router::RouterTemplate;
#[cfg(not(target_os = "emscripten"))]
use super::builtin::standalone::StandaloneFactory;

#[cfg(not(target_os = "emscripten"))]
pub use super::builtin::router::{CapabilityReadiness, CapabilityStatus, RouterReadiness};

/// What the standalone process serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Legacy single builtin instance (migration/rollback path).
    Instance(String),
    /// v5 single exit with the resolved capability set.
    Capabilities(Vec<String>),
}

/// Transport selected on the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    Stdio,
    Http {
        host: String,
        port: u16,
        /// Extra `Host` values accepted by rmcp; `"*"` disables the check.
        allowed_hosts: Vec<String>,
    },
}

/// Inputs for one standalone process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub target: Target,
    pub transport: Transport,
    /// Working directory for the served instance. Not an OS sandbox.
    pub workspace: PathBuf,
}

/// Serve `options.target` until the transport closes.
#[cfg(not(target_os = "emscripten"))]
pub async fn run(options: Options) -> Result<()> {
    match options.target {
        Target::Instance(instance) => {
            run_instance(instance, options.transport, options.workspace).await
        }
        Target::Capabilities(capabilities) => {
            run_router(capabilities, options.transport, options.workspace).await
        }
    }
}

#[cfg(not(target_os = "emscripten"))]
async fn run_instance(instance: String, transport: Transport, workspace: PathBuf) -> Result<()> {
    let factory = StandaloneFactory::new(&instance, workspace)?;
    let workspace = factory.workspace_handle();
    let served = match transport {
        Transport::Stdio => serve_stdio(factory.create()?).await,
        Transport::Http {
            host,
            port,
            allowed_hosts,
        } => serve_http(move || factory.create(), &host, port, &allowed_hosts).await,
    };
    cleanup_workspace(workspace, served).await
}

#[cfg(not(target_os = "emscripten"))]
async fn run_router(
    capabilities: Vec<String>,
    transport: Transport,
    workspace: PathBuf,
) -> Result<()> {
    let template = RouterTemplate::new(&capabilities, workspace)?;
    let workspace_handle = template.workspace_handle();
    let served = match transport {
        Transport::Stdio => {
            let router = template.create()?;
            serve_stdio(router).await
        }
        Transport::Http {
            host,
            port,
            allowed_hosts,
        } => {
            serve_http(
                move || template.create().map_err(std::io::Error::other),
                &host,
                port,
                &allowed_hosts,
            )
            .await
        }
    };
    // 只有装配了 workspace 能力才存在 shell 任务；关闭 workspace 不是清理失败。
    let cleanup = match workspace_handle {
        Some(server) => server.shutdown_shell_tasks().await,
        None => None,
    };
    served?;
    if let Some(report) = cleanup {
        if report != peri_acp_types::tasks::TaskShutdownReport::Complete {
            anyhow::bail!("Workspace shell task cleanup incomplete");
        }
    }
    Ok(())
}

#[cfg(not(target_os = "emscripten"))]
async fn cleanup_workspace(
    workspace: Option<peri_mcp_workspace::WorkspaceMcpServer>,
    served: Result<()>,
) -> Result<()> {
    let cleanup = match workspace {
        Some(server) => server.shutdown_shell_tasks().await,
        None => None,
    };
    served?;
    if cleanup != Some(peri_acp_types::tasks::TaskShutdownReport::Complete) {
        anyhow::bail!("Workspace shell task cleanup incomplete");
    }
    Ok(())
}

#[cfg(target_os = "emscripten")]
pub async fn run(_options: Options) -> Result<()> {
    anyhow::bail!("standalone MCP serving is not available on this target")
}

#[cfg(all(test, not(target_os = "emscripten")))]
#[path = "standalone_test.rs"]
mod tests;
