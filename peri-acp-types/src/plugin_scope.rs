//! 插件来源作用域身份（M6）与相关共享校验契约。
//!
//! 身份取自安装记录（`installed_plugins.json` 的 id / scope / projectPath /
//! origin）与启用选择事实，**不靠插件名或安装路径前缀推断**：同名插件在不同范围
//! 安装或由不同项目声明时是不同来源。消费方是「用户全局安装 vs 项目声明」的区分
//! 与 hook 执行来源信任门控（H4）。
//!
//! 本模块由 `plugin.rs` 迁出（STD-SIZE-001）；`plugin.rs` 保留 re-export，
//! 调用方无需改路径。

use serde::{Deserialize, Serialize};

use crate::plugin::{InstallScope, PluginOrigin};

impl PluginOrigin {
    /// 是否由外部工具（Claude Code）安装，非 Peri 管理
    pub fn is_external(&self) -> bool {
        matches!(
            self,
            Self::ClaudeCodeInstalled | Self::UserClaude | Self::ProjectClaude
        )
    }

    /// 稳定标签（来源身份 / 诊断用；不是 wire 值）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PeriInstalled => "peri-installed",
            Self::ClaudeCodeInstalled => "claude-installed",
            Self::UserClaude => "claude-user",
            Self::ProjectClaude => "claude-project",
        }
    }
}

impl InstallScope {
    /// 稳定标签（来源身份 / 诊断用；不是 wire 值）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
            Self::Local => "local",
        }
    }
}

/// 插件来源作用域身份（M6）。
///
/// 身份取自安装记录（`installed_plugins.json` 的 id / scope / projectPath /
/// origin）与启用选择事实，**不靠插件名或安装路径前缀推断**：同名插件在不同
/// 范围安装或由不同项目声明时是不同来源。消费方是「用户全局安装 vs 项目声明」
/// 的区分（用户级 `/ 项目级` 层选择规则见 `select_enabled_plugins`）与 hook
/// 执行来源信任门控（H4）。
///
/// 字段全部非敏感：不含 env / headers / OAuth 内容，也不含插件文件内容
/// （摘要另由消费方按需计算，ARC-SECRET-001）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PluginScope {
    /// 安装记录 id（`name@marketplace`）。
    pub plugin_id: String,
    /// 来源机制（Peri 安装 / Claude Code CLI 安装）。
    pub origin: PluginOrigin,
    /// 安装/启用范围：user（用户全局安装）/ project / local（项目声明）。
    pub install_scope: InstallScope,
    /// project / local 范围对应的项目路径（安装记录事实，不由当轮 cwd 反推）。
    pub project_path: Option<String>,
}

impl PluginScope {
    /// 归一化来源身份：`plugin:{origin}/{scope}/{plugin_id}[@{project_path}]`。
    ///
    /// 信任绑定与诊断共用同一串；调用方（hook 信任）在其前加来源类别前缀，
    /// 不在此处拼装信任文件格式。
    pub fn source_identity(&self) -> String {
        let mut identity = format!(
            "plugin:{}/{}/{}",
            self.origin.as_str(),
            self.install_scope.as_str(),
            self.plugin_id
        );
        if let Some(project) = self
            .project_path
            .as_deref()
            .map(str::trim)
            .filter(|path| !path.is_empty())
        {
            identity.push('@');
            identity.push_str(project);
        }
        identity
    }
}

#[cfg(test)]
mod tests {
    use crate::plugin::{McpServerConfig, McpServerConfigValidationError};

    /// 仅含缺省字段的 typed 配置（`McpServerConfig` 没有 `Default`）。
    fn empty_config() -> McpServerConfig {
        McpServerConfig {
            command: None,
            args: None,
            env: None,
            url: None,
            headers: None,
            oauth: None,
            disabled: None,
            subscriptions: None,
            system_mcp: None,
            system_mcp_tools: None,
            system_mcp_timeout: None,
            source: None,
        }
    }

    fn parse(json: &str) -> Result<McpServerConfig, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// M7：`disabled = true` 与 `system_mcp = true` 的组合对**任何** server 名都非法
    /// （不限于 builtin 实例）；共享 `validate` 是唯一判定，准入层据此定位到来源名。
    #[test]
    fn test_disabled_with_system_mcp_is_rejected_by_shared_validation() {
        for json in [
            r#"{"command":"npx","disabled":true,"system_mcp":true}"#,
            r#"{"command":"npx","disabled":true,"system_mcp":true,"system_mcp_tools":[]}"#,
            r#"{"command":"plain","url":"https://example.invalid/mcp","disabled":true,"system_mcp":true}"#,
        ] {
            // wire 解析只做与来源无关的规则；该组合的可诊断拒绝由配置级/合并级
            // 准入报出（`validate_servers`、插件严格路径、写回前置校验）。
            let config = parse(json).expect("wire 解析不在此处拒绝该组合");
            assert_eq!(
                config.validate().unwrap_err(),
                McpServerConfigValidationError::DisabledWithSystemMcp,
                "共享 validate 必须拒绝: {json}"
            );
            assert!(
                McpServerConfigValidationError::DisabledWithSystemMcp
                    .to_string()
                    .contains("disabled = true cannot be combined with system_mcp = true"),
                "固定规则正文必须保留"
            );
        }

        let typed = McpServerConfig {
            disabled: Some(true),
            system_mcp: Some(true),
            ..empty_config()
        };
        assert_eq!(
            typed.validate().unwrap_err(),
            McpServerConfigValidationError::DisabledWithSystemMcp
        );

        // 只写其中一个开关仍然合法：唯一合法的用户关闭写法是只写 disabled。
        assert!(McpServerConfig {
            disabled: Some(true),
            ..empty_config()
        }
        .validate()
        .is_ok());
    }
}
