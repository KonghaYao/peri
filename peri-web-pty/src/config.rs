use clap::Parser;

/// PTY server 启动配置。
#[derive(Debug, Clone, Parser)]
#[command(name = "peri-web-pty", about = "Web PTY terminal server")]
pub struct Config {
    /// 监听端口（默认 0 = 随机分配）
    #[arg(long, env = "PORT", default_value_t = 0)]
    pub port: u16,

    /// 默认 shell（默认 $SHELL 或 /bin/bash）
    #[arg(long, env = "SHELL")]
    pub shell: Option<String>,

    /// 工作目录
    #[arg(long, env = "CWD")]
    pub cwd: Option<String>,

    /// 第一个 shell 启动时自动注入的命令
    #[arg(long = "cmd", env = "CMD")]
    pub initial_cmd: Option<String>,

    /// 默认终端列数
    #[arg(long, default_value_t = 80)]
    pub default_cols: u16,

    /// 默认终端行数
    #[arg(long, default_value_t = 24)]
    pub default_rows: u16,
}

impl Config {
    /// 从 CLI args / 环境变量构造配置。
    pub fn from_args() -> Self {
        Parser::parse()
    }

    /// 仅从环境变量构造配置（不解析 CLI args），供 `peri web` 等嵌入式场景使用。
    /// 默认 initial_cmd 为 "peri"（自动进入 Peri 对话）。
    pub fn from_env() -> Self {
        Self {
            port: std::env::var("PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(0),
            shell: std::env::var("SHELL").ok(),
            cwd: std::env::var("CWD").ok(),
            initial_cmd: std::env::var("CMD")
                .ok()
                .or_else(|| Some("peri".to_string())),
            default_cols: 80,
            default_rows: 24,
        }
    }

    #[cfg(test)]
    /// 测试用：从给定的 args 迭代器 + 环境变量构造配置。
    pub fn parse_from<I, T>(iter: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString> + Clone,
    {
        <Self as Parser>::parse_from(iter)
    }
}

pub fn default_shell() -> String {
    if cfg!(target_os = "windows") {
        "cmd.exe".to_string()
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string())
    }
}
