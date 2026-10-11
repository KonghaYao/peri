use std::ffi::OsString;
use std::str::FromStr;

pub(crate) fn argv_requests_settings_stdin(args: &[OsString]) -> bool {
    args.iter()
        .skip(1)
        .take_while(|arg| arg.to_str() != Some("--"))
        .any(|arg| arg.to_str() == Some("--settings-stdin"))
}

/// 统一创建 tokio runtime（4 workers，4MB stack），避免 7 处重复构造。
pub(crate) fn build_runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .thread_stack_size(4 * 1024 * 1024)
        .enable_all()
        .build()
        .map_err(Into::into)
}

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

impl From<PluginScope> for peri_acp_types::plugin::InstallScope {
    fn from(scope: PluginScope) -> Self {
        match scope {
            PluginScope::User => peri_acp_types::plugin::InstallScope::User,
            PluginScope::Project => peri_acp_types::plugin::InstallScope::Project,
            PluginScope::Local => peri_acp_types::plugin::InstallScope::Local,
        }
    }
}

#[cfg(test)]
#[path = "cli_args_test.rs"]
mod tests;
