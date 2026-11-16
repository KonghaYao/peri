use peri_agent::middleware::capabilities as hook_state;
use std::fmt;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;

use peri_agent::middleware::r#trait::Middleware;

use crate::plugin::loader::LoadedPlugin;

#[cfg(test)]
#[path = "middleware_test.rs"]
mod tests;

#[path = "middleware/plugin_middleware.rs"]
mod plugin_middleware;
pub use plugin_middleware::PluginMiddleware;
