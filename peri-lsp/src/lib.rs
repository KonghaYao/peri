pub mod client;
pub mod config;
pub mod diagnostics;
pub mod error;
pub mod jsonrpc;
pub mod pool;
pub mod protocol;
pub mod uri;

pub use client::{LspClient, ServerState};
pub use config::{
    load_global_lsp_config, lsp_config_from_plugin, LspConfigFile, LspConfigSource, LspServerConfig,
};
pub use diagnostics::{
    DiagnosticEntry, DiagnosticSeverity, DiagnosticSummary, DiagnosticsRegistry,
};
pub use error::LspError;
pub use pool::LspServerPool;
pub use uri::{path_to_uri, uri_to_path};
