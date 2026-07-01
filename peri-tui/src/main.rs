use anyhow::Result;
use clap::{Parser, Subcommand};
use std::sync::OnceLock;

#[cfg(not(target_os = "windows"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

mod acp_stdio;
mod cli_args;
mod cli_plugin;
mod cli_print;

// ─── Panic Hook（TUI 专用）───────────────────────────────────────────────────

/// 全局 panic 通知通道 sender（OnceLock 保证只初始化一次）
static PANIC_NOTIFY: OnceLock<tokio::sync::mpsc::UnboundedSender<String>> = OnceLock::new();

/// 格式化 panic 信息为可读字符串（消息 + 位置 + backtrace）
fn format_panic_message(panic_info: &std::panic::PanicHookInfo<'_>) -> String {
    let payload = if let Some(s) = panic_info.payload().downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = panic_info.payload().downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_string()
    };

    let location = panic_info
        .location()
        .map(|loc| format!("{}:{}:{}", loc.file(), loc.line(), loc.column()))
        .unwrap_or_else(|| "unknown location".to_string());

    // 自动捕获 backtrace（无需手动设置 RUST_BACKTRACE=1）
    let backtrace = std::backtrace::Backtrace::capture();
    let bt_str = match backtrace.status() {
        std::backtrace::BacktraceStatus::Captured => format!("\n{}", backtrace),
        _ => String::new(),
    };

    format!("'{}'\n  at {}{}", payload, location, bt_str)
}

/// 安装自定义 panic hook：
/// - 通过 tracing::error! 记录到日志文件（不写 stderr）
/// - 通过 PANIC_NOTIFY 通道通知 TUI
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|panic_info| {
        let msg = format_panic_message(panic_info);
        tracing::error!("thread panicked at {}", msg);
        if let Some(tx) = PANIC_NOTIFY.get() {
            let _ = tx.send(msg);
        }
    }));
}

/// 创建 panic 通知通道并安装自定义 panic hook。
/// 必须在 enable_raw_mode() 之前调用。
/// 返回 UnboundedReceiver 供 TUI 消费。
pub fn init_panic_notify() -> tokio::sync::mpsc::UnboundedReceiver<String> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let _ = PANIC_NOTIFY.set(tx);
    install_panic_hook();
    rx
}

// ─── CLI 定义 ──────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "peri", version, about = "Peri AI Agent")]
struct Cli {
    /// 启用 HITL 审批模式（等同 --permission-mode default）
    #[arg(short = 'a', long = "approve")]
    approve: bool,

    // ── 非交互模式 ──
    /// 非交互模式：输出响应后退出
    #[arg(short = 'p', long = "print")]
    print: Option<Option<String>>,
    /// 输出格式：text / json / stream-json（需 -p）
    #[arg(long = "output-format", visible_alias = "outputFormat")]
    output_format: Option<String>,
    /// 最大 agentic 轮数（需 -p）
    #[arg(long = "max-turns", visible_alias = "maxTurns")]
    max_turns: Option<u32>,
    /// 极简模式：跳过 hooks/LSP/插件等初始化（需 -p）
    #[arg(long = "bare")]
    bare: bool,

    // ── 权限与安全 ──
    /// 权限模式：bypass / default / accept-edit / auto-mode
    #[arg(long = "permission-mode", visible_alias = "permissionMode")]
    permission_mode: Option<String>,
    /// 绕过所有权限检查（仅限沙箱环境）
    #[arg(long = "dangerously-skip-permissions")]
    skip_permissions: bool,

    // ── 模型与推理 ──
    /// 指定模型（别名如 sonnet 或全名）
    #[arg(long = "model")]
    model: Option<String>,
    /// 推理强度：low / medium / high / max
    #[arg(long = "effort")]
    effort: Option<String>,

    // ── 会话与对话 ──
    /// 继续当前目录最近的对话
    #[arg(short = 'c', long = "continue")]
    cont: bool,
    /// 按 session ID 恢复对话
    #[arg(short = 'r', long = "resume")]
    resume: Option<Option<String>>,
    /// 指定会话 ID（必须是有效 UUID）
    #[arg(long = "session-id", visible_alias = "sessionId")]
    session_id: Option<String>,
    /// 设置会话显示名称
    #[arg(short = 'n', long = "name")]
    session_name: Option<String>,
    /// 禁用会话持久化（需 -p）
    #[arg(long = "no-session-persistence")]
    no_session_persistence: bool,

