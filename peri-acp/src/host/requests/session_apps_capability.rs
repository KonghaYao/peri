//! `_meta["peri.mcpApps"]` 的能力声明派生（§6.3）。
//!
//! 只报**已实现**能力（未实现四项在 `McpAppsFeatures` 上是私有字段，类型层强制）；
//! 未装配 Apps 后端 ⇒ 返回 `None`（不声明，也不声称已强制 entitlement ——
//! `EntitlementState::Unenforced` 保持，§8.2）。

use super::AcpServerConfig;

/// 后端 MCP Apps 能力声明（`_meta["peri.mcpApps"]`）的派生：**只报已实现能力**
/// （features 由 `McpAppsFeatures::implemented` 单一权威给出；未实现四项在类型上
/// 无法置真），`None` = 本部署未装配 Apps 后端（不声明，也不声称已强制 entitlement
/// —— `EntitlementState::Unenforced` 保持，§8.2）。
pub(super) fn apps_capability(
    cfg: &AcpServerConfig,
) -> Option<peri_acp_types::mcp_apps_facade::McpAppsCapability> {
    let profile = cfg
        .workspace_assembly
        .as_ref()
        .map(|assembly| &assembly.mcp_profile);
    let enabled = profile
        .map(|profile| profile.apps_enabled())
        .unwrap_or(false)
        || cfg.mcp_apps_relay.is_some();
    if !enabled {
        return None;
    }
    let mime_types: Vec<String> = profile
        .map(|profile| {
            profile
                .apps_mime_types()
                .map(str::to_string)
                .collect::<Vec<String>>()
        })
        .filter(|mime_types| !mime_types.is_empty())
        .unwrap_or_else(|| vec![peri_acp_types::mcp_apps::MCP_APPS_HTML_MIME.to_string()]);
    Some(peri_acp_types::mcp_apps_facade::McpAppsCapability::backend(
        mime_types,
    ))
}
