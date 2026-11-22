//! 无配置降级宿主：在打开 session store / 资源 / 凭证之前应答 ACP 握手。
//!
//! 只有常规配置文件路径缺少 provider 时进入本模块（`--settings-stdin` 注入
//! 缺少 provider 仍在装配阶段失败退出）。它复用生产 [`StdioTransport`] 与同一
//! JSON-RPC 帧格式，但不是第二套业务 dispatch：
//!
//! - `initialize`：返回最小且诚实的 agent capabilities 与同一判定产生的
//!   terminal auth 方法；
//! - `session/*`：`-32000` + `AUTH_REQUIRED`，`data.reason="auth_required"` 携带
//!   authMethods、`setupCommand` 与提示；仓库内其它 `-32000` 用途靠该 reason
//!   区分；
//! - 其余请求：`-32601`，说明 Peri 尚未配置；
//! - 通知与响应：忽略且不回包；每回包后才读下一条，避免入站队列饱和；
//! - EOF：干净返回，由 `run_acp_stdio` 映射为退出码 0。

use serde_json::{json, Value};

use crate::transport::types::{AcpError, IncomingMessage};
use crate::transport::AcpTransport;

/// 与仓库既有 `-32000` 用途（stdio 背压、turn 失败）区分的 data 标记。
const AUTH_REQUIRED_REASON: &str = "auth_required";
const AUTH_REQUIRED_CODE: i64 = -32000;
const METHOD_NOT_FOUND_CODE: i64 = -32601;
const SETUP_COMMAND: &str = "peri --setup";
const AUTH_REQUIRED_HINT: &str = "Run `peri --setup` in a terminal to configure a model provider and credentials, then reconnect.";

/// 驱动降级宿主直到 stdin EOF 或写响应失败。
pub(super) async fn run_auth_required_gate(transport: std::sync::Arc<dyn AcpTransport>) {
    // 本连接在 initialize 中广告的认证方法；后续 AUTH_REQUIRED 的 data 复用同一
    // 份事实，不按 session 请求重新猜测（session 参数没有 client capabilities）。
    let mut advertised_auth_methods = Value::Array(Vec::new());
    while let Some(message) = transport.recv().await {
        let IncomingMessage::Request { id, method, params } = message else {
            // 通知与响应不回包：降级宿主没有待结算的 server→client 请求。
            continue;
        };
        let result = if method == "initialize" {
            let response = crate::dispatch::init::build_auth_required_initialize_response(&params);
            advertised_auth_methods = serde_json::to_value(&response.auth_methods)
                .unwrap_or_else(|_| Value::Array(Vec::new()));
            serde_json::to_value(response)
                .map_err(|error| AcpError::new(-32603, format!("Serialize failed: {error}")))
        } else if method.starts_with("session/") {
            Err(auth_required_error(&advertised_auth_methods))
        } else {
            Err(AcpError::new(
                METHOD_NOT_FOUND_CODE,
                format!(
                    "Peri is not configured; `{method}` is unavailable until `{SETUP_COMMAND}` completes"
                ),
            ))
        };
        // 先回包再读下一条：入站队列有界，连续请求不能靠预读堆积。
        if transport.send_response(id, result).await.is_err() {
            break;
        }
    }
}

fn auth_required_error(auth_methods: &Value) -> AcpError {
    AcpError::new(
        AUTH_REQUIRED_CODE,
        format!("AUTH_REQUIRED: Peri is not configured. Run `{SETUP_COMMAND}` to configure a model provider and credentials."),
    )
    .with_data(json!({
        "reason": AUTH_REQUIRED_REASON,
        "authMethods": auth_methods,
        "setupCommand": SETUP_COMMAND,
        "hint": AUTH_REQUIRED_HINT,
    }))
}
