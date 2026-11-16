//! `peri mcp-start`: serve builtin MCP capabilities in-process.
//!
//! Legacy form (`peri mcp-start <instance>`) is kept for the v5 migration
//! window. The v5 form selects capabilities with `--capability` or
//! `mcpServers.peri.x-peri.capabilities` in `<workspace>/.mcp.json` and serves
//! them through the single `peri` capability router. Capability selection is
//! resolved here, before Peri configuration or Agent startup, so the standalone
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
use peri_middlewares::mcp::standalone::{self, Options, Target, Transport};

#[derive(Debug, Parser)]
#[command(
    name = "peri mcp-start",
    about = "Serve builtin MCP capabilities without the Peri Agent",
    after_help = "v5: `peri mcp-start peri --stdio` (optionally `--capability web,cron`); \
                  selection priority is command line > <workspace>/.mcp.json > builtin default \
                  (all capabilities).\n\
                  Legacy compatibility: `peri mcp-start <instance> --stdio|--http` remains \
                  available during the v5 migration (until M4).\n\
                  .mcp.json extension: `mcpServers.peri.x-peri.capabilities` (provisional key)."
)]
#[command(group(ArgGroup::new("transport").required(true).args(["stdio", "http"])))]
struct Args {
    /// Server identity: v5 single exit `peri` or a legacy builtin instance.
    /// Omit when selecting capabilities with --capability or .mcp.json.
    instance: Option<String>,
    /// Capability to serve; repeatable or comma-separated. Accepts all/none.
    /// Selected capabilities are served by the single `peri` router.
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
    let single_exit = args
        .instance
        .as_deref()
        .is_some_and(peri_acp_types::builtin_mcp::is_v5_server_name);
    if args.instance.is_some() && !single_exit && !args.capability.is_empty() {
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
        .with_context(|| format!("cannot resolve workspace {}", display_path(&workspace)))?;
    if !workspace.is_dir() {
        bail!("workspace is not a directory: {}", display_path(&workspace));
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
    let target = match args.instance {
        Some(instance) if peri_acp_types::builtin_mcp::is_v5_server_name(&instance) => {
            Target::Capabilities(resolve_capability_selection(&args.capability, &workspace)?)
        }
        Some(instance) => {
            if !BUILTIN_RESERVED_INSTANCE_NAMES.contains(&instance.as_str()) {
                let mut expected = vec!["peri"];
                expected.extend(BUILTIN_RESERVED_INSTANCE_NAMES.iter().copied());
                bail!(
                    "unknown builtin MCP instance: {} (expected one of: {})",
                    instance,
                    expected.join(", ")
                );
            }
            Target::Instance(instance)
        }
        None => Target::Capabilities(resolve_capability_selection(&args.capability, &workspace)?),
    };
    Ok(Options {
        target,
        transport,
        workspace,
    })
}

/// Resolve the v5 capability selection into the router's enabled set. Returns a
/// typed error for unknown, duplicate, empty or conflicting selections.
fn resolve_capability_selection(
    capability_args: &[String],
    workspace: &Path,
) -> Result<Vec<String>> {
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
    if enabled.is_empty() {
        bail!(
            "no builtin MCP capability is enabled; pass --capability or configure \
             mcpServers.peri.x-peri.capabilities"
        );
    }
    Ok(enabled.into_iter().map(str::to_owned).collect())
}

fn read_mcp_capability_file(workspace: &Path) -> Result<Option<McpCapabilityFile>> {
    let path = workspace.join(".mcp.json");
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("cannot read MCP config {}", display_path(&path)))?;
    let document: serde_json::Value = serde_json::from_str(&raw)
        .with_context(|| format!("invalid JSON in MCP config {}", display_path(&path)))?;
    let file = parse_mcp_file_capabilities(&document)
        .with_context(|| format!("invalid MCP capability config {}", display_path(&path)))?;
    Ok(Some(file))
}

/// Render a path for diagnostics without the Windows verbatim prefix that
/// `canonicalize` adds but users never typed. Runs on every platform so Unix CI
/// covers the `\\?\` / `\\?\UNC\` normalization.
fn display_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{unc}");
    }
    text.strip_prefix(r"\\?\")
        .unwrap_or(text.as_ref())
        .to_string()
}

#[cfg(test)]
#[path = "cli_mcp_start_test.rs"]
mod tests;
