//! Build ACP `initialize` response with full session capabilities.

// [TRAP] initialize 响应必须声明全部 session capabilities
// 与 TUI 路径的 AcpServerConfig 对齐，否则 client 无法使用对应功能。

use std::collections::HashMap;

use agent_client_protocol_schema::v1::{
    AgentCapabilities, AuthMethod, AuthMethodTerminal, InitializeResponse, McpCapabilities,
    PromptCapabilities, SessionCapabilities, SessionCloseCapabilities, SessionDeleteCapabilities,
    SessionForkCapabilities, SessionListCapabilities, SessionResumeCapabilities,
};
use agent_client_protocol_schema::ProtocolVersion;
use peri_acp_types::mcp_apps_facade::McpAppsCapability;
use peri_acp_types::PeriCaps;
use serde_json::Value;

/// Registry 旧标记与顶层兼容标记使用的 `_meta` 键（ACP 标准字段为
/// `clientCapabilities.auth.terminal`）。
const TERMINAL_AUTH_META_KEY: &str = "terminal-auth";
const SETUP_AUTH_METHOD_ID: &str = "peri-setup";
const SETUP_AUTH_METHOD_NAME: &str = "Configure Peri";
const SETUP_AUTH_METHOD_DESCRIPTION: &str =
    "Run Peri's one-time setup wizard to choose a model provider and save credentials";
const SETUP_AUTH_METHOD_ARG: &str = "--setup";
const SETUP_AUTH_ENV_KEY: &str = "PERI_ACP_SETUP";

/// initialize 响应里的 terminal auth 方法（唯一判定点）。
///
/// 只在客户端显式声明**认证**能力时广告，避免把 `clientCapabilities.terminal`
/// （`terminal/*` 工具能力）误当成认证能力：
///
/// - ACP v1：`clientCapabilities.auth.terminal == true`；
/// - Registry 旧标记：`clientCapabilities._meta["terminal-auth"] == true`；
/// - 顶层兼容标记：`params._meta["terminal-auth"] == true`。
///
/// 未声明任一标记时返回空列表（不冒充可用认证方式）。正常宿主与无配置降级
/// 宿主共用本函数。
pub fn terminal_auth_methods(params: &Value) -> Vec<AuthMethod> {
    let client_capabilities = params.get("clientCapabilities");
    let v1_terminal = client_capabilities
        .and_then(|capabilities| capabilities.get("auth"))
        .and_then(|auth| auth.get("terminal"))
        .and_then(Value::as_bool)
        == Some(true);
    let registry_meta = meta_flag_true(
        client_capabilities.and_then(|capabilities| capabilities.get("_meta")),
        TERMINAL_AUTH_META_KEY,
    );
    let top_meta = meta_flag_true(params.get("_meta"), TERMINAL_AUTH_META_KEY);
    if !v1_terminal && !registry_meta && !top_meta {
        return Vec::new();
    }
    vec![AuthMethod::Terminal(
        AuthMethodTerminal::new(SETUP_AUTH_METHOD_ID, SETUP_AUTH_METHOD_NAME)
            .description(SETUP_AUTH_METHOD_DESCRIPTION)
            .args(vec![SETUP_AUTH_METHOD_ARG.to_string()])
            .env(HashMap::from([(
                SETUP_AUTH_ENV_KEY.to_string(),
                "1".to_string(),
            )])),
    )]
}

fn meta_flag_true(meta: Option<&Value>, key: &str) -> bool {
    meta.and_then(Value::as_object)
        .and_then(|meta| meta.get(key))
        .and_then(Value::as_bool)
        == Some(true)
}

/// 无配置降级宿主的 initialize 响应：最小且诚实的 agent capabilities。
///
/// 不声明 `loadSession`、session 生命周期、MCP 与 prompt 能力（宿主此刻既没有
/// provider 也没有 session store）；`_meta` 只回显 [`PeriCaps::default`] 的关闭
/// 状态。`protocolVersion` 与正常 initialize 相同（V1），认证方法走同一判定。
pub fn build_auth_required_initialize_response(params: &Value) -> InitializeResponse {
    let capabilities = AgentCapabilities::new().meta(PeriCaps::default().to_agent_meta());
    InitializeResponse::new(ProtocolVersion::V1)
        .agent_capabilities(capabilities)
        .auth_methods(terminal_auth_methods(params))
}

/// Construct the full [`InitializeResponse`] with all session lifecycle
/// capabilities declared (load, list, close, resume, fork).
///
/// Echoes the client's declared peri caps back via `_meta` so the client
/// can verify which extensions the agent will honor.
///
/// `mcpCapabilities.acp` 声明 agent 支持 MCP over ACP（client 在会话 setup 中
/// 以 `type: "acp"` 声明 server，agent 经请求作用域的 `mcp/message` 调用它）。
/// 声明是硬前置：未声明的 client 不得假定 agent 会处理 `mcp/message`。
///
/// `apps`（可选）写入同层 `_meta["peri.mcpApps"]`：**只报已实现能力**（未实现
/// 四项在 `McpAppsFeatures` 上是私有字段，类型层强制）；`None` = 本部署未装配
/// Apps 后端 ⇒ **不声明**（不声称未接线的能力）。
///
/// Used by both TUI (MpscTransport) and stdio transport implementations.
pub fn build_initialize_response(
    peri_caps: &PeriCaps,
    apps: Option<&McpAppsCapability>,
) -> InitializeResponse {
    let caps = AgentCapabilities::new()
        .load_session(true)
        .prompt_capabilities(PromptCapabilities::new().image(true))
        .session_capabilities(
            SessionCapabilities::new()
                .list(SessionListCapabilities::new())
                .close(SessionCloseCapabilities::new())
                .resume(SessionResumeCapabilities::new())
                .fork(SessionForkCapabilities::new())
                .delete(SessionDeleteCapabilities::new()),
        )
        .mcp_capabilities(McpCapabilities::new().acp(true).http(true));
    let mut meta = peri_caps.to_agent_meta();
    if let Some(apps) = apps {
        meta.insert(
            "peri.mcpApps".into(),
            serde_json::to_value(apps).expect("McpAppsCapability 序列化不应失败"),
        );
    }
    let caps = caps.meta(meta);
    InitializeResponse::new(ProtocolVersion::V1).agent_capabilities(caps)
}

#[cfg(test)]
#[path = "init_auth_test.rs"]
mod init_auth_tests;
