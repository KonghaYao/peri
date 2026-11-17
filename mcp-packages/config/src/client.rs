use std::fmt;
use std::fmt::{Debug, Formatter};
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use peri_acp_types::configuration::{
    ConfigurationErrorKind, ConfigurationPaths, ConfigurationRequest, ConfigurationResponse,
    ConfigurationValue, CONFIGURATION_METHOD,
};
use rmcp::model::ClientRequest;
use rmcp::model::CustomRequest;
use rmcp::model::ServerResult;
use rmcp::service::PeerRequestOptions;
use rmcp::ServiceExt;

use crate::ConfigurationMcpServer;

#[path = "client/configuration_client.rs"]
mod configuration_client;
pub use configuration_client::ConfigurationClient;
