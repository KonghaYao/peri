//! `peri mcp-start`: serve builtin MCP capabilities in-process.
//!
//! Legacy form (`peri mcp-start <instance>`) is kept for the v5 migration
//! window. The v5 form selects capabilities with `--capability` or
//! `mcpServers.peri.x-peri.capabilities` in `<workspace>/.mcp.json`; until the
//! capability router lands, exactly one selected capability is served through
//! the legacy instance path. Multi-capability selection fails with a typed
//! explanation instead of silently serving one capability.
//!
//! Dispatched before Peri configuration or Agent startup, so the standalone
//! service consumes no Peri settings and needs no sibling worker binary.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{ArgGroup, Parser};
use peri_acp_types::builtin_mcp::BUILTIN_RESERVED_INSTANCE_NAMES;
use peri_config::mcp_capabilities::{
    CapabilitySource, McpCapabilityFile, parse_capability_tokens, parse_mcp_file_capabilities,
    resolve_capabilities,
};
use peri_middlewares::mcp::standalone::{self, Options, Transport};

#[derive(Debug, Parser)]
#[command(
    name = "peri mcp-start",
    about = "Serve builtin MCP capabilities without the Peri Agent",
    after_help = "Legacy compatibility: `peri mcp-start <instance> --stdio|--http` remains \
                  available during the v5 migration.\n\
                  v5: `peri mcp-start --capability web,cron --stdio`; selection priority is \
                  command line > <workspace>/.mcp.json > builtin default (all capabilities).\n\
                  .mcp.json extension: `mcpServers.peri.x-peri.capabilities` (provisional key)."
)]
#[command(group(ArgGroup::new("transport").required(true).args(["stdio", "http"])))]
struct Args {
    /// Legacy builtin instance to serve (web, artifact, cron, monitor, workspace, workflow).
    /// Omit when selecting capabilities with --capability or .mcp.json.
    instance: Option<String>,
    /// Capability to serve; repeatable or comma-separated. Accepts all/none.
    /// A single capability currently maps to its legacy instance until the router lands.
    #[arg(long = "capability", value_name = "NAME")]
    capability: Vec<String>,
    /// Serve MCP over stdin/stdout.
    #[arg(long, conflicts_with = "http")]
    stdio: bool,
    /// Serve MCP Streamable HTTP at /mcp.
    #[arg(long, conflicts_with = "stdio")]
    http: bool,
    /// HTTP listen host (default 127.0.0.1). A non-loopback host is
    /// unauthenticated: only use it on an isolated network.
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    /// HTTP listen port (default 8765).
    #[arg(long, default_value_t = 8765)]
    port: u16,
    /// Extra HTTP `Host` header values accepted by the server (repeatable).
    /// The loopback defaults stay accepted; use `*` to disable Host validation.
    #[arg(long = "allowed-host", value_name = "HOST")]
    allowed_host: Vec<String>,
    /// Working directory for the served instance (default: current directory).
    /// This is not an OS sandbox.
    #[arg(long)]
    workspace: Option<PathBuf>,
}

pub(super) fn run(args: &[OsString]) -> Result<()> {
    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push(OsString::from("peri mcp-start"));
    argv.extend_from_slice(args);
    let args = Args::try_parse_from(argv)?;
    let options = resolve_options(args)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(standalone::run(options))
}

fn resolve_options(args: Args) -> Result<Options> {
    if args.instance.is_some() && !args.capability.is_empty() {
        bail!(
            "mcp-start accepts either a legacy instance argument or --capability, not both; \
             use one form"
        );
    }
    if !args.allowed_host.is_empty() && !args.http {
        bail!("--allowed-host requires --http");
    }
    let workspace = match args.workspace {
        Some(path) => path,
        None => std::env::current_dir().context("cannot determine current directory")?,
    };
    let workspace = workspace
        .canonicalize()
        .with_context(|| format!("cannot resolve workspace {}", workspace.display()))?;
    if !workspace.is_dir() {
        bail!("workspace is not a directory: {}", workspace.display());
    }
    let transport = if args.stdio {
        Transport::Stdio
    } else {
        Transport::Http {
            host: args.host,
            port: args.port,
            allowed_hosts: args.allowed_host,
        }
    };
    let instance = match args.instance {
        Some(instance) => {
            if !BUILTIN_RESERVED_INSTANCE_NAMES.contains(&instance.as_str()) {
                bail!(
                    "unknown builtin MCP instance: {} (expected one of: {})",
                    instance,
                    BUILTIN_RESERVED_INSTANCE_NAMES.join(", ")
                );
            }
            instance
        }
        None => resolve_capability_instance(&args.capability, &workspace)?,
    };
    Ok(Options {
        instance,
        transport,
        workspace,
    })
}

/// Resolve the v5 capability selection to the one legacy instance that this
/// process can serve today. Returns a typed error for unknown, duplicate,
/// empty or conflicting selections.
fn resolve_capability_instance(capability_args: &[String], workspace: &Path) -> Result<String> {
    let cli = if capability_args.is_empty() {
        None
    } else {
        Some(parse_capability_tokens(
            capability_args,
            CapabilitySource::Cli,
        )?)
    };
    let file = read_mcp_capability_file(workspace)?;
    let resolved = resolve_capabilities(cli.as_ref(), file.as_ref())?;
    for conflict in resolved.conflicts() {
        tracing::warn!(
            capability = conflict.capability.name(),
            winner = %conflict.winner,
            overridden = %conflict.overridden,
            winner_enabled = conflict.winner_enabled,
            overridden_enabled = conflict.overridden_enabled,
            "mcp-start capability selection overrides a conflicting source"
        );
    }
    let enabled = resolved.enabled_names();
    match enabled.as_slice() {
        [instance] => Ok((*instance).to_string()),
        [] => bail!(
            "no builtin MCP capability is enabled; pass --capability or configure \
             mcpServers.peri.x-peri.capabilities"
        ),
        _ => bail!(
            "v5 single-exit capability router is not implemented; exactly one capability can be \
             served per process, but {} were selected: {}. Use `peri mcp-start <instance> \
             --stdio|--http` or select one capability.",
            enabled.len(),
            enabled.join(", ")
        ),
    }
}

fn read_mcp_capability_file(workspace: &Path) -> Result<Option<McpCapabilityFile>> {
    let path = workspace.join(".mcp.json");
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("cannot read MCP config {}", path.display()))?;
    let document: serde_json::Value = serde_json::from_str(&raw)
        .with_context(|| format!("invalid JSON in MCP config {}", path.display()))?;
    let file = parse_mcp_file_capabilities(&document)
        .with_context(|| format!("invalid MCP capability config {}", path.display()))?;
    Ok(Some(file))
}

#[cfg(test)]
#[path = "cli_mcp_start_test.rs"]
mod tests;
