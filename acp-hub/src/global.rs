//! 全局 ACP 请求处理
//!
//! 处理不需要路由到子进程的全局请求：
//! - initialize：返回 Hub 的 ACP 能力声明
//! - session/list：返回当前活跃 session 列表
//! - commands/list：返回支持的斜杠命令列表

use crate::error::ok_response;
use serde_json::Value;

/// 处理 initialize 请求
pub fn handle_initialize(id: &Value) -> Value {
    ok_response(
        id,
        serde_json::json!({
            "protocolVersion": 1,
            "capabilities": {
                "prompt": {
                    "stream": true,
                },
                "elicitation": {},
                "session": {
                    "management": true
                },
                "experimental": {
                    "acp-hub": true
                }
            },
            "serverInfo": {
                "name": "acp-hub",
                "version": env!("CARGO_PKG_VERSION")
            }
        }),
    )
}

/// 处理 session/list 请求
pub fn handle_session_list(id: &Value, sessions: &[crate::router::SessionInfo]) -> Value {
    let list: Vec<Value> = sessions
        .iter()
        .map(|s| {
            serde_json::json!({
                "session_id": s.session_id,
                "cwd": s.cwd,
                "created_at": s.created_at.to_rfc3339(),
                "status": match s.status {
                    crate::router::SessionStatus::Ready => "ready",
                    crate::router::SessionStatus::Crashed => "crashed",
                }
            })
        })
        .collect();
    ok_response(id, serde_json::json!(list))
}

/// 处理 commands/list 请求，返回静态命令列表
pub fn handle_commands_list(id: &Value) -> Value {
    ok_response(
        id,
        serde_json::json!([
            {
                "name": "/clear",
                "description": "Clear the conversation history"
            },
            {
                "name": "/compact",
                "description": "Compact the conversation context"
            },
            {
                "name": "/model",
                "description": "Switch the LLM model",
                "arguments": [{"name": "model", "description": "Model name or alias", "required": true}]
            },
            {
                "name": "/help",
                "description": "Show available commands"
            }
        ]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_handle_initialize() {
        let id = serde_json::json!(1);
        let resp = handle_initialize(&id);
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["result"]["protocolVersion"], 1);
        assert_eq!(resp["result"]["serverInfo"]["name"], "acp-hub");
    }

    #[test]
    fn test_handle_commands_list() {
        let id = serde_json::json!(2);
        let resp = handle_commands_list(&id);
        let cmds = resp["result"].as_array().unwrap();
        assert!(cmds.len() >= 2);
        assert_eq!(cmds[0]["name"], "/clear");
    }

    #[test]
    fn test_handle_session_list_empty() {
        let id = serde_json::json!(3);
        let sessions: Vec<crate::router::SessionInfo> = vec![];
        let resp = handle_session_list(&id, &sessions);
        assert_eq!(resp["id"], 3);
        let list = resp["result"].as_array().unwrap();
        assert!(list.is_empty());
    }
}