    // ── 工具控制 ──
    /// 允许的工具列表（如 "Bash(git:*)" "Edit"）
    #[arg(long = "allowedTools", visible_alias = "allowed-tools")]
    allowed_tools: Option<Vec<String>>,
    /// 禁止的工具列表
    #[arg(long = "disallowedTools", visible_alias = "disallowed-tools")]
    disallowed_tools: Option<Vec<String>>,

    // ── 配置 ──
    /// 加载额外 settings 文件或 JSON 字符串
    #[arg(long = "settings")]
    settings: Option<String>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// 以 ACP Agent 模式运行（stdin/stdout JSON-RPC）
    Acp {
        /// 工作目录
        #[arg(long, default_value = ".")]
        cwd: String,
        /// 模型名称/别名
        #[arg(long)]
        model: Option<String>,
        /// Agent 类型（从 .claude/agents/ 中选择）
        #[arg(short = 'g', long)]
        agent: Option<String>,
    },
    /// 更新：从 GitHub 下载并安装最新版本
    Update,
    /// 配置同步：在设备间同步 settings/skills/mcp/plugins
    Sync {
        #[command(subcommand)]
        action: SyncAction,
        /// Relay server URL
        #[arg(long, default_value = "wss://peri-sync.claude-code-best.win")]
        server: String,
    },
    /// 插件管理
    Plugin {
        #[command(subcommand)]
        action: PluginAction,
    },
    /// 启动 Web PTY 终端服务
    Web,
}

#[derive(Subcommand)]
enum SyncAction {
    /// 发送本地配置到远端设备
    Sender,
    /// 从远端设备接收配置
    Receiver,
}

#[derive(Subcommand)]
enum PluginAction {
    /// 列出已安装的插件
    List {
        /// JSON 输出
        #[arg(long)]
        json: bool,
    },
    /// 安装插件
    Install {
        /// 插件名称（格式: name@marketplace）
        plugin: String,
        /// 安装范围：user / project / local
        #[arg(short = 's', long, default_value = "user")]
        scope: String,
    },
    /// 卸载插件
    Uninstall {
        /// 插件 ID（格式: name@marketplace）
        plugin: String,
        /// 卸载范围（不指定则从所有范围移除）
        #[arg(short = 's', long)]
        scope: Option<String>,
    },
}

// ─── 环境变量注入 ──────────────────────────────────────────────────────────

/// 从 settings.json 读取 env 字段并注入进程环境变量
/// 仅在进程环境变量不存在时设置（进程环境优先）
fn inject_env_from_settings() {
    let path = dirs_next::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".peri")
        .join("settings.json");

    inject_env_from_file(&path, &[&["config", "env"], &["env"]]);
}

/// 从 Claude Code 配置文件 ~/.claude/settings.json 读取 env 字段并注入进程环境变量。
///
/// Claude Code 将 API Key 等凭据存储在其 settings.json 的顶层 `env` 字段中。
/// 此函数在 Peri 自身配置加载后调用，确保即使 Peri 尚未配置也能接入已配置的
/// Claude Code 凭据。进程环境变量和 Peri 配置中的 env 优先级更高（不会被覆盖）。
fn inject_env_from_claude_settings() {
    let path = dirs_next::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".claude")
        .join("settings.json");

    inject_env_from_file(&path, &[&["env"]]);
}

/// 从指定 JSON 文件按优先级路径数组提取 env 字段并注入进程环境变量。
///
/// `env_paths` 每个元素是一个 JSON 路径段数组，如 `["config", "env"]` 表示 `json.config.env`。
/// 按数组顺序尝试，首次命中即停止。未命中任何路径则无操作。
fn inject_env_from_file(path: &std::path::Path, env_paths: &[&[&str]]) {
    if !path.exists() {
        return;
    }

    let Ok(content) = std::fs::read_to_string(path) else {
        return;
    };

    let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else {
        return;
    };

    for segments in env_paths {
        let mut current = &json;
        for seg in *segments {
            current = match current.get(*seg) {
                Some(v) => v,
                None => {
                    current = &serde_json::Value::Null;
                    break;
                }
            };
        }
        if let Some(env_map) = current.as_object() {
            inject_env_map(env_map);
            return;
        }
    }
}

/// 遍历 env map 注入进程环境变量，仅在变量未设置时写入
fn inject_env_map(env_map: &serde_json::Map<String, serde_json::Value>) {
    for (key, value) in env_map {
        if let Some(value_str) = value.as_str() {
            if std::env::var(key).is_err() {
                unsafe {
                    std::env::set_var(key, value_str);
                }
            }
        }
    }
}

