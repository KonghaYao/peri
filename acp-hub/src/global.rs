//! 全局 ACP 请求处理
//!
//! 处理不需要路由到子进程的全局请求：
//! - initialize：返回 Hub 的 ACP 能力声明
//! - session/list：返回当前活跃 session 列表
//! - commands/list：返回支持的斜杠命令列表

use crate::error::ok_response;
use agent_client_protocol_schema::v1::{
    AgentCapabilities, InitializeResponse, PromptCapabilities, SessionCapabilities,
    SessionCloseCapabilities, SessionId, SessionInfo, SessionListCapabilities,
    SessionResumeCapabilities,
};
use agent_client_protocol_schema::ProtocolVersion;
use serde_json::Value;
use std::path::PathBuf;

/// 处理 initialize 请求，返回符合 ACP v1 规范的 InitializeResponse
pub fn handle_initialize(id: &Value) -> Value {
    let caps = AgentCapabilities::new()
        .load_session(true)
        .prompt_capabilities(PromptCapabilities::new())
        .session_capabilities(
            SessionCapabilities::new()
                .list(SessionListCapabilities::new())
                .close(SessionCloseCapabilities::new())
                .resume(SessionResumeCapabilities::new()),
        );
    let init_resp = InitializeResponse::new(ProtocolVersion::V1).agent_capabilities(caps);
    let result = serde_json::to_value(&init_resp).unwrap_or(Value::Null);
    ok_response(id, result)
}

/// 处理 session/list 请求，返回符合 ACP v1 规范的 SessionInfo 列表
pub fn handle_session_list(id: &Value, sessions: &[crate::router::SessionInfo]) -> Value {
    let list: Vec<Value> = sessions
        .iter()
        .map(|s| {
            let mut info =
                SessionInfo::new(SessionId::new(s.session_id.as_str()), PathBuf::from(&s.cwd));
            if let Some(ref title) = s.title {
                info = info.title(title.clone());
            }
            if let Some(ref updated) = s.updated_at {
                info = info.updated_at(updated.clone());
            }
            serde_json::to_value(&info).unwrap_or(Value::Null)
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
        // 关键：字段名应为 agentCapabilities 而非 capabilities
        assert!(resp["result"].get("agentCapabilities").is_some());
        assert!(resp["result"]
            .get("agentCapabilities")
            .unwrap()
            .get("sessionCapabilities")
            .is_some());
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

    #[test]
    fn test_handle_session_list_with_items() {
        let id = serde_json::json!(4);
        let sessions = vec![crate::router::SessionInfo {
            session_id: "abc-123".to_string(),
            cwd: "/home/user/project".to_string(),
            title: Some("My Session".to_string()),
            updated_at: Some("2026-07-18T12:00:00Z".to_string()),
            created_at: chrono::Utc::now(),
            status: crate::router::SessionStatus::Ready,
        }];
        let resp = handle_session_list(&id, &sessions);
        let list = resp["result"].as_array().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["sessionId"], "abc-123");
        assert_eq!(list[0]["cwd"], "/home/user/project");
        assert_eq!(list[0]["title"], "My Session");
    }
}
