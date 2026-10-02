//! A pure Workspace MCP process. Peri's CLI only dispatches to this binary.

use std::{net::SocketAddr, path::PathBuf};

use anyhow::{bail, Context, Result};
use clap::{ArgGroup, Parser};
use peri_mcp_workspace::{
    ResourceRoot, ResourceScope, WorkspaceMcpServer, WorkspaceResourcesInput,
};
use rmcp::{
    transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    },
    ServiceExt,
};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Parser)]
#[command(
    name = "peri mcp-start workspace",
    about = "Serve Workspace MCP without Peri Agent"
)]
#[command(group(ArgGroup::new("transport").required(true).args(["stdio", "http"])))]
struct Args {
    /// Serve MCP over stdin/stdout.
    #[arg(long, conflicts_with = "http")]
    stdio: bool,
    /// Serve Streamable HTTP at /mcp.
    #[arg(long, conflicts_with = "stdio")]
    http: bool,
    /// Fixed Workspace directory. This is not an OS sandbox.
    #[arg(long)]
    workspace: PathBuf,
    /// Loopback listen address for HTTP mode.
    #[arg(long, requires = "http")]
    bind: Option<SocketAddr>,
    /// Project skill root. If supplied, replaces default skill roots.
    #[arg(long = "skill-root")]
    skill_roots: Vec<PathBuf>,
    /// Project agent root. If supplied, replaces default agent roots.
    #[arg(long = "agent-root")]
    agent_roots: Vec<PathBuf>,
    /// Publish no filesystem skill roots.
    #[arg(long, conflicts_with_all = ["skill_roots", "plugin_skill_roots"])]
    no_skill_roots: bool,
    /// Publish no filesystem agent roots.
    #[arg(long, conflicts_with_all = ["agent_roots", "plugin_agent_roots"])]
    no_agent_roots: bool,
    /// Plugin skill root in NAME=PATH form.
    #[arg(long = "plugin-skill-root")]
    plugin_skill_roots: Vec<String>,
    /// Plugin agent root in NAME=PATH form.
    #[arg(long = "plugin-agent-root")]
    plugin_agent_roots: Vec<String>,
    /// Do not publish built-in skills and agents.
    #[arg(long)]
    disable_bundled: bool,
}

fn plugin_root(value: &str) -> Result<ResourceRoot> {
    let (name, path) = value
        .split_once('=')
        .context("plugin root must be NAME=PATH")?;
    if name.is_empty() || path.is_empty() {
        bail!("plugin root must be NAME=PATH");
    }
    let path = canonical_directory(path, "plugin root")?;
    Ok(ResourceRoot::plugin(path, name))
}

fn canonical_directory(path: impl AsRef<std::path::Path>, name: &str) -> Result<PathBuf> {
    let path = std::fs::canonicalize(path).with_context(|| format!("cannot resolve {name}"))?;
    if !path.is_dir() {
        bail!("{name} is not a directory");
    }
    Ok(path)
}

fn resource_input(args: &Args, workspace: &std::path::Path) -> Result<WorkspaceResourcesInput> {
    let mut input = WorkspaceResourcesInput::new().with_disable_bundled(args.disable_bundled);
    if !args.no_skill_roots {
        if args.skill_roots.is_empty() {
            if let Some(home) = dirs_next::home_dir() {
                input = input.with_skill_root(ResourceRoot::new(
                    home.join(".claude/skills"),
                    ResourceScope::User,
                ));
            }
            input = input.with_skill_root(ResourceRoot::new(
                workspace.join(".claude/skills"),
                ResourceScope::Project,
            ));
        } else {
            for root in &args.skill_roots {
                input = input.with_skill_root(ResourceRoot::new(
                    canonical_directory(root, "skill root")?,
                    ResourceScope::Project,
                ));
            }
        }
    }
    if !args.no_agent_roots {
        if args.agent_roots.is_empty() {
            for root in [workspace.join(".claude/agents"), workspace.join("agents")] {
                input = input.with_agent_root(ResourceRoot::new(root, ResourceScope::Project));
            }
        } else {
            for root in &args.agent_roots {
                input = input.with_agent_root(ResourceRoot::new(
                    canonical_directory(root, "agent root")?,
                    ResourceScope::Project,
                ));
            }
        }
    }
    for root in &args.plugin_skill_roots {
        input = input.with_skill_root(plugin_root(root)?);
    }
    for root in &args.plugin_agent_roots {
        input = input.with_agent_root(plugin_root(root)?);
    }
    Ok(input)
}

async fn serve_stdio(server: WorkspaceMcpServer) -> Result<()> {
    let task_owner = server.clone();
    let service = server
        .serve((tokio::io::stdin(), tokio::io::stdout()))
        .await
        .context("Workspace MCP stdio initialization failed")?;
    let result = service
        .waiting()
        .await
        .context("Workspace MCP stdio failed");
    let _ = task_owner.shutdown_shell_tasks().await;
    result.map(|_| ())
}

async fn serve_http(server: WorkspaceMcpServer, bind: SocketAddr) -> Result<()> {
    if !bind.ip().is_loopback() {
        bail!("non-loopback HTTP binding requires authentication, which is not configured");
    }
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .context("cannot bind Workspace MCP HTTP listener")?;
    let shutdown = CancellationToken::new();
    let mut config = StreamableHttpServerConfig::default();
    config.legacy_session_mode = true;
    config.cancellation_token = shutdown.clone();
    let task_owner = server.clone();
    let service: StreamableHttpService<_, LocalSessionManager> =
        StreamableHttpService::new(move || Ok(server.clone()), Default::default(), config);
    let router = axum::Router::new().nest_service("/mcp", service);
    let result = axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            shutdown.cancel();
        })
        .await
        .context("Workspace MCP HTTP server failed");
    let _ = task_owner.shutdown_shell_tasks().await;
    result
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                _ = terminate.recv() => {},
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let workspace = canonical_directory(&args.workspace, "workspace")?;
    let resources = resource_input(&args, &workspace)?;
    let server = WorkspaceMcpServer::standalone(workspace.to_string_lossy().into_owned())
        .with_resources(resources);
    if args.stdio {
        serve_stdio(server).await
    } else {
        serve_http(
            server,
            args.bind
                .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 8765))),
        )
        .await
    }
}

#[cfg(test)]
#[path = "peri_mcp_workspace/tests.rs"]
mod tests;