/// 从指定路径或 JSON 字符串加载额外 settings 并合并到环境变量
fn inject_settings_override(source: &str) {
    let json_str = if std::path::Path::new(source).exists() {
        match std::fs::read_to_string(source) {
            Ok(content) => content,
            Err(e) => {
                eprintln!("警告: 无法读取 settings 文件 '{}': {e}", source);
                return;
            }
        }
    } else {
        source.to_string()
    };

    let Ok(json) = serde_json::from_str::<serde_json::Value>(&json_str) else {
        eprintln!("警告: --settings 内容不是有效的 JSON");
        return;
    };

    if let Some(env_obj) = json.get("config").and_then(|c| c.get("env")) {
        if let Some(env_map) = env_obj.as_object() {
            for (key, value) in env_map {
                if let Some(value_str) = value.as_str() {
                    if std::env::var(key).is_err() {
                        unsafe {
                            std::env::set_var(key, value_str);
                        }
                    }
                }
            }
        }
    }
}

// ─── 辅助函数 ──────────────────────────────────────────────────────────────

/// 统一创建 tokio runtime（4 workers，4MB stack），避免 7 处重复构造
fn build_runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .thread_stack_size(4 * 1024 * 1024)
        .enable_all()
        .build()
        .map_err(Into::into)
}

// ─── 入口 ──────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    // Set jemalloc MALLOC_CONF env vars BEFORE any allocation.
    // Must be the very first line — jemalloc reads these during init.
    peri_tui::alloc_config::init_alloc_conf();

    // 最先注入环境变量（进程环境变量优先）
    // 优先级：进程环境 > Peri 配置 > Claude Code 配置
    inject_env_from_settings();
    inject_env_from_claude_settings();

    let cli = Cli::parse();

    // -p/--print 模式（优先级高于子命令）
    if cli.print.is_some() {
        // 限制 worker 数（默认=CPU 核数，18 核=72MB 栈空间浪费），4 MB stack
        let rt = build_runtime()?;
        return rt.block_on(cli_print::run_print(
            cli.print.and_then(|o| o),
            cli.output_format,
            cli.max_turns,
            cli.bare,
            cli.model,
            cli.effort,
            cli.permission_mode,
            cli.skip_permissions,
            cli.allowed_tools.unwrap_or_default(),
            cli.disallowed_tools.unwrap_or_default(),
            cli.settings,
            None,
        ));
    }

    match cli.command {
        None => run_tui(TuiOptions {
            approve: cli.approve,
            permission_mode: cli.permission_mode,
            skip_permissions: cli.skip_permissions,
            model: cli.model,
            effort: cli.effort,
            continue_session: cli.cont,
            resume_session: cli.resume.and_then(|o| o),
            session_id: cli.session_id,
            session_name: cli.session_name,
            settings: cli.settings,
            allowed_tools: cli.allowed_tools.unwrap_or_default(),
            disallowed_tools: cli.disallowed_tools.unwrap_or_default(),
        }),
        Some(Commands::Acp {
            cwd,
            model: _,
            agent: _,
        }) => {
            // 限制 worker 数（默认=CPU 核数，18 核=72MB 栈空间浪费），4 MB stack
            let rt = build_runtime()?;
            rt.block_on(acp_stdio::run_acp_stdio(cwd))
        }
        Some(Commands::Update) => {
            // 限制 worker 数（默认=CPU 核数，18 核=72MB 栈空间浪费），4 MB stack
            let rt = build_runtime()?;
            rt.block_on(async {
                match peri_tui::update::run_update().await {
                    Ok(tag) => println!("Updated to {tag}"),
                    Err(e) => {
                        eprintln!("Update failed: {e:#}");
                        std::process::exit(1);
                    }
                }
                Ok(())
            })
        }
        Some(Commands::Sync { action, server }) => {
            // 限制 worker 数（默认=CPU 核数，18 核=72MB 栈空间浪费），4 MB stack
            let rt = build_runtime()?;
            rt.block_on(async {
                match action {
                    SyncAction::Sender => peri_tui::sync::run_sync_sender(&server).await,
                    SyncAction::Receiver => peri_tui::sync::run_sync_receiver(&server).await,
                }
            })
            .map(|_| println!("Sync complete"))
            .map_err(|e| {
                eprintln!("Sync failed: {e:#}");
                std::process::exit(1);
            })
        }
        Some(Commands::Plugin { action }) => {
            // 限制 worker 数（默认=CPU 核数，18 核=72MB 栈空间浪费），4 MB stack
            let rt = build_runtime()?;
            rt.block_on(async {
                match action {
                    PluginAction::List { json } => cli_plugin::run_plugin_list(json),
                    PluginAction::Install { plugin, scope } => {
                        cli_plugin::run_plugin_install(&plugin, &scope).await
                    }
                    PluginAction::Uninstall { plugin, scope } => {
                        cli_plugin::run_plugin_uninstall(&plugin, scope.as_deref()).await
                    }
                }
            })
        }
        Some(Commands::Web) => {
            let rt = build_runtime()?;
            rt.block_on(async {
                peri_web_pty::start_server(peri_web_pty::config::Config::from_env()).await
            })
            .map_err(|e| {
                eprintln!("Web PTY server error: {e:#}");
                std::process::exit(1);
            })
        }
    }
}

