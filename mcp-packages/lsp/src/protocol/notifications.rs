use lsp_types::*;
use serde_json::Value;

use crate::jsonrpc::JsonRpcNotification;

/// 构建 textDocument/didOpen 通知
pub fn did_open_notification(
    uri: &str,
    language_id: &str,
    version: i32,
    text: &str,
) -> JsonRpcNotification {
    JsonRpcNotification::new(
        "textDocument/didOpen",
        Some(serde_json::json!({
            "textDocument": {
                "uri": uri,
                "languageId": language_id,
                "version": version,
                "text": text
            }
        })),
    )
}

/// 构建 textDocument/didChange 通知（full sync 模式）
pub fn did_change_notification(uri: &str, version: i32, full_text: &str) -> JsonRpcNotification {
    JsonRpcNotification::new(
        "textDocument/didChange",
        Some(serde_json::json!({
            "textDocument": {
                "uri": uri,
                "version": version
            },
            "contentChanges": [{ "text": full_text }]
        })),
    )
}

/// 构建 textDocument/didSave 通知
pub fn did_save_notification(uri: &str, text: Option<&str>) -> JsonRpcNotification {
    let params = if let Some(t) = text {
        serde_json::json!({
            "textDocument": { "uri": uri },
            "text": t
        })
    } else {
        serde_json::json!({
            "textDocument": { "uri": uri }
        })
    };
    JsonRpcNotification::new("textDocument/didSave", Some(params))
}

/// 构建 initialized 通知
pub fn initialized_notification() -> JsonRpcNotification {
    JsonRpcNotification::new("initialized", Some(Value::Object(Default::default())))
}

/// 解析 publishDiagnostics 通知参数
pub fn parse_publish_diagnostics(params: &Value) -> Option<PublishDiagnosticsParams> {
    serde_json::from_value(params.clone()).ok()
}
