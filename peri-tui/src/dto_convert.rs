//! DTO 转换层：peri_middlewares 运行时类型 → peri_acp_types DTO。
//!
//! P4b 类型隔离的核心桥梁。TUI 通过此模块将 ACP 层传入的运行时类型
//! 转换为 DTO，逐步消除对 peri_middlewares 的依赖。
//!
//! ## 当前兼容性
//!
//! | 运行时类型 | DTO 对应 | 状态 |
//! |---|---|---|
//! | `ClientStatus` | `ClientStatusDto` | ✅ 1:1（tuple→struct variant 差异已处理） |
//! | `OAuthStatus` | `OAuthStatusDto` | ✅ 1:1 |
//! | `McpInitStatus` | `McpInitStatusDto` | ✅ 1:1 |
//! | `OAuthCallbackResult` | `OAuthCallbackResultDto` | ✅ 1:1 |
//! | `ConfigSource` | `ConfigSourceDto` | ✅ tuple→struct variant + PathBuf→String |
//! | `ServerInfo` | `ServerInfoDto` | ✅ 复合转换（含上述所有 DTO） |
//! | `PermissionMode` | `PermissionModeDto` | ✅ 1:1 |
//! | `InstallScope` | `InstallScopeDto` | ✅ DTO 已同步 Local 变体 |
//! | `MarketplaceSource` | `MarketplaceSourceDto` | ❌ DTO 与运行时完全不同（Git/Local/Registry vs GitHub/Git/Url/File/Directory/Npm） |
//! | `RegisteredHook` | `RegisteredHookDto` | ❌ 结构完全不同 |
//! | `HookEvent` / `HookType` | DTO 对应 | ❌ 变体子集 |
//! | `SkillMetadata` | `SkillMetadataDto` | ✅ 1:1（PathBuf→String + SkillSource→SkillSourceDto） |
//! | `SkillSource` | `SkillSourceDto` | ✅ 1:1（含 Global + Agm 扩展） |

use peri_acp_types::mcp_types::{
    ClientStatusDto, ConfigSourceDto, McpInitStatusDto, OAuthCallbackResultDto, OAuthStatusDto,
    ServerInfoDto,
};
use peri_acp_types::permission::PermissionModeDto;
use peri_acp_types::plugin_types::InstallScopeDto;
use peri_acp_types::skill::{SkillMetadataDto, SkillSourceDto};

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

pub fn oauth_callback_result_dto(
    r: peri_middlewares::mcp::OAuthCallbackResult,
) -> OAuthCallbackResultDto {
    OAuthCallbackResultDto {
        code: r.code,
        state: r.state,
    }
}

pub fn config_source_dto(s: peri_middlewares::mcp::ConfigSource) -> ConfigSourceDto {
    match s {
        peri_middlewares::mcp::ConfigSource::Project(p) => ConfigSourceDto::Project {
            path: p.to_string_lossy().into_owned(),
        },
        peri_middlewares::mcp::ConfigSource::Global(p) => ConfigSourceDto::Global {
            path: p.to_string_lossy().into_owned(),
        },
        peri_middlewares::mcp::ConfigSource::Plugin => ConfigSourceDto::Plugin,
    }
}

pub fn server_info_dto(s: peri_middlewares::mcp::ServerInfo) -> ServerInfoDto {
    ServerInfoDto {
        name: s.name,
        transport_type: s.transport_type,
        status: client_status_dto(s.status),
        tool_count: s.tool_count,
        resource_count: s.resource_count,
        oauth_status: oauth_status_dto(s.oauth_status),
        source: s.source.map(config_source_dto),
        url: s.url,
        plugin_source: s.plugin_source,
    }
}

// ── OAuth 回调桥接 ──────────────────────────────────────────────────

