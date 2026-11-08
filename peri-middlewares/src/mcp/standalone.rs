//! In-process serving for builtin MCP instances (`peri mcp-start`).
//!
//! `peri mcp-start <instance> --stdio|--http` serves the named builtin instance
//! from this binary; no sibling worker executable is involved. HTTP mode is MCP
//! Streamable HTTP at `/mcp` and is unauthenticated: bind a non-loopback host only
//! on an isolated network.
//!
//! `workflow` is served slot-only: without an ACP host nothing binds a
//! `WorkflowRuntime`, so tool calls that need the runtime return the existing typed
//! "workflow runtime is not bound" error. Orphan tools/resources remain listable.

use std::path::PathBuf;

use anyhow::Result;
#[cfg(not(target_os = "emscripten"))]
use peri_mcp_common::standalone::{serve_http, serve_stdio};

#[cfg(not(target_os = "emscripten"))]
use super::builtin::standalone::StandaloneFactory;

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

/// Inputs for one standalone builtin instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub instance: String,
    pub transport: Transport,
    /// Working directory for the served instance. Not an OS sandbox.
    pub workspace: PathBuf,
}

/// Serve `options.instance` until the transport closes.
#[cfg(not(target_os = "emscripten"))]
pub async fn run(options: Options) -> Result<()> {
    let factory = StandaloneFactory::new(&options.instance, options.workspace)?;
    let workspace = factory.workspace_handle();
    let served = match options.transport {
        Transport::Stdio => serve_stdio(factory.create()?).await,
        Transport::Http {
            host,
            port,
            allowed_hosts,
        } => serve_http(move || factory.create(), &host, port, &allowed_hosts).await,
    };
    let cleanup = cleanup_workspace(workspace).await;
    served?;
    cleanup
}

#[cfg(not(target_os = "emscripten"))]
async fn cleanup_workspace(
    workspace: Option<peri_mcp_workspace::WorkspaceMcpServer>,
) -> Result<()> {
    let Some(server) = workspace else {
        // Other builtin instances have no workspace shell registry to shut down.
        return Ok(());
    };
    require_workspace_cleanup(server.shutdown_shell_tasks().await)
}

#[cfg(not(target_os = "emscripten"))]
fn require_workspace_cleanup(
    report: Option<peri_acp_types::tasks::TaskShutdownReport>,
) -> Result<()> {
    if report != Some(peri_acp_types::tasks::TaskShutdownReport::Complete) {
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
