//! 远程端点解析与稳定身份。
//!
//! 引擎只能来自**已确认的 locator 语法或显式选择**，不由端口、响应或「哪个 SDK 好使」推断：
//! 官方 Rust Quickstart 与 SQL over HTTP 参考页把 `turso://` 记为 Turso 数据库、
//! `libsql://` 记为 libSQL 数据库；两者都能换成 `https://` 访问同一服务面，所以
//! `https://`/`http://` 单独出现时引擎不可判定，必须显式给出。

use std::fmt;

use sha2::{Digest, Sha256};
use url::Url;

/// 远程引擎（含驱动对应关系，见模块文档）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RemoteEngine {
    /// Turso 数据库引擎（驱动 `turso_serverless`）。
    Turso,
    /// libSQL 引擎（驱动 `libsql` 的 remote feature）。
    LibSql,
}

impl RemoteEngine {
    /// 本机登记与 CLI 显式选择使用的稳定名字。
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Turso => "turso",
            Self::LibSql => "libsql",
        }
    }

    /// 解析显式引擎选择；不认识的取值返回 `None`，由调用方报类型化错误。
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "turso" => Some(Self::Turso),
            "libsql" => Some(Self::LibSql),
            _ => None,
        }
    }
}

/// locator 解析失败的原因。**不携带 locator 原文**（可能含主机名或凭证）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EndpointError {
    Empty,
    /// 没有 scheme：看起来是本机路径，不属于远程 locator。
    NotARemoteUrl,
    UnsupportedScheme,
    /// URL 里带 userinfo：凭证只经显式来源注入。
    UserInfoPresent,
    /// 带 query/fragment：embedded replica / sync 形态不在首期支持面内，不做静默解释。
    QueryOrFragmentUnsupported,
    /// `https://` 等无法判定引擎的形态，且未显式选择引擎。
    AmbiguousEngine,
    /// 显式引擎与 locator scheme 指向不同引擎。
    EngineConflict,
    UnparsableUrl,
}

impl fmt::Display for EndpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Empty => "session store locator is empty",
            Self::NotARemoteUrl => "session store locator is not a remote URL",
            Self::UnsupportedScheme => "session store locator scheme is unsupported",
            Self::UserInfoPresent => "session store URL must not carry credentials",
            Self::QueryOrFragmentUnsupported => {
                "session store URL must not carry query or fragment (embedded replica and sync are unsupported)"
            }
            Self::AmbiguousEngine => {
                "remote engine cannot be derived from this URL; select the engine explicitly"
            }
            Self::EngineConflict => "selected engine conflicts with the locator scheme",
            Self::UnparsableUrl => "session store URL cannot be parsed",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for EndpointError {}

/// 已确认的远程端点：URL + 引擎，Debug 保留实际地址。
#[derive(Clone)]
pub(crate) struct RemoteEndpoint {
    url: Url,
    engine: RemoteEngine,
}

impl RemoteEndpoint {
    /// 解析远程 locator。`explicit_engine` 来自显式配置；为 `None` 时只有
    /// `turso://` / `libsql://` 能确定引擎。
    pub(crate) fn parse(
        raw: &str,
        explicit_engine: Option<RemoteEngine>,
    ) -> Result<Self, EndpointError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(EndpointError::Empty);
        }
        let Some((scheme, _)) = trimmed.split_once("://") else {
            return Err(EndpointError::NotARemoteUrl);
        };
        let scheme = scheme.to_ascii_lowercase();
        let scheme_engine = match scheme.as_str() {
            "turso" => Some(RemoteEngine::Turso),
            "libsql" => Some(RemoteEngine::LibSql),
            "https" | "http" => None,
            _ => return Err(EndpointError::UnsupportedScheme),
        };
        let url = Url::parse(trimmed).map_err(|_| EndpointError::UnparsableUrl)?;
        if !url.username().is_empty() || url.password().is_some() {
            return Err(EndpointError::UserInfoPresent);
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(EndpointError::QueryOrFragmentUnsupported);
        }
        if url.host_str().is_none() {
            return Err(EndpointError::UnparsableUrl);
        }
        let engine = match (explicit_engine, scheme_engine) {
            (Some(explicit), Some(derived)) if explicit != derived => {
                return Err(EndpointError::EngineConflict);
            }
            (Some(explicit), _) => explicit,
            (None, Some(derived)) => derived,
            (None, None) => return Err(EndpointError::AmbiguousEngine),
        };
        Ok(Self { url, engine })
    }

    pub(crate) fn engine(&self) -> RemoteEngine {
        self.engine
    }

    /// 交给 SDK 的 URL 原文（不含 userinfo/query，已在解析期拒绝）。
    pub(crate) fn sdk_url(&self) -> &str {
        self.url.as_str()
    }

    /// 稳定别名归一：scheme 不参与身份（同一存储的 `turso://` 与 `https://` 形式必须同身份），
    /// host 小写，路径去尾斜杠。
    pub(crate) fn canonical_locator(&self) -> String {
        let host = self.url.host_str().unwrap_or_default().to_ascii_lowercase();
        let port = self.url.port().map(|p| format!(":{p}")).unwrap_or_default();
        let path = self.url.path().trim_end_matches('/');
        format!("https://{host}{port}{path}")
    }

    /// locator 的稳定摘要：按 [`Self::canonical_locator`] 归一后的 SHA-256。
    ///
    /// 它描述的是 locator 身份本身，与登记机制无关——v10 撤销本机登记表后生产路径已无消费者
    /// （原用途是按 store 区分本机登记行），当前只有测试在做「同一 locator 的不同写法摘要
    /// 相同」的比对；是否随该测试一并删除归「统一 schema」段决定。
    pub(crate) fn locator_digest(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.canonical_locator().as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// 诊断用主机家族，不含主机名（主机名含数据库与组织标识）。
    pub(crate) fn host_class(&self) -> &'static str {
        let host = self.url.host_str().unwrap_or_default();
        if host.ends_with(".turso.io") || host == "turso.io" {
            "official_turso_cloud_domain"
        } else if host == "localhost" || host.starts_with("127.") {
            "loopback"
        } else {
            "other_domain"
        }
    }
}

impl fmt::Debug for RemoteEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteEndpoint")
            .field("scheme", &self.url.scheme())
            .field("engine", &self.engine)
            .field("url", &self.url)
            .field("host_class", &self.host_class())
            .finish()
    }
}