/// 桥接 OAuth 回调 channel：runtime `Sender<OAuthCallbackResult>` →
/// DTO `Sender<OAuthCallbackResultDto>`。
///
/// spawn 一个后台 task，在 DTO 结果到达时转换回 runtime 类型并转发到原 channel。
/// OAuth 流程是低频事件，per-call spawn 开销可忽略。
pub fn bridge_oauth_callback(
    runtime_tx: tokio::sync::oneshot::Sender<peri_middlewares::mcp::OAuthCallbackResult>,
) -> tokio::sync::oneshot::Sender<OAuthCallbackResultDto> {
    let (dto_tx, dto_rx) = tokio::sync::oneshot::channel::<OAuthCallbackResultDto>();
    tokio::spawn(async move {
        if let Ok(dto) = dto_rx.await {
            let _ = runtime_tx.send(peri_middlewares::mcp::OAuthCallbackResult {
                code: dto.code,
                state: dto.state,
            });
        }
    });
    dto_tx
}

// ── Skill 类型 ──────────────────────────────────────────────────────

fn skill_source_dto(s: peri_middlewares::skills::loader::SkillSource) -> SkillSourceDto {
    match s {
        peri_middlewares::skills::loader::SkillSource::User => SkillSourceDto::User,
        peri_middlewares::skills::loader::SkillSource::Global => SkillSourceDto::Global,
        peri_middlewares::skills::loader::SkillSource::Project => SkillSourceDto::Project,
        peri_middlewares::skills::loader::SkillSource::Plugin => SkillSourceDto::Plugin,
        peri_middlewares::skills::loader::SkillSource::Builtin => SkillSourceDto::Builtin,
    }
}

pub fn skill_metadata_dto(s: peri_middlewares::skills::loader::SkillMetadata) -> SkillMetadataDto {
    SkillMetadataDto {
        name: s.name,
        description: s.description,
        path: s.path.to_string_lossy().into_owned(),
        source: skill_source_dto(s.source),
        plugin_name: s.plugin_name,
        disabled: false,
    }
}

// ── Permission 类型 ──────────────────────────────────────────────────

pub fn permission_mode_dto(m: peri_middlewares::prelude::PermissionMode) -> PermissionModeDto {
    match m {
        peri_middlewares::prelude::PermissionMode::Default => PermissionModeDto::Default,
        peri_middlewares::prelude::PermissionMode::AcceptEdit => PermissionModeDto::AcceptEdit,
        peri_middlewares::prelude::PermissionMode::AutoMode => PermissionModeDto::AutoMode,
        peri_middlewares::prelude::PermissionMode::Bypass => PermissionModeDto::Bypass,
    }
}

// ── Plugin 类型 ─────────────────────────────────────────────────────

