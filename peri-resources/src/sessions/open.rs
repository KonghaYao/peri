//! 会话存储定位与打开请求（D：配置与装配）。
//!
//! 打开入口表达「会话存储在哪里、如何访问」，而不是「SQLite 文件在哪里」：
//!
//! - `--db-path` 归一出的已确认本机路径不进入 locator 解析：不解 `env:`、不判 URL、
//!   不经字符串往返，非 UTF-8 字节原样交给本机装配；
//! - `--session-store` 原文里的本机路径保留 Windows drive/UNC 语义，`C:` 不会被当成
//!   URI scheme；
//! - `env:<变量名>` 只解引用一次、不递归，用于把 URL 之类的值放在环境变量里；
//! - 远程 locator 的引擎只能由已确认语法（`turso://` / `libsql://`）或显式选择给出，
//!   `https://` 单独出现时明确要求显式选择，不按 scheme、端口或响应猜；
//! - 凭证只经 [`CredentialSource`]（环境变量名）表达，本模块不读 `.env`、不搜索 cwd
//!   或父目录、不内置默认名或别名；远程模式缺少凭证来源直接报配置错误；
//! - [`AccessIntent`] 只表达「打算怎么用」，只接受确认过的拼写；参数存在不等于拿到写权限，
//!   能不能写由打开结果回答；
//! - 只有 `env:` 形式的 locator 会读进程环境：环境里存在某个云 URL 变量**不会**切换后端，
//!   也不会把默认本机库换成远程库；解析 locator 不等价于读取凭证值。
//!
//! 本模块是纯解析与校验：不做 I/O、不连接、不创建目录或文件，也不宣告连接成功。

use std::fmt;
use std::path::PathBuf;

use peri_acp_types::session_resources::AccessMode;
use peri_acp_types::session_store::{SessionStoreDeployment, SessionStoreLocator};

use super::remote::{
    CredentialError, CredentialSource, EndpointError, RemoteEndpoint, RemoteEngine,
};

/// locator 解析/校验失败。只携带**变量名**与稳定分类，不携带 locator 原文或凭证值。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LocatorError {
    Empty,
    /// The local SQLite backend is not part of the WASM build.
    #[cfg(target_os = "emscripten")]
    LocalStoreUnsupported,
    #[cfg(target_os = "emscripten")]
    ExecutionUnsupported,
    /// `env:` 后面的变量名不合法。
    InvalidEnvReference,
    EnvValueMissing {
        name: String,
    },
    EnvValueEmpty {
        name: String,
    },
    EnvValueNotUnicode {
        name: String,
    },
    /// `env:` 指向的值又是一个 `env:` 引用：只解引用一次，不递归。
    EnvValueIsReference {
        name: String,
    },
    Remote(EndpointError),
    /// 显式引擎名不是已知取值（`turso` / `libsql`）。
    UnknownEngine,
    /// 远程 locator 但没有给出凭证来源。
    MissingCredentialSource,
    /// 本机 locator 却给了凭证来源：不静默忽略配置。
    CredentialSourceForLocalStore,
    /// 本机 locator 却给了引擎名：不静默忽略配置。
    EngineForLocalStore,
}

impl fmt::Display for LocatorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("session store locator is empty"),
            #[cfg(target_os = "emscripten")]
            Self::LocalStoreUnsupported => formatter.write_str("local session store is unavailable on WASM"),
            #[cfg(target_os = "emscripten")]
            Self::ExecutionUnsupported => formatter.write_str("WASM session execution host is unavailable"),
            Self::InvalidEnvReference => {
                formatter.write_str("session store env reference is not a valid variable name")
            }
            Self::EnvValueMissing { name } => write!(
                formatter,
                "session store env variable {name} is not set; the locator is not defined"
            ),
            Self::EnvValueEmpty { name } => {
                write!(formatter, "session store env variable {name} is empty")
            }
            Self::EnvValueNotUnicode { name } => write!(
                formatter,
                "session store env variable {name} is not valid unicode"
            ),
            Self::EnvValueIsReference { name } => write!(
                formatter,
                "session store env variable {name} is itself an env reference; only one level is resolved"
            ),
            Self::Remote(error) => write!(formatter, "{error}"),
            Self::UnknownEngine => formatter.write_str(
                "unknown session store engine; expected one of the confirmed engine names",
            ),
            Self::MissingCredentialSource => formatter.write_str(
                "remote session store requires an explicit credential source; no default is assumed",
            ),
            Self::CredentialSourceForLocalStore => formatter.write_str(
                "credential source was given for a local session store; it is not silently ignored",
            ),
            Self::EngineForLocalStore => formatter.write_str(
                "an engine was named for a local session store; it is not silently ignored",
            ),
        }
    }
}

