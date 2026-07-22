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
#[path = "global_test.rs"]
mod tests;
