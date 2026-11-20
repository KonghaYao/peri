//! 宿主提供的 Workspace 绑定（bare 初始化路径的 typed 输入）。
//!
//! 三态语义（只有前两态由本模块表达；「未绑定」= 池里没有该绑定）：
//!
//! - [`HostWorkspaceBinding::Remote`]：显式 descriptor。本会话的 workspace 面**就是**
//!   这台 HTTP MCP：发现、工具注入、资源面都走它，不再装配本机 builtin workspace。
//! - [`HostWorkspaceBinding::Absent`]：显式「无 workspace」。不装配本机 workspace、
//!   不探测本机目录，也不静默回退到 builtin workspace。
//!
//! 凭据纪律：
//!
//! - header 值只有两个来源——调用方内存（[`HeaderValueSource::Memory`]）或环境变量
//!   （[`HeaderValueSource::Env`]，绑定时解析一次）。
//! - 所有本模块类型的 `Debug` 都脱敏（只出现 header 名与环境变量名）；错误消息同样
//!   不包含 header 值。URL 只打印 scheme/host/port（query/fragment 可能带凭据）。
//!
//! 名字约束：descriptor 的 `name` 只接受 [`HOST_WORKSPACE_SERVER_NAME`]（`workspace`）。
//! 其他名字属于部署方普通 MCP，走既有配置/会话声明路径，不经过本输入——避免同一入口
//! 悄悄承担「任意 server 注入」的第二职责。

use std::collections::{HashMap, HashSet};
use std::fmt;

use thiserror::Error;

use super::config::{ConfigSource, McpServerConfig};

/// 宿主 workspace descriptor 唯一接受的 server 名。
///
/// 只有该名会得到 `ConfigSource::WorkspaceRemote` + `system_mcp = true`，
/// 从而被资源面识别为宿主任命的外部执行环境。
pub const HOST_WORKSPACE_SERVER_NAME: &str = "workspace";

/// 宿主 workspace 绑定的 header 值来源。
#[derive(Clone)]
pub enum HeaderValueSource {
    /// 调用方内存中已有的值（例如一次性 Bearer）。
    Memory(String),
    /// 环境变量名；绑定时解析一次，之后只保留解析后的值。
    Env { var: String },
}

impl fmt::Debug for HeaderValueSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Memory(_) => f.write_str("Memory(***)"),
            Self::Env { var } => f.debug_tuple("Env").field(var).finish(),
        }
    }
}

/// 单个宿主 workspace header。
#[derive(Clone)]
pub struct WorkspaceHeader {
    pub(crate) name: String,
    pub(crate) value: HeaderValueSource,
}

impl WorkspaceHeader {
    /// header 值来自调用方内存。
    #[must_use]
    pub fn memory(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: HeaderValueSource::Memory(value.into()),
        }
    }

    /// header 值来自环境变量（绑定时解析；变量名进入错误与 Debug，值不出现）。
    #[must_use]
    pub fn from_env(name: impl Into<String>, var: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: HeaderValueSource::Env { var: var.into() },
        }
    }
}

impl fmt::Debug for WorkspaceHeader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkspaceHeader")
            .field("name", &self.name)
            .field("value", &self.value)
            .finish()
    }
}

/// 宿主给出的 workspace MCP descriptor（`name` / `url` / `headers`）。
#[derive(Clone)]
pub struct WorkspaceDescriptor {
    pub(crate) name: String,
    pub(crate) url: String,
    pub(crate) headers: Vec<WorkspaceHeader>,
}

impl WorkspaceDescriptor {
    /// 完整构造。`name` 必须是 [`HOST_WORKSPACE_SERVER_NAME`]；校验在绑定时进行。
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        url: impl Into<String>,
        headers: Vec<WorkspaceHeader>,
    ) -> Self {
        Self {
            name: name.into(),
            url: url.into(),
            headers,
        }
    }

    /// 便捷构造：`workspace` 名 + 显式 headers。
    #[must_use]
    pub fn workspace(url: impl Into<String>, headers: Vec<WorkspaceHeader>) -> Self {
        Self::new(HOST_WORKSPACE_SERVER_NAME, url, headers)
    }
}