impl std::error::Error for LocatorError {}

impl From<EndpointError> for LocatorError {
    fn from(error: EndpointError) -> Self {
        Self::Remote(error)
    }
}

impl From<CredentialError> for LocatorError {
    fn from(error: CredentialError) -> Self {
        match error {
            CredentialError::InvalidName => Self::InvalidEnvReference,
            CredentialError::Missing { name } => Self::EnvValueMissing { name },
            CredentialError::Empty { name } => Self::EnvValueEmpty { name },
            CredentialError::NotUnicode { name } => Self::EnvValueNotUnicode { name },
            CredentialError::EmptyValue => Self::MissingCredentialSource,
        }
    }
}

/// 访问意图：部署参数层的纯类型，只接受确认过的两条拼写。
///
/// 解析成功不等于拥有写权限：真正可写由打开结果（访问模式与数据能力）回答，本类型不赋权。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AccessIntent {
    ReadWrite,
    ReadOnly,
}

impl AccessIntent {
    /// 部署参数层携带的访问模式直接映射（[`SessionStoreDeployment`] 侧无拼写输入，
    /// 因此没有可失败的解析）。
    pub(crate) fn from_access_mode(access: AccessMode) -> Self {
        match access {
            AccessMode::ReadWrite => Self::ReadWrite,
            AccessMode::ReadOnly => Self::ReadOnly,
        }
    }

    /// 交给门面的访问模式（`Self::access` 的取值面）。
    pub(crate) fn mode(self) -> AccessMode {
        match self {
            Self::ReadWrite => AccessMode::ReadWrite,
            Self::ReadOnly => AccessMode::ReadOnly,
        }
    }

    pub(crate) fn is_read_only(self) -> bool {
        matches!(self, Self::ReadOnly)
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ReadWrite => "read-write",
            Self::ReadOnly => "read-only",
        }
    }
}

impl fmt::Display for AccessIntent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 打开请求里的 locator：延迟解析，直到真正打开时才读环境变量。
///
/// `Debug` 手写：远程 locator 原文含主机与库名，解析前也不进日志。
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum StorageLocator {
    /// 未指定：使用默认本机库（`~/.peri/threads/threads.db`）。
    Default,
    /// 已解析的本机路径（`--db-path` 等既有入口），不做 URL 重新解释。
    LocalPath(PathBuf),
    /// locator 原文：本机路径或远程 URL，由 [`looks_like_remote_url`] 分流。
    Literal(String),
    /// `env:<变量名>` 间接定位。
    EnvVar(String),
}

impl StorageLocator {
    /// 纯词法判断：`env:` 前缀按间接定位处理，其余保持原文。
    pub(crate) fn parse(raw: &str) -> Result<Self, LocatorError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(LocatorError::Empty);
        }
        match trimmed.strip_prefix("env:") {
            Some(name) => {
                // 变量名用与凭证来源相同的规则校验，不另立一套。
                CredentialSource::env(name.trim())?;
                Ok(Self::EnvVar(name.trim().to_owned()))
            }
            None => Ok(Self::Literal(trimmed.to_owned())),
        }
    }
}

impl fmt::Debug for StorageLocator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Default => formatter.write_str("Default"),
            Self::LocalPath(path) => formatter.debug_tuple("LocalPath").field(path).finish(),
            Self::Literal(raw) if looks_like_remote_url(raw) => {
                // 解析后由 `RemoteEndpoint` 的脱敏 `Debug` 给 scheme/引擎/主机家族。
                formatter.write_str("Literal(<remote locator redacted>)")
            }
            Self::Literal(raw) => formatter.debug_tuple("Literal").field(raw).finish(),
            Self::EnvVar(name) => formatter.debug_tuple("EnvVar").field(name).finish(),
        }
    }
}

