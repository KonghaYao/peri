use std::ffi::OsString;

use anyhow::Result;
use clap::Parser;

fn workflow_index(args: &[OsString]) -> Option<usize> {
    for index in 1..args.len() {
        if args[index] != "workflow" || args[..index].iter().any(|arg| arg == "--") {
            continue;
        }
        let Ok(cli) = super::Cli::try_parse_from(&args[..=index]) else {
            continue;
        };
        if matches!(cli.command, Some(super::Commands::Workflow { .. }))
            && super::validate_cli(&cli).is_ok()
        {
            return Some(index);
        }
    }
    None
}

pub(crate) fn argv_requests_workflow(args: &[OsString]) -> bool {
    workflow_index(args).is_some()
}

pub(crate) fn run_before_configuration(args: &[OsString]) -> Result<()> {
    let index = workflow_index(args).expect("workflow dispatch requires a workflow argv");
    let workflow_args = &args[index + 1..];
    let runtime = super::build_runtime()?;
    let exit_code = runtime.block_on(peri_acp::workflow_cli::run(workflow_args))?;
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}

#[cfg(test)]
#[path = "cli_workflow_test.rs"]
mod tests;
