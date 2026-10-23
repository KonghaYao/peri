//! `peri mcp-start <instance>`: serve a builtin MCP instance in-process.
//!
//! Dispatched before Peri configuration or Agent startup, so the standalone
//! service consumes no Peri config and needs no sibling worker binary.

use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{ArgGroup, Parser};
use peri_acp_types::builtin_mcp::BUILTIN_RESERVED_INSTANCE_NAMES;
use peri_middlewares::mcp::standalone::{self, Options, Transport};

#[derive(Debug, Parser)]
#[command(
    name = "peri mcp-start",
    about = "Serve a builtin MCP instance without the Peri Agent"
)]
#[command(group(ArgGroup::new("transport").required(true).args(["stdio", "http"])))]
struct Args {
    /// Builtin instance to serve (web, artifact, cron, workspace, workflow).
    instance: String,
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
    if !BUILTIN_RESERVED_INSTANCE_NAMES.contains(&args.instance.as_str()) {
        bail!(
            "unknown builtin MCP instance: {} (expected one of: {})",
            args.instance,
            BUILTIN_RESERVED_INSTANCE_NAMES.join(", ")
        );
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
        }
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(standalone::run(Options {
        instance: args.instance,
        transport,
        workspace,
    }))
}

#[cfg(test)]
#[path = "cli_mcp_start_test.rs"]
mod tests;
