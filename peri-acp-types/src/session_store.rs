//! 会话存储部署参数 — 跨层传递的**中性**定位描述（D：定位、凭证与部署装配）。
//!
//! 这个类型只承载「用户希望把会话存储放在哪里、用什么访问」，是部署入口（TUI /
//! print / ACP stdio / meta）与资源装配层之间的载体：它**不是**解析结果，也不
//! 建立连接。真正的纯解析（locator 形态、`env:` 解引用、引擎名、凭证来源）与后端
//! 选择只发生在资源装配层，见 `peri-resources` 的 typed open request。
//!
//! 两种定位输入在类型上保持区分（[`SessionStoreLocator`]）：`--db-path` 是**已确认的
//! 本机路径**，`--session-store` 是**待解析的 locator 原文**。路径不经 `String` 往返，
//! 资源层也不会把它当 `env:` 引用或远程 URL 重新解释。
//!
//! 纪律（与资源层一致）：
//!
//! - 只接受凭证**来源**（环境变量名），不接受 token 字面量，不读 `.env`；
//! - `engine` 只在 locator 形态无法唯一决定引擎时给出，资源层不按 scheme/端口猜；
//! - `access` 是入口决定的意图，**不是权限**：能不能写由打开结果回答；
//! - 本类型不含凭证值，`Debug` 也不回显任何 locator 取值（远程 locator 含主机与库名）。

use std::fmt;
use std::path::PathBuf;

use crate::session_resources::AccessMode;

/// 中性定位描述：默认本机库 / 已确认本机路径 / 待解析 locator 原文。
///
/// 三者的区别是**语义**，不是表示形式：
///
/// - [`Self::LocalPath`]：`--db-path` 归一后的已确认本机路径。保留平台路径语义与原始
///   字节（Windows drive/UNC、Unix 非 UTF-8 文件名），资源层原样交给本机装配——既不按
///   `env:` 解引用，也不按 URL 猜后端。因此 `--db-path env:UNSET` 指的是名为
///   `env:UNSET` 的本机文件；
/// - [`Self::Locator`]：`--session-store` 原文，形态未定（本机路径、远程 URL 或
///   `env:<变量名>`），交由资源装配层纯解析。`--session-store env:UNSET` 是解引用环境
///   变量 `UNSET`，与上一条语义不同；
/// - [`Self::Default`]：未指定，由资源层换成默认本机库（`~/.peri/threads/threads.db`）。
#[derive(Clone, PartialEq, Eq)]
pub enum SessionStoreLocator {
    Default,
    LocalPath(PathBuf),
    Locator(String),
}

impl fmt::Debug for SessionStoreLocator {
    /// 只给形态，不给取值：`Locator` 原文可能是含主机与库名的远程 locator，
    /// 本机路径含用户环境。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Default => formatter.write_str("Default"),
            Self::LocalPath(_) => formatter.write_str("LocalPath(<redacted>)"),
            Self::Locator(_) => formatter.write_str("Locator(<redacted>)"),
        }
    }
}

/// 会话存储部署参数。
///
/// 默认值（[`SessionStoreDeployment::default_local`]）＝默认本机库 + 读写意图，
/// 与既有 `--db-path` 缺省行为一致；`--db-path <path>` 归一为
/// [`SessionStoreDeployment::local_path`]，`--session-store <locator>` 归一为
/// [`SessionStoreDeployment::from_locator`]，两者与 `--session-store` 互斥。
#[derive(Clone, PartialEq, Eq)]
pub struct SessionStoreDeployment {
    locator: SessionStoreLocator,
    engine: Option<String>,
    credential_env: Option<String>,
    access: AccessMode,
}

impl SessionStoreDeployment {
    /// 默认本机库（`~/.peri/threads/threads.db`）、读写意图。
    pub fn default_local() -> Self {
        Self {
            locator: SessionStoreLocator::Default,
            engine: None,
            credential_env: None,
            access: AccessMode::ReadWrite,
        }
    }

    /// 既有 `--db-path` 归一：已确认本机路径，语义与迁移前一致。
    ///
    /// 路径按 `PathBuf` 原样保留，不转成字符串：`env:` 之类的**文件名**不会被当成
    /// 环境变量引用，非 UTF-8 字节也不会在往返中损坏。
    pub fn local_path(path: PathBuf) -> Self {
        Self {
            locator: SessionStoreLocator::LocalPath(path),
            engine: None,
            credential_env: None,
            access: AccessMode::ReadWrite,
        }
    }

    /// `--session-store <locator>` 原文（本机路径、远程 URL 或 `env:<变量名>`）。
    ///
    /// 原文形态未定，由资源装配层解析；本构造不做任何形态判断。
    pub fn from_locator(raw: impl Into<String>) -> Self {
        Self {
            locator: SessionStoreLocator::Locator(raw.into()),
            engine: None,
            credential_env: None,
            access: AccessMode::ReadWrite,
        }
    }

    /// `--session-store-engine <名>`：只在 locator 形态无法唯一决定引擎时需要。
    pub fn with_engine(mut self, engine: impl Into<String>) -> Self {
        self.engine = Some(engine.into());
        self
    }

    /// `--session-store-token-env <变量名>`：凭证来源，不接受凭证值。
    pub fn with_credential_env(mut self, name: impl Into<String>) -> Self {
        self.credential_env = Some(name.into());
        self
    }

    /// 访问意图（入口决定；只读入口不得给出读写意图）。
    pub fn with_access(mut self, access: AccessMode) -> Self {
        self.access = access;
        self
    }

    /// 定位描述：默认本机库 / 已确认本机路径 / 待解析 locator 原文。
    pub fn locator(&self) -> &SessionStoreLocator {
        &self.locator
    }

    /// 显式引擎名（尚未校验；校验归资源层）。
    pub fn engine_name(&self) -> Option<&str> {
        self.engine.as_deref()
    }

    /// 凭证来源变量名（尚未校验；值从不进入本结构）。
    pub fn credential_env_name(&self) -> Option<&str> {
        self.credential_env.as_deref()
    }

    /// 入口声明的访问意图。
    pub fn access(&self) -> AccessMode {
        self.access
    }
}

impl Default for SessionStoreDeployment {
    fn default() -> Self {
        Self::default_local()
    }
}

impl fmt::Debug for SessionStoreDeployment {
    /// 故意不打印 locator 取值与凭证变量名之外的内容：诊断只需要「形态 + 是否配置」。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionStoreDeployment")
            .field(
                "locator",
                &match self.locator {
                    SessionStoreLocator::Default => "<default-local>",
                    SessionStoreLocator::LocalPath(_) => "<local-path>",
                    SessionStoreLocator::Locator(_) => "<configured>",
                },
            )
            .field("engine_configured", &self.engine.is_some())
            .field("credential_env_configured", &self.credential_env.is_some())
            .field("access", &self.access)
            .finish()
    }
}

#[cfg(test)]
#[path = "session_store_test.rs"]
mod tests;
