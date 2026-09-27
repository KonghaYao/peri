//! 远程凭证：来源与值分离，只接受显式注入。
//!
//! 规则（D §4）：资源库不搜索 cwd 或父目录 `.env`，不内置默认变量名、别名或候选名；
//! 变量名由调用方显式给出，缺失/空值各有类型化错误。凭证值不实现泄密的 `Debug`、
//! 不实现 `Serialize`/`Deserialize`，只在 SDK 调用边界 [`SessionStoreCredential::expose`]。

use std::fmt;

/// 凭证来源（环境变量名）或凭证值本身的问题。只携带**变量名**，不携带值。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CredentialError {
    /// 变量名不是合法的 `[A-Za-z0-9_]+`。
    InvalidName,
    Missing {
        name: String,
    },
    Empty {
        name: String,
    },
    NotUnicode {
        name: String,
    },
    /// 直接注入的凭证为空。
    EmptyValue,
}

impl fmt::Display for CredentialError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName => formatter.write_str("credential variable name is not valid"),
            Self::Missing { name } => {
                write!(
                    formatter,
                    "credential environment variable {name} is not set"
                )
            }
            Self::Empty { name } => {
                write!(formatter, "credential environment variable {name} is empty")
            }
            Self::NotUnicode { name } => write!(
                formatter,
                "credential environment variable {name} is not valid unicode"
            ),
            Self::EmptyValue => formatter.write_str("injected credential is empty"),
        }
    }
}

impl std::error::Error for CredentialError {}

/// 凭证来源：只表达「从哪个环境变量取」。
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CredentialSource {
    name: String,
}

impl CredentialSource {
    pub(crate) fn env(name: impl Into<String>) -> Result<Self, CredentialError> {
        let name = name.into();
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(CredentialError::InvalidName);
        }
        Ok(Self { name })
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// 解析凭证：只读进程环境，缺失、空值与非 Unicode 分别是类型化错误；
    /// 不尝试其他变量名，也不回退到默认值。
    pub(crate) fn resolve(&self) -> Result<SessionStoreCredential, CredentialError> {
        match std::env::var(&self.name) {
            Ok(value) if value.is_empty() => Err(CredentialError::Empty {
                name: self.name.clone(),
            }),
            Ok(value) => Ok(SessionStoreCredential(value)),
            Err(std::env::VarError::NotPresent) => Err(CredentialError::Missing {
                name: self.name.clone(),
            }),
            Err(std::env::VarError::NotUnicode(_)) => Err(CredentialError::NotUnicode {
                name: self.name.clone(),
            }),
        }
    }
}

impl fmt::Debug for CredentialSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("CredentialSource")
            .field(&format_args!("env:{}", self.name))
            .finish()
    }
}

impl fmt::Display for CredentialSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "env:{}", self.name)
    }
}

/// 凭证值：不 `Debug` 泄露、不序列化；`expose` 只给 adapter 边界调用。
pub(crate) struct SessionStoreCredential(String);

impl SessionStoreCredential {
    /// 直接注入（云实验 runner 在受控进程内解析 `.env` 后注入）。
    pub(crate) fn new(value: impl Into<String>) -> Result<Self, CredentialError> {
        let value = value.into();
        if value.is_empty() {
            return Err(CredentialError::EmptyValue);
        }
        Ok(Self(value))
    }

    /// 只给 SDK 调用边界使用；调用方不得把它写进日志、错误或快照。
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }

    /// 复制一份凭证值：**只为让受保护的连接工厂持有**（adapter 生命周期长于一次打开，
    /// 而重建必须由工厂自己完成，不能回头找调用方要凭证）。
    ///
    /// 本类型刻意不实现 `Clone`：复制凭证是一次要写明的决定，不是随手可用的便利；
    /// 复制只发生在进程内，两个副本都不出这条边界。
    pub(crate) fn duplicate(&self) -> Self {
        Self(self.0.clone())
    }

    /// 诊断用的粗粒度长度类（值本身不泄露）。
    pub(crate) fn length_class(&self) -> usize {
        self.0.len() / 16
    }
}

impl fmt::Debug for SessionStoreCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SessionStoreCredential(redacted)")
    }
}