impl fmt::Debug for WorkspaceDescriptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkspaceDescriptor")
            .field("name", &self.name)
            .field("url", &redacted_url(&self.url))
            .field("headers", &self.headers)
            .finish()
    }
}

/// 宿主 workspace 绑定：显式远程 descriptor 或显式无 workspace。
#[derive(Debug, Clone)]
pub enum HostWorkspaceBinding {
    /// 显式远程 workspace MCP；本会话不再装配本机 builtin workspace。
    Remote(WorkspaceDescriptor),
    /// 显式无 workspace：不装配本机 workspace、不探测本机目录、不回退。
    Absent,
}

impl HostWorkspaceBinding {
    /// [`Self::Remote`] 的构造别名。
    #[must_use]
    pub fn remote(descriptor: WorkspaceDescriptor) -> Self {
        Self::Remote(descriptor)
    }

    /// [`Self::Absent`] 的构造别名。
    #[must_use]
    pub fn absent() -> Self {
        Self::Absent
    }

    /// 校验并按 header 来源解析出绑定事实。
    ///
    /// 这是**唯一**读取环境变量的位置：绑定之后环境变量再变化不影响本池。
    pub(crate) fn resolve(&self) -> Result<ResolvedHostWorkspace, HostWorkspaceBindingError> {
        match self {
            Self::Remote(descriptor) => descriptor.resolve().map(ResolvedHostWorkspace::Remote),
            Self::Absent => Ok(ResolvedHostWorkspace::Absent),
        }
    }
}

/// 绑定失败原因（不含 header 值；URL 只给出契约要求，不回显输入）。
#[derive(Debug, Error, PartialEq, Eq)]
pub enum HostWorkspaceBindingError {
    /// 初始化已经开始（或池已关闭）：typed 输入必须在 MCP 初始化前绑定。
    #[error("host workspace binding must be set before MCP initialization")]
    WindowClosed,
    /// 每个池只接受一次绑定（与 `set_session_servers` 同一 one-shot 规则）。
    #[error("host workspace binding is already set")]
    AlreadySet,
    /// descriptor 名不是 `workspace`：其他 MCP 名字走既有部署配置路径。
    #[error("host workspace descriptor name must be `workspace`: {name}")]
    NameNotWorkspace { name: String },
    /// URL 必须以 `http://` 或 `https://` 开头（不回显 URL，避免 query 里的凭据进入日志）。
    #[error("host workspace descriptor URL must start with http:// or https://")]
    InvalidUrl,
    /// header 名为空。
    #[error("host workspace descriptor header name must not be empty")]
    EmptyHeaderName,
    /// header 名重复（大小写不敏感，与 HTTP 语义一致）。
    #[error("host workspace descriptor header duplicated: {name}")]
    DuplicateHeader { name: String },
    /// 环境变量不存在。
    #[error("host workspace header `{header}` requires environment variable `{var}`")]
    MissingHeaderEnvironment { header: String, var: String },
    /// 环境变量值不是合法 UTF-8。
    #[error("host workspace header `{header}` environment variable `{var}` is not valid unicode")]
    NonUnicodeHeaderEnvironment { header: String, var: String },
}

/// 已校验、已解析的绑定事实（池内只保存这一形态）。
#[derive(Clone)]
pub(crate) enum ResolvedHostWorkspace {
    Remote(ResolvedWorkspaceDescriptor),
    Absent,
}

impl fmt::Debug for ResolvedHostWorkspace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Remote(descriptor) => f.debug_tuple("Remote").field(descriptor).finish(),
            Self::Absent => f.write_str("Absent"),
        }
    }
}