pub fn install_scope_dto(s: peri_middlewares::plugin::InstallScope) -> InstallScopeDto {
    match s {
        peri_middlewares::plugin::InstallScope::User => InstallScopeDto::User,
        peri_middlewares::plugin::InstallScope::Project => InstallScopeDto::Project,
        peri_middlewares::plugin::InstallScope::Local => InstallScopeDto::Local,
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

    #[test]
    fn test_oauth_callback_result_conversion() {
        let dto = oauth_callback_result_dto(peri_middlewares::mcp::OAuthCallbackResult {
            code: "abc".into(),
            state: "xyz".into(),
        });
        assert_eq!(dto.code, "abc");
        assert_eq!(dto.state, "xyz");
    }

    #[test]
    fn test_config_source_all_variants() {
        use std::path::PathBuf;
        // Project
        let dto = config_source_dto(peri_middlewares::mcp::ConfigSource::Project(PathBuf::from(
            "/tmp/.mcp.json",
        )));
        assert_eq!(
            dto,
            ConfigSourceDto::Project {
                path: "/tmp/.mcp.json".into()
            }
        );
        // Global
        let dto = config_source_dto(peri_middlewares::mcp::ConfigSource::Global(PathBuf::from(
            "/home/user/settings.json",
        )));
        assert_eq!(
            dto,
            ConfigSourceDto::Global {
                path: "/home/user/settings.json".into()
            }
        );
        // Plugin
        let dto = config_source_dto(peri_middlewares::mcp::ConfigSource::Plugin);
        assert_eq!(dto, ConfigSourceDto::Plugin);
    }

    #[test]
    fn test_server_info_full_conversion() {
        let info = peri_middlewares::mcp::ServerInfo {
            name: "test-server".into(),
            transport_type: "stdio".into(),
            status: peri_middlewares::mcp::ClientStatus::Connected,
            tool_count: 5,
            resource_count: 3,
            oauth_status: peri_middlewares::mcp::OAuthStatus::Authorized,
            source: None,
            url: Some("http://localhost:8080".into()),
            plugin_source: Some("test@marketplace".into()),
        };
        let dto = server_info_dto(info);
        assert_eq!(dto.name, "test-server");
        assert_eq!(dto.transport_type, "stdio");
        assert_eq!(dto.status, ClientStatusDto::Connected);
        assert_eq!(dto.tool_count, 5);
        assert_eq!(dto.resource_count, 3);
        assert_eq!(dto.oauth_status, OAuthStatusDto::Authorized);
        assert!(dto.source.is_none());
        assert_eq!(dto.url, Some("http://localhost:8080".into()));
        assert_eq!(dto.plugin_source, Some("test@marketplace".into()));
    }

    #[test]
    fn test_skill_metadata_conversion() {
        use peri_middlewares::skills::loader::{SkillMetadata, SkillSource};
        let s = SkillMetadata {
            name: "writer".into(),
            description: "A writing skill".into(),
            path: std::path::PathBuf::from("/home/user/.claude/skills/writer/SKILL.md"),
            source: SkillSource::User,
            plugin_name: None,
        };
        let dto = skill_metadata_dto(s);
        assert_eq!(dto.name, "writer");
        assert_eq!(dto.description, "A writing skill");
        assert_eq!(dto.path, "/home/user/.claude/skills/writer/SKILL.md");
        assert_eq!(dto.source, SkillSourceDto::User);
        assert_eq!(dto.plugin_name, None);
        assert!(!dto.disabled);
    }

    #[test]
    fn test_skill_source_all_variants() {
        use peri_middlewares::skills::loader::SkillSource;
        // 验证全部 5 个运行时变体都能转换
        for s in [
            SkillSource::User,
            SkillSource::Global,
            SkillSource::Project,
            SkillSource::Plugin,
            SkillSource::Builtin,
        ] {
            let _ = skill_source_dto(s);
        }
    }

    #[test]
    fn test_skill_metadata_with_plugin() {
        use peri_middlewares::skills::loader::{SkillMetadata, SkillSource};
        let s = SkillMetadata {
            name: "tdd".into(),
            description: "TDD skill".into(),
            path: std::path::PathBuf::from("/tmp/plugins/tdd/SKILL.md"),
            source: SkillSource::Plugin,
            plugin_name: Some("tdd-plugin".into()),
        };
        let dto = skill_metadata_dto(s);
        assert_eq!(dto.source, SkillSourceDto::Plugin);
        assert_eq!(dto.plugin_name, Some("tdd-plugin".into()));
    }

    #[test]
    fn test_install_scope_conversion() {
        assert_eq!(
            install_scope_dto(peri_middlewares::plugin::InstallScope::User),
            InstallScopeDto::User
        );
        assert_eq!(
            install_scope_dto(peri_middlewares::plugin::InstallScope::Project),
            InstallScopeDto::Project
        );
        assert_eq!(
            install_scope_dto(peri_middlewares::plugin::InstallScope::Local),
            InstallScopeDto::Local
        );
    }

    #[test]
    fn test_permission_mode_all_variants() {
        use peri_middlewares::prelude::PermissionMode;
        // 验证全部 4 个运行时变体都能转换
        for m in [
            PermissionMode::Default,
            PermissionMode::AcceptEdit,
            PermissionMode::AutoMode,
            PermissionMode::Bypass,
        ] {
            let _ = permission_mode_dto(m);
        }
    }

    #[test]
    fn test_permission_mode_dto_mapping() {
        use peri_middlewares::prelude::PermissionMode;
        assert_eq!(
            permission_mode_dto(PermissionMode::Default),
            PermissionModeDto::Default
        );
        assert_eq!(
            permission_mode_dto(PermissionMode::AcceptEdit),
            PermissionModeDto::AcceptEdit
        );
        assert_eq!(
            permission_mode_dto(PermissionMode::AutoMode),
            PermissionModeDto::AutoMode
        );
        assert_eq!(
            permission_mode_dto(PermissionMode::Bypass),
            PermissionModeDto::Bypass
        );
    }
}