/// 解析后的会话存储位置。`Default` 由装配层换成默认本机路径（那条路径的解析规则
/// 与既有 `open()` 完全一致，不在本模块复制一份）。
#[derive(Clone, Debug)]
pub(crate) enum ResolvedLocator {
    Default,
    Local(PathBuf),
    Remote(Box<RemoteEndpoint>),
}

/// 打开请求：locator + 凭证来源 + 访问意图。
///
/// `Debug` 可以安全打印：远程端点的 `Debug` 只给 scheme/引擎/主机家族，凭证来源只给
/// 变量名，凭证值从不进入本结构（只在打开时按需解析）。
#[derive(Clone, Debug)]
pub(crate) struct SessionStoreOpenRequest {
    input: StorageLocator,
    engine: Option<RemoteEngine>,
    credential: Option<CredentialSource>,
    intent: AccessIntent,
}

impl SessionStoreOpenRequest {
    /// 本机库打开请求：`Some(path)` 用指定路径，`None` 用默认路径。
    /// 与既有 `--db-path` 语义一致，不带凭证。
    pub(crate) fn local(path: Option<PathBuf>, intent: AccessIntent) -> Self {
        Self {
            input: match path {
                Some(path) => StorageLocator::LocalPath(path),
                None => StorageLocator::Default,
            },
            engine: None,
            credential: None,
            intent,
        }
    }

    /// 按 locator 原文构造（D §3.1 的最小配置面）。
    pub(crate) fn from_locator(
        raw: &str,
        engine: Option<RemoteEngine>,
        credential_env: Option<&str>,
        intent: AccessIntent,
    ) -> Result<Self, LocatorError> {
        let input = StorageLocator::parse(raw)?;
        let credential = match credential_env {
            Some(name) => Some(CredentialSource::env(name)?),
            None => None,
        };
        Ok(Self {
            input,
            engine,
            credential,
            intent,
        })
    }

    /// 从部署参数构造（D-04 的唯一一次转换）：CLI 定位参数在这里变成 typed request。
    ///
    /// 纯解析，不读环境变量、不连接、不创建文件：locator 形态、引擎名与凭证来源的
    /// 冲突都在这里失败；`env:` 的间接定位仍延迟到 [`Self::resolve_locator`]。
    ///
    /// 两条定位入口按**类型**分派，不按字符串形态猜：
    ///
    /// - [`SessionStoreLocator::LocalPath`]（`--db-path`）是已确认本机路径，`PathBuf`
    ///   原样进入 [`StorageLocator::LocalPath`]：不解 `env:`、不判 URL、不经字符串往返，
    ///   远程专属参数同时给出即在此失败；
    /// - [`SessionStoreLocator::Locator`]（`--session-store`）是原文，按词法解析（`env:`
    ///   才是间接定位）。
    pub(crate) fn from_deployment(
        deployment: &SessionStoreDeployment,
    ) -> Result<Self, LocatorError> {
        let intent = AccessIntent::from_access_mode(deployment.access());
        match deployment.locator() {
            SessionStoreLocator::Default => {
                // 没有 locator 时远程专用参数无处生效：不静默忽略配置。
                reject_remote_only_parameters(deployment)?;
                Ok(Self::local(None, intent))
            }
            SessionStoreLocator::LocalPath(path) => {
                reject_remote_only_parameters(deployment)?;
                Ok(Self {
                    input: StorageLocator::LocalPath(path.clone()),
                    engine: None,
                    credential: None,
                    intent,
                })
            }
            SessionStoreLocator::Locator(raw) => Self::from_locator(
                raw,
                engine_from_name(deployment.engine_name())?,
                deployment.credential_env_name(),
                intent,
            ),
        }
    }

    /// 访问模式视图（等价于 `self.intent().mode()`）：装配层把它交给组合层，
    /// 由组合层决定数据面按只读还是读写打开。
    pub(crate) fn access(&self) -> AccessMode {
        self.intent.mode()
    }

