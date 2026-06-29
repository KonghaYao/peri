//! DTO 转换层：peri_middlewares 运行时类型 → peri_acp_types DTO。
//!
//! P4b 类型隔离的核心桥梁。TUI 通过此模块将 ACP 层传入的运行时类型
//! 转换为 DTO，逐步消除对 peri_middlewares 的依赖。
//!
//! ## 当前兼容性
//!
//! 仅 MCP 的 ClientStatus / OAuthStatus / McpInitStatus 是 1:1 对应的。
//! 其他类型的 DTO 与运行时类型已有差异（变体/字段不同），
//! 需先同步 DTO 定义后才能添加转换函数。
//!
//! | 运行时类型 | DTO 对应 | 状态 |
//! |---|---|---|
//! | `ClientStatus` | `ClientStatusDto` | ✅ 1:1（tuple→struct variant 差异已处理） |
//! | `OAuthStatus` | `OAuthStatusDto` | ✅ 1:1 |
//! | `McpInitStatus` | `McpInitStatusDto` | ✅ 1:1 |
//! | `PermissionMode` | `PermissionModeDto` | ❌ 变体完全不同 |
//! | `InstallScope` | `InstallScopeDto` | ❌ DTO 缺少 Local 变体 |
//! | `ConfigSource` | `ConfigSourceDto` | ❌ tuple→struct variant + PathBuf→String |
//! | `ServerInfo` | `ServerInfoDto` | ❌ 字段重命名 + 缺失 |
//! | `MarketplaceSource` | `MarketplaceSourceDto` | ❌ 变体完全不同 |
//! | `RegisteredHook` | `RegisteredHookDto` | ❌ 结构完全不同 |
//! | `HookEvent` / `HookType` | DTO 对应 | ❌ 变体子集 |

use peri_acp_types::mcp_types::{ClientStatusDto, McpInitStatusDto, OAuthStatusDto};

// ── MCP 类型（1:1 兼容）──────────────────────────────────────────────

pub fn client_status_dto(s: peri_middlewares::mcp::ClientStatus) -> ClientStatusDto {
    match s {
        peri_middlewares::mcp::ClientStatus::Connected => ClientStatusDto::Connected,
        peri_middlewares::mcp::ClientStatus::Failed(reason) => ClientStatusDto::Failed { reason },
        peri_middlewares::mcp::ClientStatus::Disconnected => ClientStatusDto::Disconnected,
        peri_middlewares::mcp::ClientStatus::Disabled => ClientStatusDto::Disabled,
        peri_middlewares::mcp::ClientStatus::Uninitialized => ClientStatusDto::Uninitialized,
    }
}

pub fn oauth_status_dto(s: peri_middlewares::mcp::OAuthStatus) -> OAuthStatusDto {
    match s {
        peri_middlewares::mcp::OAuthStatus::None => OAuthStatusDto::None,
        peri_middlewares::mcp::OAuthStatus::Authorized => OAuthStatusDto::Authorized,
        peri_middlewares::mcp::OAuthStatus::NeedsAuthorization => {
            OAuthStatusDto::NeedsAuthorization
        }
    }
}

pub fn mcp_init_status_dto(s: peri_middlewares::mcp::McpInitStatus) -> McpInitStatusDto {
    match s {
        peri_middlewares::mcp::McpInitStatus::Pending => McpInitStatusDto::Pending,
        peri_middlewares::mcp::McpInitStatus::Initializing { connected, total } => {
            McpInitStatusDto::Initializing { connected, total }
        }
        peri_middlewares::mcp::McpInitStatus::Ready { total } => McpInitStatusDto::Ready { total },
        peri_middlewares::mcp::McpInitStatus::Failed(e) => McpInitStatusDto::Failed(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_status_failed_converts_to_struct_variant() {
        let dto = client_status_dto(peri_middlewares::mcp::ClientStatus::Failed(
            "timeout".into(),
        ));
        assert_eq!(
            dto,
            ClientStatusDto::Failed {
                reason: "timeout".into()
            }
        );
    }

    #[test]
    fn test_client_status_all_variants() {
        for s in [
            peri_middlewares::mcp::ClientStatus::Connected,
            peri_middlewares::mcp::ClientStatus::Failed("err".into()),
            peri_middlewares::mcp::ClientStatus::Disconnected,
            peri_middlewares::mcp::ClientStatus::Disabled,
            peri_middlewares::mcp::ClientStatus::Uninitialized,
        ] {
            let _ = client_status_dto(s);
        }
    }

    #[test]
    fn test_mcp_init_status_roundtrip() {
        let dto = mcp_init_status_dto(peri_middlewares::mcp::McpInitStatus::Initializing {
            connected: 2,
            total: 5,
        });
        assert_eq!(
            dto,
            McpInitStatusDto::Initializing {
                connected: 2,
                total: 5
            }
        );
    }
}
