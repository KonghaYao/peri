//! 启动配置与校验（WP-P1 起为单进程形态）。
//!
//! 这个模块只承载**运行参数**，分三类：
//!
//! 1. 传输与暴露面（stdio/HTTP、bind、Host/Origin、body 上限）。
//! 2. 授权来源（**只记录来源**：env 变量名或文件路径，绝不保存 token 值，
//!    因此 token 不可能出现在 `Debug`/日志/`--help`/命令行里）。
//! 3. 工作区根（文件类工具的能力边界）与 Bash 任务的保留策略。
//!
//! 明确不属于配置的内容：
//! - 工具语义常量（Read 2000 行、Bash 65000 字节/2000 行、Grep 250 行等）是迁移
//!   对等性要求，**不可**由运维参数改变。
//! - 工作区根是**能力边界而不是安全边界**：它约束文件类工具的路径解析，但
//!   `Bash` 以当前用户权限在本机执行、命令不额外限制——本产品不提供隔离保证，
//!   因此这里没有"隔离开关"这类可配项。

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::ConfigError;
use crate::wire::DEFAULT_MAX_REQUEST_BODY_BYTES;

/// 配置环境变量前缀。
pub const ENV_PREFIX: &str = "LOCAL_MCP_";

/// server 传输类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    /// stdio：stdout 只写 NDJSON-RPC，日志走 stderr。
    Stdio,
    /// Streamable HTTP：默认只绑回环。
    Http,
}

/// HTTP 暴露面配置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HttpConfig {
    /// 监听地址；默认 `127.0.0.1:0`（端口 0 = 由内核分配）。
    pub bind: SocketAddr,
    /// 允许的 `Host` 值（host 或 host:port）；空 = 只允许回环。
    pub allowed_hosts: Vec<String>,
    /// 允许的 `Origin` 值（含 scheme）；空 = 不校验 Origin（缺失 Origin 一律放行）。
    pub allowed_origins: Vec<String>,
    /// POST body 上限。
    pub max_request_body_bytes: usize,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([127, 0, 0, 1], 0)),
            allowed_hosts: Vec::new(),
            allowed_origins: Vec::new(),
            max_request_body_bytes: DEFAULT_MAX_REQUEST_BODY_BYTES,
        }
    }
}

/// 授权 token 的**来源**；本类型不持有任何 secret 值。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum TokenSource {
    /// 从环境变量读取（值在认证层读取，不进入配置结构）。
    Env {
        /// 环境变量名。
        var: String,
    },
    /// 从文件读取（值在认证层读取，不进入配置结构）。
    File {
        /// 文件路径（路径本身不是 secret）。
        path: PathBuf,
    },
}

/// 授权配置。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthConfig {
    /// token 来源；`None` 表示无鉴权（仅允许回环）。
    pub token: Option<TokenSource>,
}

impl AuthConfig {
    /// 是否配置了 token 来源。
    pub fn is_configured(&self) -> bool {
        self.token.is_some()
    }
}

/// 工作区配置：文件类工具的能力边界，同时是 `Bash` 的 cwd。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    /// 工作区根（宿主路径）。
    pub root: PathBuf,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        }
    }
}

/// Bash 任务保留策略（WP-007 起**全部字段都有真实消费者**）。
///
/// 刻意不放进来的东西：TERM→KILL 的升级窗口。它是源实现 `kill_process_group_escalating`
/// 的 2s 常量（`tasks::bash::KILL_ESCALATION`）——语义常量不由运维参数改变，
/// 因此这里既没有"看起来可配但没人读"的字段，也没有被悄悄削弱的升级语义。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskConfig {
    /// 终态任务保留时长（秒）；消费方 = 任务注册表的回收器。
    pub ttl_secs: u64,
    /// 最多保留的终态任务条数；消费方 = 任务注册表的回收器。
    pub retention: usize,
    /// 同时运行的后台 shell 任务上限；默认值即源实现 `SHELL_LIMIT = 5`。
    ///
    /// 它是运行期容量旋钮，不是工具语义常量：调大它只改变并发容量，
    /// 不改变任何工具的输入、输出或错误文案。
    pub max_concurrent_shell_tasks: usize,
}

impl Default for TaskConfig {
    fn default() -> Self {
        Self {
            ttl_secs: 3600,
            retention: 100,
            // 源 `peri-agent/src/agent/async_tasks/registry.rs::SHELL_LIMIT`。
            max_concurrent_shell_tasks: 5,
        }
    }
}

/// server 全量配置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// 传输类型。
    pub transport: TransportKind,
    /// HTTP 配置（stdio 下仍参与校验，便于同进程切换）。
    pub http: HttpConfig,
    /// 授权来源。
    pub auth: AuthConfig,
    /// 工作区根（文件类工具的能力边界与 `Bash` 的 cwd）。
    pub workspace: WorkspaceConfig,
    /// 任务保留策略。
    pub tasks: TaskConfig,
    /// 允许非回环绑定；默认 `false`（fail closed）。
    pub allow_non_loopback: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            transport: TransportKind::Stdio,
            http: HttpConfig::default(),
            auth: AuthConfig::default(),
            workspace: WorkspaceConfig::default(),
            tasks: TaskConfig::default(),
            allow_non_loopback: false,
        }
    }
}