    /// 部署参数层的访问意图（只读选择走独立 seam 的依据）。
    pub(crate) fn intent(&self) -> AccessIntent {
        self.intent
    }

    /// 凭证**来源**（环境变量名，不含值）：远程装配按它取值，值从不进入本结构。
    pub(crate) fn credential_source(&self) -> Option<&CredentialSource> {
        self.credential.as_ref()
    }

    /// 解析最终 location；同时校验凭证来源与 locator 形态是否匹配。
    ///
    /// 这是唯一读取环境变量的地方（`env:` 间接定位），只解引用一次。
    pub(crate) fn resolve_locator(&self) -> Result<ResolvedLocator, LocatorError> {
        let resolved = match &self.input {
            StorageLocator::Default => ResolvedLocator::Default,
            StorageLocator::LocalPath(path) => ResolvedLocator::Local(path.clone()),
            StorageLocator::Literal(raw) => {
                if looks_like_remote_url(raw) {
                    ResolvedLocator::Remote(Box::new(RemoteEndpoint::parse(raw, self.engine)?))
                } else {
                    ResolvedLocator::Local(PathBuf::from(raw))
                }
            }
            StorageLocator::EnvVar(name) => {
                let value = match std::env::var(name) {
                    Ok(value) => value,
                    Err(std::env::VarError::NotPresent) => {
                        return Err(LocatorError::EnvValueMissing { name: name.clone() })
                    }
                    Err(std::env::VarError::NotUnicode(_)) => {
                        return Err(LocatorError::EnvValueNotUnicode { name: name.clone() })
                    }
                };
                let value = value.trim().to_owned();
                if value.is_empty() {
                    return Err(LocatorError::EnvValueEmpty { name: name.clone() });
                }
                if value.starts_with("env:") {
                    return Err(LocatorError::EnvValueIsReference { name: name.clone() });
                }
                if looks_like_remote_url(&value) {
                    ResolvedLocator::Remote(Box::new(RemoteEndpoint::parse(&value, self.engine)?))
                } else {
                    ResolvedLocator::Local(PathBuf::from(value))
                }
            }
        };
        match (&resolved, &self.credential, &self.engine) {
            (ResolvedLocator::Remote(_), None, _) => Err(LocatorError::MissingCredentialSource),
            (ResolvedLocator::Default | ResolvedLocator::Local(_), Some(_), _) => {
                Err(LocatorError::CredentialSourceForLocalStore)
            }
            (ResolvedLocator::Default | ResolvedLocator::Local(_), None, Some(_)) => {
                Err(LocatorError::EngineForLocalStore)
            }
            _ => Ok(resolved),
        }
    }
}

/// 已确认的本机定位（默认库或 `--db-path` 路径）不接受远程专用参数：给出即配置错误，
/// 不静默忽略，也不因为「可能以后用得上」而保留。
fn reject_remote_only_parameters(deployment: &SessionStoreDeployment) -> Result<(), LocatorError> {
    if deployment.engine_name().is_some() {
        return Err(LocatorError::EngineForLocalStore);
    }
    if deployment.credential_env_name().is_some() {
        return Err(LocatorError::CredentialSourceForLocalStore);
    }
    Ok(())
}

/// 显式引擎名 → 已确认引擎；未知取值按配置错误处理，不猜也不轮流试。
pub(crate) fn engine_from_name(name: Option<&str>) -> Result<Option<RemoteEngine>, LocatorError> {
    match name {
        Some(name) => match RemoteEngine::parse(name) {
            Some(engine) => Ok(Some(engine)),
            None => Err(LocatorError::UnknownEngine),
        },
        None => Ok(None),
    }
}

/// 词法上是否像一个远程 URL：scheme 为 ASCII 字母数字且长度 ≥ 2。
/// 单字符 scheme 一律当本机路径，避免把 Windows drive（`C:`）读成 URI scheme；
/// UNC 路径（`\\server\share`）不含 `://`，同样落回路径分支。
fn looks_like_remote_url(raw: &str) -> bool {
    let Some((scheme, _)) = raw.split_once("://") else {
        return false;
    };
    scheme.len() >= 2 && scheme.chars().all(|c| c.is_ascii_alphanumeric())
}
