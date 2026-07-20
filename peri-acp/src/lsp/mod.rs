//! LSP middleware integration.
//!
//! Re-exports `peri_lsp` types and provides integration with
//! `peri_middlewares::LspMiddleware` for the agent builder.
//!
//! LSP servers are configured in `SessionContext::lsp_servers`
//! and automatically registered when non-empty.

pub use peri_lsp::config::LspServerConfig;
