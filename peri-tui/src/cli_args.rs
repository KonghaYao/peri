use std::str::FromStr;

// ─── OutputFormat ─────────────────────────────────────────────────────────

/// 输出格式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    #[default]
    Text,
    Json,
    StreamJson,
}

impl FromStr for OutputFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "text" => Ok(OutputFormat::Text),
            "json" => Ok(OutputFormat::Json),
            "stream-json" => Ok(OutputFormat::StreamJson),
            _ => Err(format!(
                "未知的输出格式: '{}'（可选值: text, json, stream-json）",
                s
            )),
        }
    }
}

// ─── PluginScope ──────────────────────────────────────────────────────────

/// 插件安装范围
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PluginScope {
    #[default]
    User,
    Project,
    Local,
}

impl FromStr for PluginScope {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "user" => Ok(PluginScope::User),
            "project" => Ok(PluginScope::Project),
            "local" => Ok(PluginScope::Local),
            _ => Err(format!(
                "未知的插件范围: '{}'（可选值: user, project, local）",
                s
            )),
        }
    }
}

impl From<PluginScope> for peri_middlewares::plugin::InstallScope {
    fn from(scope: PluginScope) -> Self {
        match scope {
            PluginScope::User => peri_middlewares::plugin::InstallScope::User,
            PluginScope::Project => peri_middlewares::plugin::InstallScope::Project,
            PluginScope::Local => peri_middlewares::plugin::InstallScope::Local,
        }
    }
}

#[cfg(test)]
#[path = "cli_args_test.rs"]
mod tests;
