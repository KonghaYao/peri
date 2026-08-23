//! 通用 JavaScript 子进程执行宿主。
//!
//! 隐藏子进程生命周期、NDJSON JSON-RPC、pending request、stderr 消费与取消；
//! 具体 method 与 params 的业务含义由上层 Adapter 解释。

mod error;
mod executor;
mod host;
mod process_tree;
mod rpc;

pub use error::{JsExecutionFailure, JsRuntimeError, ResourceKind, Result};
pub use executor::{
    JsExecutionLimits, JsExecutionRequest, JsExecutionResult, JsExecutor, JsRpcRouter,
};
pub use host::{JsExecutionHost, JsProcessSpec};
pub use rpc::{parse_message, IncomingMessage, JsonRpcError, ParsedMessage, RpcChannel};