// ─── TUI 模式 ──────────────────────────────────────────────────────────────

/// TUI 模式启动选项
#[allow(dead_code)] // 部分 CLI 桥接字段尚未接入
struct TuiOptions {
    approve: bool,
    permission_mode: Option<String>,
    skip_permissions: bool,
    model: Option<String>,
    effort: Option<String>,
    continue_session: bool,
    resume_session: Option<String>,
    session_id: Option<String>,
    session_name: Option<String>,
    settings: Option<String>,
    allowed_tools: Vec<String>,
    disallowed_tools: Vec<String>,
}

fn run_tui(opts: TuiOptions) -> Result<()> {
    // --settings 覆盖
    if let Some(ref settings_path) = opts.settings {
        inject_settings_override(settings_path);
    }

    if opts.approve {
        unsafe {
            std::env::set_var("YOLO_MODE", "false");
        }
    }

    if opts.skip_permissions {
        unsafe {
            std::env::set_var("YOLO_MODE", "true");
        }
    }

    // 在创建 tokio runtime 之前初始化 tracing，确保 reqwest::blocking::Client
    // 的内部 runtime 与应用 runtime 完全隔离，避免嵌套 runtime drop panic。
    let _telemetry = peri_agent::telemetry::init_tracing("agent-tui");

    // 安装自定义 panic hook，必须在 enable_raw_mode() 之前，
    // 否则 Rust 默认 panic hook 的 stderr 输出会破坏 TUI 画面。
    let panic_notify_rx = init_panic_notify();

    // 限制 worker 数（默认=CPU 核数，18 核=72MB 栈空间浪费），4 MB stack
    let rt = build_runtime()?;

    let result = rt.block_on(async {
        // ratatui-kit fullscreen() 自行管理 raw mode / alternate screen / 事件循环。
        // 外层不做任何终端操作。
        let launch_opts = peri_tui::launch::TuiLaunchOptions {
            approve: opts.approve,
            permission_mode: opts.permission_mode.clone(),
            skip_permissions: opts.skip_permissions,
            model: opts.model.clone(),
            effort: opts.effort.clone(),
            continue_session: opts.continue_session,
            resume_session: opts.resume_session.clone(),
            session_id: opts.session_id.clone(),
            session_name: opts.session_name.clone(),
            settings: opts.settings.clone(),
            allowed_tools: opts.allowed_tools.clone(),
            disallowed_tools: opts.disallowed_tools.clone(),
        };
        peri_tui::kit::entry::run_kit_fullscreen(launch_opts, panic_notify_rx).await
    });

    // 先 drop rt（关闭所有 tokio 任务），再 drop _telemetry
    drop(rt);
    drop(_telemetry);

    if let Err(e) = result {
        eprintln!("Error: {e}");
    }

    Ok(())
}

#[cfg(test)]
mod cli_integration_test;

#[cfg(test)]
mod tests {
    use super::*;

    fn make_temp_file(content: &str) -> tempfile::TempPath {
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(content.as_bytes()).unwrap();
        file.into_temp_path()
    }