impl Config {
    /// 校验配置；失败必须以 [`crate::error::exit_code::USAGE`] 退出。
    ///
    /// fail closed 规则：
    /// - 非回环绑定需要同时满足 `allow_non_loopback` 与已配置 token 来源；
    /// - body 上限必须为正且不超过 64 MiB；
    /// - 工作区根必须是已存在目录；
    /// - 任务保留策略必须为正。
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !self.workspace.root.is_dir() {
            return Err(ConfigError::WorkspaceNotDirectory);
        }
        if self.http.max_request_body_bytes == 0 {
            return Err(ConfigError::InvalidValue {
                field: "http.max_request_body_bytes",
                reason: "must be greater than zero".to_string(),
            });
        }
        if self.http.max_request_body_bytes > 64 * 1024 * 1024 {
            return Err(ConfigError::InvalidValue {
                field: "http.max_request_body_bytes",
                reason: "must not exceed 64 MiB".to_string(),
            });
        }
        if !self.bind_is_loopback() {
            if !self.allow_non_loopback {
                return Err(ConfigError::InvalidValue {
                    field: "http.bind",
                    reason: "non-loopback bind requires --allow-non-loopback".to_string(),
                });
            }
            if !self.auth.is_configured() {
                return Err(ConfigError::TokenRequiredForNonLoopback);
            }
        }
        if self.tasks.max_concurrent_shell_tasks == 0 {
            return Err(ConfigError::InvalidValue {
                field: "tasks.max_concurrent_shell_tasks",
                reason: "must be greater than zero".to_string(),
            });
        }
        if self.tasks.retention == 0 {
            return Err(ConfigError::InvalidValue {
                field: "tasks.retention",
                reason: "must be greater than zero".to_string(),
            });
        }
        if self.tasks.ttl_secs == 0 {
            return Err(ConfigError::InvalidValue {
                field: "tasks.ttl_secs",
                reason: "must be greater than zero".to_string(),
            });
        }
        Ok(())
    }

    /// bind 地址是否为回环（`127.0.0.0/8` 或 `::1`）。
    pub fn bind_is_loopback(&self) -> bool {
        match self.http.bind.ip() {
            IpAddr::V4(v4) => v4.is_loopback(),
            IpAddr::V6(v6) => v6.is_loopback(),
        }
    }
}

pub use cli::ServerCli;

mod cli {
    use std::net::SocketAddr;
    use std::path::PathBuf;

    use clap::Parser;

    use super::{
        AuthConfig, Config, HttpConfig, TaskConfig, TokenSource, TransportKind, WorkspaceConfig,
    };

