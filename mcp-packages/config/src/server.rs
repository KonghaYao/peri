use std::collections::hash_map::DefaultHasher;
use std::io;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;

use fs4::FileExt;
use peri_acp_types::configuration::{
    ConfigurationErrorKind, ConfigurationFailure, ConfigurationPaths, ConfigurationRequest,
    ConfigurationResponse, ConfigurationValue, CONFIGURATION_METHOD,
};
use rmcp::model::CustomRequest;
use rmcp::model::CustomResult;
use rmcp::model::Implementation;
use rmcp::model::ServerCapabilities;
use rmcp::model::ServerConfig;
use rmcp::service::RequestContext;
use rmcp::service::RoleServer;
use rmcp::ErrorData;
use rmcp::ServerHandler;
use uuid::Uuid;

#[cfg(test)]
#[path = "server_diagnostics_test.rs"]
mod diagnostic_tests;

#[path = "server/configuration_mcp_server.rs"]
mod configuration_mcp_server;
#[cfg(test)]
use configuration_mcp_server::failure;
pub use configuration_mcp_server::ConfigurationMcpServer;