    #[test]
    fn test_inject_from_config_env() {
        // 测试 config.env 标准格式
        let path = make_temp_file(r#"{"config": {"env": {"TEST_C1": "v1"}}}"#);
        inject_env_from_file(&path, &[&["config", "env"]]);
        assert_eq!(std::env::var("TEST_C1").unwrap(), "v1");
        unsafe {
            std::env::remove_var("TEST_C1");
        }
    }

    #[test]
    fn test_inject_from_top_level_env() {
        // 测试顶层 env 格式（兼容旧格式/Claude Code 格式）
        let path = make_temp_file(r#"{"env": {"TEST_T1": "v2"}}"#);
        inject_env_from_file(&path, &[&["env"]]);
        assert_eq!(std::env::var("TEST_T1").unwrap(), "v2");
        unsafe {
            std::env::remove_var("TEST_T1");
        }
    }

    #[test]
    fn test_inject_fallback_order() {
        // 测试优先 config.env 再回退顶层 env
        // 只存在顶层 env 时应该回退成功
        let path = make_temp_file(r#"{"env": {"TEST_FB1": "from_fallback"}}"#);
        inject_env_from_file(&path, &[&["config", "env"], &["env"]]);
        assert_eq!(std::env::var("TEST_FB1").unwrap(), "from_fallback");
        unsafe {
            std::env::remove_var("TEST_FB1");
        }
    }

    #[test]
    fn test_inject_config_env_priority_over_top_level() {
        // config.env 存在时优先使用，不回退到顶层 env
        let path = make_temp_file(
            r#"{"config": {"env": {"TEST_PRI": "from_config"}}, "env": {"TEST_PRI": "from_top"}}"#,
        );
        inject_env_from_file(&path, &[&["config", "env"], &["env"]]);
        assert_eq!(std::env::var("TEST_PRI").unwrap(), "from_config");
        unsafe {
            std::env::remove_var("TEST_PRI");
        }
    }

    #[test]
    fn test_process_env_priority() {
        // 进程环境变量存在时不被 settings.json 覆盖
        unsafe {
            std::env::set_var("TEST_PROC_PRI", "from_process");
        }
        let path = make_temp_file(r#"{"env": {"TEST_PROC_PRI": "from_file"}}"#);
        inject_env_from_file(&path, &[&["env"]]);
        assert_eq!(std::env::var("TEST_PROC_PRI").unwrap(), "from_process");
        unsafe {
            std::env::remove_var("TEST_PROC_PRI");
        }
    }

    #[test]
    fn test_skip_non_string_values() {
        // 非字符串值应跳过不 panic
        let path = make_temp_file(r#"{"env": {"TEST_NUM": 123, "TEST_STR": "ok"}}"#);
        inject_env_from_file(&path, &[&["env"]]);
        // 数字值不应被注入
        assert!(std::env::var("TEST_NUM").is_err());
        assert_eq!(std::env::var("TEST_STR").unwrap(), "ok");
        unsafe {
            std::env::remove_var("TEST_STR");
        }
    }

    #[test]
    fn test_no_file_no_panic() {
        // 文件不存在时不应 panic
        let path = std::path::PathBuf::from("/nonexistent/path/settings.json");
        inject_env_from_file(&path, &[&["env"]]);
    }

    #[test]
    fn test_no_env_field_no_panic() {
        // JSON 中没有 env 字段时不应 panic
        let path = make_temp_file(r#"{"other": "data"}"#);
        inject_env_from_file(&path, &[&["config", "env"], &["env"]]);
    }

    /// 端到端测试：模拟顶层 env 格式 → 注入进程环境 → LlmProvider::from_env() 可用
    #[test]
    fn test_e2e_top_level_env_to_provider() {
        // 保存可能被覆盖的环境变量
        let save_keys = ["TEST_E2E_API_KEY", "TEST_E2E_BASE_URL", "MODEL_PROVIDER"];
        let saved: Vec<(&str, Option<String>)> = save_keys
            .iter()
            .map(|k| (*k, std::env::var(k).ok()))
            .collect();

        // 创建一个顶层 env 格式的配置文件（模拟当前 ~/.peri/settings.json 的格式）
        let path = make_temp_file(
            r#"{"env": {"TEST_E2E_API_KEY": "sk-e2e-test-key", "TEST_E2E_BASE_URL": "https://e2e-test.example.com/v1"}}"#,
        );

        // 调用注入函数（使用 inject_env_from_settings 相同的查找策略）
        inject_env_from_file(&path, &[&["config", "env"], &["env"]]);

        // 验证环境变量已注入
        assert_eq!(
            std::env::var("TEST_E2E_API_KEY").unwrap(),
            "sk-e2e-test-key"
        );
        assert_eq!(
            std::env::var("TEST_E2E_BASE_URL").unwrap(),
            "https://e2e-test.example.com/v1"
        );

        // 清理测试环境变量
        unsafe {
            std::env::remove_var("TEST_E2E_API_KEY");
        }
        unsafe {
            std::env::remove_var("TEST_E2E_BASE_URL");
        }

        // 恢复之前保存的环境变量
        for (key, value) in saved {
            match value {
                Some(v) => unsafe {
                    std::env::set_var(key, v);
                },
                None => unsafe { std::env::remove_var(key) },
            }
        }
    }
}
// test
