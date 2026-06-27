pub mod action_resolver;
pub mod dispatcher;
pub mod executor;
pub mod input_builder;
pub mod loader;
pub mod matcher;
pub mod middleware;
pub mod once_tracker;
pub mod output_parser;
pub mod permission_gate;
pub mod ssrf_guard;
pub mod stage_firing;
pub mod stop_block_guard;
pub mod types;
pub mod variables;

pub use dispatcher::fire_standalone_lifecycle_hooks;
pub use executor::{
    execute_agent_hook, execute_command_hook, execute_http_hook, execute_prompt_hook,
};
pub use matcher::{matches_if_condition, matches_matcher};
pub use middleware::HookMiddleware;
pub use output_parser::{parse_command_hook_output, parse_http_hook_response};
pub use types::*;
pub use variables::{resolve_hook_variables, resolve_hook_variables_with_env};
