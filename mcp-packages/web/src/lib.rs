mod agents;
mod server;
pub mod web_common;
pub mod web_fetch;
pub mod web_search;

pub use agents::{BuiltinAgent, BUILTIN_AGENTS};
pub use server::WebMcpServer;
