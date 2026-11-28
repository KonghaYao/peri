mod cli_args;
mod cli_auth;
#[cfg(test)]
mod cli_integration_test;
mod cli_mcp_start;
mod cli_meta;
mod cli_plugin;
mod cli_print;
mod cli_tui;
mod cli_workflow;

#[cfg(test)]
#[path = "main_test.rs"]
mod tests;

use anyhow::Result;
#[cfg(test)]
use cli_tui::propagate_tui_result;
#[cfg(test)]
use peri_acp_types::session_resources::AccessMode;
use peri_acp_types::session_store::SessionStoreDeployment;
use peri_tui::kit::panic::init_panic_notify;
#[cfg(test)]
use std::ffi::OsString;
#[cfg(test)]
use std::path::PathBuf;

mod startup;
use startup::{Cli, Commands, MachineAction, inject_settings_override, validate_cli};
#[cfg(test)]
use startup::{
    MetaAction, argv_requests_meta, inject_env_from_file, local_session_db_path,
    pre_scan_config_file, session_store_deployment,
};

fn main() -> anyhow::Result<()> {
    // OSC 7577：取走终端注入的 token 并记录启动参数。必须早于任何线程/子进程创建，
    // 否则 token 会被 MCP server、shell 工具等子进程继承。
    peri_tui::kit::program_resume::init();
    startup::run()
}
