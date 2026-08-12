//! Build ACP `initialize` response with full session capabilities.

// [TRAP] initialize 响应必须声明全部 session capabilities
// 与 TUI 路径的 AcpServerConfig 对齐，否则 client 无法使用对应功能。

use agent_client_protocol_schema::v1::{
    AgentCapabilities, InitializeResponse, PromptCapabilities, SessionCapabilities,
    SessionCloseCapabilities, SessionDeleteCapabilities, SessionForkCapabilities,
    SessionListCapabilities, SessionResumeCapabilities,
};
use agent_client_protocol_schema::ProtocolVersion;
use peri_acp_types::PeriCaps;

/// Construct the full [`InitializeResponse`] with all session lifecycle
/// capabilities declared (load, list, close, resume, fork).
///
/// Echoes the client's declared peri caps back via `_meta` so the client
/// can verify which extensions the agent will honor.
///
/// Used by both TUI (MpscTransport) and stdio transport implementations.
pub fn build_initialize_response(peri_caps: &PeriCaps) -> InitializeResponse {
    let caps = AgentCapabilities::new()
        .load_session(true)
        .prompt_capabilities(PromptCapabilities::new())
        .session_capabilities(
            SessionCapabilities::new()
                .list(SessionListCapabilities::new())
                .close(SessionCloseCapabilities::new())
                .resume(SessionResumeCapabilities::new())
                .fork(SessionForkCapabilities::new())
                .delete(SessionDeleteCapabilities::new()),
        );
    let caps = caps.meta(peri_caps.to_agent_meta());
    InitializeResponse::new(ProtocolVersion::V1).agent_capabilities(caps)
}
