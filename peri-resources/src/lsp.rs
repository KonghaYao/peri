//! peri-lsp 作为 resource 实现接入（伞形 PRD 决策 20：既有 crate 归位）。
//!
//! 门面镜像：LSP 能力（服务器池 / 客户端 / 诊断 / 协议类型）以本模块为
//! 唯一引用入口，消费方（Middleware 等）经 Resources 门面使用、不直接依赖
//! peri-lsp crate。实例化与持有（服务器池生命周期）随装配归位（L5）后
//! 收口至 Resources context，本模块仅为类型/能力出口，不解释业务语义。

pub use peri_lsp::client;
pub use peri_lsp::config;
pub use peri_lsp::diagnostics;
pub use peri_lsp::error;
pub use peri_lsp::jsonrpc;
pub use peri_lsp::pool;
pub use peri_lsp::protocol;
pub use peri_lsp::uri;