    /// 命令行。
    ///
    /// 注意：这里**没有** `--token` 参数。token 只能通过 `--token-env` 指向的环境变量
    /// 或 `--token-file` 指向的文件提供，避免出现在命令行与进程列表里。
    #[derive(Debug, Clone, Parser)]
    #[command(
        name = "local-mcp-server",
        about = "独立本机 MCP server（单进程直接在本机执行 Peri 七工具）",
        version
    )]
    pub struct ServerCli {
        /// 传输类型。
        #[arg(long, value_enum, default_value_t = TransportArg::Stdio)]
        pub transport: TransportArg,
        /// HTTP 监听地址（默认回环随机端口）。
        #[arg(long, default_value = "127.0.0.1:0")]
        pub bind: SocketAddr,
        /// 允许的 Host（可重复；留空表示只允许回环）。
        #[arg(long = "allowed-host")]
        pub allowed_hosts: Vec<String>,
        /// 允许的 Origin（可重复；需含 scheme）。
        #[arg(long = "allowed-origin")]
        pub allowed_origins: Vec<String>,
        /// POST body 上限（字节）。
        #[arg(long, default_value_t = super::DEFAULT_MAX_REQUEST_BODY_BYTES)]
        pub max_body_bytes: usize,
        /// 从该环境变量读取 bearer token。
        #[arg(long)]
        pub token_env: Option<String>,
        /// 从该文件读取 bearer token。
        #[arg(long)]
        pub token_file: Option<PathBuf>,
        /// 允许非回环绑定（必须同时配置 token 来源）。
        #[arg(long, default_value_t = false)]
        pub allow_non_loopback: bool,
        /// 工作区根（文件类工具的能力边界，也是 `Bash` 的 cwd）。
        #[arg(long)]
        pub workspace: Option<PathBuf>,
        /// 终态任务日志保留秒数。
        #[arg(long, default_value_t = 3600)]
        pub task_ttl_secs: u64,
        /// 终态任务最大保留条数。
        #[arg(long, default_value_t = 100)]
        pub task_retention: usize,
    }

    /// clap 用的传输枚举。
    #[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
    pub enum TransportArg {
        /// stdio。
        Stdio,
        /// Streamable HTTP。
        Http,
    }

    impl From<TransportArg> for TransportKind {
        fn from(value: TransportArg) -> Self {
            match value {
                TransportArg::Stdio => TransportKind::Stdio,
                TransportArg::Http => TransportKind::Http,
            }
        }
    }

    impl ServerCli {
        /// 转成经过校验的 [`Config`]。
        pub fn into_config(self) -> Result<Config, crate::error::ConfigError> {
            if self.token_env.is_some() && self.token_file.is_some() {
                return Err(crate::error::ConfigError::InvalidValue {
                    field: "auth.token",
                    reason: "token_env 与 token_file 只能二选一".to_string(),
                });
            }
            let token = match (self.token_env, self.token_file) {
                (Some(var), None) => Some(TokenSource::Env { var }),
                (None, Some(path)) => Some(TokenSource::File { path }),
                _ => None,
            };

            let config = Config {
                transport: self.transport.into(),
                http: HttpConfig {
                    bind: self.bind,
                    allowed_hosts: self.allowed_hosts,
                    allowed_origins: self.allowed_origins,
                    max_request_body_bytes: self.max_body_bytes,
                },
                auth: AuthConfig { token },
                workspace: match self.workspace {
                    Some(root) => WorkspaceConfig { root },
                    None => WorkspaceConfig::default(),
                },
                tasks: TaskConfig {
                    ttl_secs: self.task_ttl_secs,
                    retention: self.task_retention,
                    ..TaskConfig::default()
                },
                allow_non_loopback: self.allow_non_loopback,
            };
            config.validate()?;
            Ok(config)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loopback_config() -> Config {
        Config {
            workspace: WorkspaceConfig {
                root: std::env::temp_dir(),
            },
            ..Config::default()
        }
    }

    #[test]
    fn default_config_is_stdio_loopback_without_token() {
        let config = loopback_config();
        assert_eq!(config.transport, TransportKind::Stdio);
        assert!(config.bind_is_loopback());
        assert!(!config.auth.is_configured());
        config.validate().expect("默认配置必须自洽");
    }

    #[test]
    fn non_loopback_requires_explicit_flag() {
        let config = Config {
            http: HttpConfig {
                bind: "0.0.0.0:8080".parse().unwrap(),
                ..HttpConfig::default()
            },
            ..loopback_config()
        };
        assert!(matches!(
            config.validate(),
            Err(ConfigError::InvalidValue {
                field: "http.bind",
                ..
            })
        ));
    }

    #[test]
    fn non_loopback_with_flag_still_requires_token_source() {
        let config = Config {
            http: HttpConfig {
                bind: "0.0.0.0:8080".parse().unwrap(),
                ..HttpConfig::default()
            },
            allow_non_loopback: true,
            ..loopback_config()
        };
        assert_eq!(
            config.validate(),
            Err(ConfigError::TokenRequiredForNonLoopback)
        );
    }

    #[test]
    fn non_loopback_with_token_source_is_accepted() {
        let config = Config {
            http: HttpConfig {
                bind: "0.0.0.0:8080".parse().unwrap(),
                ..HttpConfig::default()
            },
            auth: AuthConfig {
                token: Some(TokenSource::Env {
                    var: "LOCAL_MCP_TOKEN".to_string(),
                }),
            },
            allow_non_loopback: true,
            ..loopback_config()
        };
        config.validate().expect("回环 + token 来源应通过校验");
    }

    #[test]
    fn body_limit_bounds_are_enforced() {
        let zero = Config {
            http: HttpConfig {
                max_request_body_bytes: 0,
                ..HttpConfig::default()
            },
            ..loopback_config()
        };
        assert!(zero.validate().is_err());

        let huge = Config {
            http: HttpConfig {
                max_request_body_bytes: 128 * 1024 * 1024,
                ..HttpConfig::default()
            },
            ..loopback_config()
        };
        assert!(huge.validate().is_err());
    }

    #[test]
    fn missing_workspace_directory_fails_closed() {
        let config = Config {
            workspace: WorkspaceConfig {
                root: PathBuf::from("/definitely/not/here/for/local-mcp"),
            },
            ..Config::default()
        };
        assert_eq!(config.validate(), Err(ConfigError::WorkspaceNotDirectory));
    }

    #[test]
    fn token_source_records_only_location_not_value() {
        let config = Config {
            auth: AuthConfig {
                token: Some(TokenSource::Env {
                    var: "LOCAL_MCP_TOKEN".to_string(),
                }),
            },
            ..loopback_config()
        };
        let rendered = format!("{config:?}");
        assert!(rendered.contains("LOCAL_MCP_TOKEN"));
        // 结构上就没有可存放 token 值的字段：这里额外确认没有常见占位键。
        assert!(!rendered.contains("token_value"));
        assert!(!rendered.contains("secret"));
    }
}