/// 已解析的远程 workspace descriptor：headers 已是最终值（仍只存在内存里）。
#[derive(Clone)]
pub(crate) struct ResolvedWorkspaceDescriptor {
    pub(crate) name: String,
    pub(crate) url: String,
    pub(crate) headers: HashMap<String, String>,
}

impl ResolvedWorkspaceDescriptor {
    /// 转成池内 server 配置：远端来源身份 + system 启动依赖。
    ///
    /// `system_mcp_tools = None`：远端工具清单只来自 live `tools/list`，
    /// 不在装配期预声明（与 ACP workspace 声明同一语义）。
    pub(crate) fn server_config(&self) -> McpServerConfig {
        McpServerConfig {
            command: None,
            args: None,
            env: None,
            url: Some(self.url.clone()),
            headers: Some(self.headers.clone()),
            oauth: None,
            disabled: None,
            dynamic: false,
            subscriptions: None,
            system_mcp: Some(true),
            system_mcp_tools: None,
            system_mcp_timeout: None,
            source: Some(ConfigSource::WorkspaceRemote),
        }
    }
}

impl fmt::Debug for ResolvedWorkspaceDescriptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut names: Vec<&str> = self.headers.keys().map(String::as_str).collect();
        names.sort_unstable();
        f.debug_struct("WorkspaceDescriptor")
            .field("name", &self.name)
            .field("url", &redacted_url(&self.url))
            .field("header_names", &names)
            .finish()
    }
}

impl WorkspaceDescriptor {
    fn resolve(&self) -> Result<ResolvedWorkspaceDescriptor, HostWorkspaceBindingError> {
        if self.name != HOST_WORKSPACE_SERVER_NAME {
            return Err(HostWorkspaceBindingError::NameNotWorkspace {
                name: self.name.clone(),
            });
        }
        if !(self.url.starts_with("http://") || self.url.starts_with("https://")) {
            return Err(HostWorkspaceBindingError::InvalidUrl);
        }
        let mut headers = HashMap::with_capacity(self.headers.len());
        let mut seen = HashSet::with_capacity(self.headers.len());
        for header in &self.headers {
            if header.name.is_empty() {
                return Err(HostWorkspaceBindingError::EmptyHeaderName);
            }
            if !seen.insert(header.name.to_ascii_lowercase()) {
                return Err(HostWorkspaceBindingError::DuplicateHeader {
                    name: header.name.clone(),
                });
            }
            let value = match &header.value {
                HeaderValueSource::Memory(value) => value.clone(),
                HeaderValueSource::Env { var } => match std::env::var(var) {
                    Ok(value) => value,
                    Err(std::env::VarError::NotPresent) => {
                        return Err(HostWorkspaceBindingError::MissingHeaderEnvironment {
                            header: header.name.clone(),
                            var: var.clone(),
                        });
                    }
                    Err(std::env::VarError::NotUnicode(_)) => {
                        return Err(HostWorkspaceBindingError::NonUnicodeHeaderEnvironment {
                            header: header.name.clone(),
                            var: var.clone(),
                        });
                    }
                },
            };
            headers.insert(header.name.clone(), value);
        }
        Ok(ResolvedWorkspaceDescriptor {
            name: self.name.clone(),
            url: self.url.clone(),
            headers,
        })
    }
}

/// Debug/日志用的 URL 投影：只保留 scheme + host + port。
///
/// 远端 URL 可能把凭据放在 query/fragment 里，日志与错误消息不应带出它们。
fn redacted_url(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(parsed) => {
            let mut projected = format!("{}://", parsed.scheme());
            if let Some(host) = parsed.host_str() {
                projected.push_str(host);
            }
            if let Some(port) = parsed.port() {
                projected.push(':');
                projected.push_str(&port.to_string());
            }
            projected.push_str("/***");
            projected
        }
        Err(_) => "<invalid-url>".to_owned(),
    }
}

#[cfg(test)]
#[path = "host_workspace_binding_test.rs"]
mod host_workspace_binding_tests;
