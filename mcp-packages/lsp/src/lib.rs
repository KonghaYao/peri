pub mod client;
pub mod config;
pub mod diagnostics;
pub mod error;
pub mod jsonrpc;
pub mod pool;
pub mod protocol;
pub mod uri;

pub mod formatters;
mod server;
pub mod tool;

pub use client::{LspClient, ServerState};
pub use config::{
    load_global_lsp_config, load_merged_lsp_servers, lsp_config_from_plugin, LspConfigFile,
    LspConfigSource, LspServerConfig,
};
pub use diagnostics::{
    DiagnosticEntry, DiagnosticSeverity, DiagnosticSummary, DiagnosticsRegistry,
};
pub use error::LspError;
pub use pool::LspServerPool;
pub use server::LspMcpServer;
pub use tool::LspTool;
pub use uri::{path_to_uri, uri_to_path};

/// 构造 host 级唯一 LSP pool。
///
/// pool 的具体所有权由 builtin `lsp` handler 与 host shutdown 通过同一 `Arc`
/// 共享；本函数只提供 host 装配所需的构造 seam，不创建 session 级 pool。
pub fn create_host_lsp_pool(
    cwd: &str,
    configs: &[LspServerConfig],
) -> std::sync::Arc<LspServerPool> {
    let config = LspConfigFile {
        lsp_servers: configs
            .iter()
            .map(|server| (server.name.clone(), server.clone()))
            .collect(),
    };
    std::sync::Arc::new(LspServerPool::new(cwd, config))
}
