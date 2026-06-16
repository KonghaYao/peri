use std::io;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::{Parser, Subcommand};
use peri_acp::transport::mpsc::mpsc_transport_pair;
use peri_tui::{
    acp_client::AcpTuiClient,
    acp_server::{run_acp_server, AcpServerConfig},
    app::App,
    event, ui,
};
use ratatui::{
    crossterm::{
        event::{
            DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
            EnableFocusChange, EnableMouseCapture,
        },
        execute,
        terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    },
    prelude::*,
};

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
    // ── 向后兼容 ──
    /// 向后兼容，无操作（YOLO 已是默认行为）
    #[arg(short = 'y', long = "yolo")]
    yolo: bool,
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
    /// 权限模式：bypass / default / dont-ask / accept-edit / auto-mode
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
                std::env::set_var(key, value_str);
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
                        std::env::set_var(key, value_str);
                    }
                }
            }
        }
    }
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
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4) // 限制 worker 数（默认=CPU 核数，18 核=72MB 栈空间浪费）
            .thread_stack_size(4 * 1024 * 1024) // 4 MB (default: 8 MB)
            .enable_all()
            .build()?;
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
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4) // 限制 worker 数（默认=CPU 核数，18 核=72MB 栈空间浪费）
                .thread_stack_size(4 * 1024 * 1024) // 4 MB (default: 8 MB)
                .enable_all()
                .build()?;
            rt.block_on(acp_stdio::run_acp_stdio(cwd))
        }
        Some(Commands::Update) => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4) // 限制 worker 数（默认=CPU 核数，18 核=72MB 栈空间浪费）
                .thread_stack_size(4 * 1024 * 1024) // 4 MB (default: 8 MB)
                .enable_all()
                .build()?;
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
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4) // 限制 worker 数（默认=CPU 核数，18 核=72MB 栈空间浪费）
                .thread_stack_size(4 * 1024 * 1024) // 4 MB (default: 8 MB)
                .enable_all()
                .build()?;
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
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4) // 限制 worker 数（默认=CPU 核数，18 核=72MB 栈空间浪费）
                .thread_stack_size(4 * 1024 * 1024) // 4 MB (default: 8 MB)
                .enable_all()
                .build()?;
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
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .thread_stack_size(4 * 1024 * 1024)
                .enable_all()
                .build()?;
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
        std::env::set_var("YOLO_MODE", "false");
    }

    if opts.skip_permissions {
        std::env::set_var("YOLO_MODE", "true");
    }

    // 在创建 tokio runtime 之前初始化 tracing，确保 reqwest::blocking::Client
    // 的内部 runtime 与应用 runtime 完全隔离，避免嵌套 runtime drop panic。
    let _telemetry = peri_agent::telemetry::init_tracing("agent-tui");

    // 安装自定义 panic hook，必须在 enable_raw_mode() 之前，
    // 否则 Rust 默认 panic hook 的 stderr 输出会破坏 TUI 画面。
    let panic_notify_rx = init_panic_notify();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4) // 限制 worker 数（默认=CPU 核数，18 核=72MB 栈空间浪费）
        .thread_stack_size(4 * 1024 * 1024) // 4 MB (default: 8 MB)
        .enable_all()
        .build()?;

    let result = rt.block_on(async {
        // 初始化终端
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(
            stdout,
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste,
            EnableFocusChange
        )?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        // 运行应用
        let result = run_app(&mut terminal, &opts, panic_notify_rx).await;

        // 恢复终端
        disable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            DisableBracketedPaste,
            DisableFocusChange
        )?;
        terminal.show_cursor()?;

        result
    });

    // 先 drop rt（关闭所有 tokio 任务），再 drop _telemetry
    drop(rt);
    drop(_telemetry);

    if let Err(e) = result {
        eprintln!("Error: {e}");
    }

    Ok(())
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    tui_opts: &TuiOptions,
    panic_notify_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
) -> Result<()> {
    let mut app = App::new().await;

    // 接入 panic hook 通知通道
    app.services.panic_notify_rx = Some(panic_notify_rx);

    // 根据环境变量/CLI 参数设置初始权限模式
    {
        use peri_middlewares::prelude::PermissionMode;
        let initial_mode = if tui_opts.skip_permissions {
            PermissionMode::Bypass
        } else if let Some(ref mode_str) = tui_opts.permission_mode {
            match mode_str.as_str() {
                "bypass" => PermissionMode::Bypass,
                "default" => PermissionMode::Default,
                "dont-ask" => PermissionMode::DontAsk,
                "accept-edit" => PermissionMode::AcceptEdit,
                "auto-mode" => PermissionMode::AutoMode,
                _ => {
                    if std::env::var("YOLO_MODE")
                        .map(|v| !v.eq_ignore_ascii_case("false") && v != "0")
                        .unwrap_or(true)
                    {
                        PermissionMode::Bypass
                    } else {
                        PermissionMode::Default
                    }
                }
            }
        } else if tui_opts.approve {
            PermissionMode::Default
        } else if std::env::var("YOLO_MODE")
            .map(|v| !v.eq_ignore_ascii_case("false") && v != "0")
            .unwrap_or(true)
        {
            PermissionMode::Bypass
        } else {
            PermissionMode::Default
        };
        app.services.permission_mode.store(initial_mode);
    }

    // --model 覆盖
    if let Some(ref model_str) = tui_opts.model {
        let config = app.services.peri_config.read();
        if let Some(new_provider) =
            peri_tui::app::agent::LlmProvider::from_config_for_alias(&config, model_str)
        {
            tracing::info!(model = %new_provider.model_name(), "CLI --model 覆盖生效");
        }
    }

    // 会话恢复：-c 恢复当前目录最近会话，-r <id> 恢复指定会话
    if let Some(ref session_id) = tui_opts.resume_session {
        tracing::info!(session_id = %session_id, "-r: 恢复指定会话");
        app.open_thread(session_id.clone());
    } else if tui_opts.continue_session {
        let store = app.services.thread_store.clone();
        let cwd = app.services.cwd.clone();
        let thread_id = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                let threads = store.list_threads().await.ok()?;
                threads.into_iter().find(|t| t.cwd == cwd).map(|t| t.id)
            })
        });
        if let Some(tid) = thread_id {
            tracing::info!(thread_id = %tid, "-c: 恢复最近会话");
            app.open_thread(tid);
        } else {
            tracing::info!("-c: 当前目录无历史会话，创建新会话");
        }
    }

    // 检测是否需要 Setup 向导
    {
        let cfg = app.services.peri_config.read();
        if peri_tui::app::setup_wizard::needs_setup(&cfg.config) {
            app.global_ui.setup_wizard = Some(peri_tui::app::SetupWizardPanel::new());
        }
    }

    // 后台初始化 MCP 连接池（不阻塞 UI）
    app.spawn_mcp_init();

    // 加载已启用插件数据
    {
        let claude_dir = dirs_next::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".claude");
        app.services.plugin_data = Some(peri_middlewares::plugin::load_enabled_plugins_aggregated(
            &claude_dir,
        ));
        // 将插件命令注册到所有 session 的 CommandRegistry
        let plugin_commands = app
            .services
            .plugin_data
            .as_ref()
            .map(|pd| pd.all_commands.clone())
            .unwrap_or_default();
        // 将插件 skills 追加到所有 session 的 skill 列表
        let plugin_skill_dirs = app
            .services
            .plugin_data
            .as_ref()
            .map(|pd| pd.all_skill_dirs.clone())
            .unwrap_or_default();
        let plugin_skills = peri_middlewares::skills::list_skills(&plugin_skill_dirs);
        app.session_mgr
            .current_mut()
            .commands
            .command_registry
            .register_plugin_commands(plugin_commands.clone());
        let session = app.session_mgr.current_mut();
        let existing_names: std::collections::HashSet<String> = session
            .commands
            .skills
            .iter()
            .map(|s| s.name.clone())
            .collect();
        for skill in &plugin_skills {
            if !existing_names.contains(&skill.name) {
                session.commands.skills.push(skill.clone());
            }
        }
    }

    // ── Step 6-a: Setup ACP Server + Client ──────────────────────────────
    {
        let provider = {
            let cfg_guard = app.services.peri_config.read();
            peri_tui::app::LlmProvider::from_config(&cfg_guard)
        }
        .or_else(peri_tui::app::LlmProvider::from_env);

        if let Some(provider) = provider {
            // Gather plugin configs
            let plugin_skill_dirs = app
                .services
                .plugin_data
                .as_ref()
                .map(|pd| pd.all_skill_dirs.clone())
                .unwrap_or_default();
            let plugin_agent_dirs = app
                .services
                .plugin_data
                .as_ref()
                .map(|pd| pd.all_agent_dirs.clone())
                .unwrap_or_default();
            let plugin_lsp_servers = app
                .services
                .plugin_data
                .as_ref()
                .map(|pd| pd.all_lsp_servers.clone())
                .unwrap_or_default();
            let plugin_hooks = app
                .services
                .plugin_data
                .as_ref()
                .map(|pd| pd.all_hooks.clone())
                .unwrap_or_default();

            // Build hook groups from plugin hooks + global hooks + local hooks
            let mut hook_groups: Vec<Vec<peri_middlewares::hooks::RegisteredHook>> = Vec::new();
            if !plugin_hooks.is_empty() {
                hook_groups.push(plugin_hooks);
            }
            let global_hooks = peri_middlewares::hooks::loader::load_global_settings_hooks();
            if !global_hooks.is_empty() {
                hook_groups.push(global_hooks);
            }
            let local_hooks =
                peri_middlewares::hooks::loader::load_settings_local_hooks(&app.services.cwd);
            if !local_hooks.is_empty() {
                hook_groups.push(local_hooks);
            }

            let flat_hooks: Vec<peri_middlewares::hooks::RegisteredHook> =
                hook_groups.iter().flatten().cloned().collect();
            tracing::info!(
                groups = hook_groups.len(),
                total_hooks = flat_hooks.len(),
                "Hook groups assembled for ACP server"
            );

            // Create session-level tool_search_index and shared_tools
            let tool_search_index = Arc::new(peri_middlewares::tool_search::ToolSearchIndex::new());
            let shared_tools = Arc::new(parking_lot::RwLock::new(std::collections::HashMap::new()));

            // 构建 SessionManager：支撑 SubAgent cascade cancel 与 goal_state 跨 prompt 共享。
            // TUI 本地仍维护 SessionState（history/frozen/agent_pool 等），SessionManager
            // 只持有 AcpSession 元数据 + active_agents + goal_state。
            //
            // 关键：session_manager 与 server_config 共享同一 `Arc<RwLock<PeriConfig>>`，
            // 与 ServiceRegistry.peri_config 也是同一 Arc —— Single Source of Truth。
            let shared_peri_config = app.services.peri_config.clone();
            // SessionManager 接收 `Arc<PeriConfig>`（frozen 快照），与 AcpServerConfig
            // 的 `Arc<RwLock<PeriConfig>>` 不同：SessionManager 仅用于 cascade cancel
            // 与 goal_state，不参与热更新，故传一份快照。
            let session_manager_peri_config_snapshot =
                Arc::new(app.services.peri_config.read().clone());
            let session_manager = peri_acp::session::SessionManager::new(
                app.services.thread_store.clone(),
                provider.clone(),
                session_manager_peri_config_snapshot,
                app.services.permission_mode.clone(),
                None,
            );

            let server_config = AcpServerConfig {
                provider: Arc::new(parking_lot::RwLock::new(provider.clone())),
                peri_config: shared_peri_config,
                permission_mode: app.services.permission_mode.clone(),
                cron_scheduler: Some(app.services.cron.scheduler.clone()),
                mcp_pool: app.services.mcp_pool.clone(),
                channel_state: app.services.channel_state.clone(),
                plugin_skill_dirs,
                plugin_agent_dirs,
                plugin_hooks: flat_hooks,
                hook_groups,
                plugin_lsp_servers,
                tool_search_index: tool_search_index.clone(),
                shared_tools: shared_tools.clone(),
                thread_store: app.services.thread_store.clone(),
                langfuse_session: {
                    if let Some(config) = peri_acp::langfuse::LangfuseConfig::from_env() {
                        tracing::info!("Langfuse tracing enabled (TUI mode)");
                        peri_acp::langfuse::LangfuseSession::new(config)
                            .await
                            .map(Arc::new)
                    } else {
                        None
                    }
                },
                config_path: peri_tui::config::config_path(),
                session_manager,
            };

            let (client_transport, server_transport) = mpsc_transport_pair();
            tokio::spawn(async move {
                run_acp_server(Arc::new(server_transport), server_config).await;
            });

            let (acp_client, notification_rx) = AcpTuiClient::new(client_transport);
            // Spawn notification pump
            acp_client.spawn_pump();
            // Wire notification receiver to active session's AgentComm
            app.session_mgr.current_mut().agent.acp_notification_rx = Some(notification_rx);
            app.acp_client = Some(acp_client);
        }
    }

    // Spinner tick 驱动：每次渲染前推进一帧
    app.session_mgr.current_mut().spinner_state.advance_tick();

    // 初始全量绘制一次
    terminal.draw(|f| ui::main_ui::render(f, &mut app))?;
    let mut last_render = Instant::now();

    /// loading 动画帧率限制间隔（约 30 FPS）。
    /// 仅在 loading=true 且无用户事件的 poll 超时路径生效，
    /// 用户交互（键盘/鼠标/resize）始终立即渲染。
    const TARGET_FRAME_INTERVAL: Duration = Duration::from_millis(33);

    'event_loop: loop {
        // 推进 Spinner 动画帧
        app.session_mgr.current_mut().spinner_state.advance_tick();
        // 轮询 agent 结果
        let mut agent_updated = false;
        agent_updated |= app.poll_agent();
        agent_updated |= app.poll_at_mention();
        // 轮询后台事件（MCP OAuth 等）
        let bg_updated = app.poll_background_events();
        // 轮询 panic hook 通知
        let panic_updated = app.poll_panic_notifications();
        // 检查 cron 定时触发
        app.poll_cron_triggers();

        match event::next_event(&mut app).await? {
            Some(action) => match action {
                event::Action::Quit => break 'event_loop,
                event::Action::Submit(input) => {
                    app.submit_message(input);
                    terminal.draw(|f| ui::main_ui::render(f, &mut app))?;
                    last_render = Instant::now();
                }
                event::Action::Redraw => {
                    // 有用户交互（键盘/鼠标/resize）→ 始终重绘
                    terminal.draw(|f| ui::main_ui::render(f, &mut app))?;
                    last_render = Instant::now();
                }
            },
            None => {
                // 无用户事件（poll 超时）：在阻塞结束后重新读取缓存版本
                // 这样能捕获渲染线程在等待期间发出的更新
                let cache_version = app
                    .session_mgr
                    .current_mut()
                    .messages
                    .render_cache
                    .read()
                    .version;
                let cache_updated =
                    cache_version != app.session_mgr.current_mut().messages.last_render_version;
                let loading = app.session_mgr.current_mut().ui.loading;
                let should_render =
                    cache_updated || agent_updated || bg_updated || panic_updated || loading;
                if should_render {
                    let now = Instant::now();
                    // loading 路径：限制帧率到 TARGET_FRAME_INTERVAL，降低 CPU 开销
                    // 非 loading 路径（cache_updated/agent_updated/bg_updated）始终立即渲染
                    if !loading || now.duration_since(last_render) >= TARGET_FRAME_INTERVAL {
                        terminal.draw(|f| ui::main_ui::render(f, &mut app))?;
                        last_render = now;
                    }
                }
            }
        }
        // /exit 或 /quit 命令设置的退出标志
        if app.global_ui.quit_requested {
            break 'event_loop;
        }
    }

    // Fire SessionEnd hooks before shutdown
    {
        let mut hooks = app
            .services
            .plugin_data
            .as_ref()
            .map(|pd| pd.all_hooks.clone())
            .unwrap_or_default();
        hooks.extend(peri_middlewares::hooks::loader::load_global_settings_hooks());
        hooks.extend(peri_middlewares::hooks::loader::load_settings_local_hooks(
            &app.services.cwd,
        ));
        if !hooks.is_empty() {
            let cwd = app.services.cwd.clone();
            let provider_name = app.services.provider_name.clone();
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    peri_middlewares::hooks::middleware::fire_standalone_lifecycle_hooks(
                        &hooks,
                        peri_middlewares::hooks::types::HookEvent::SessionEnd,
                        &cwd,
                        "",
                        "",
                        &provider_name,
                        None,
                        Some("prompt_input_exit"),
                    )
                    .await;
                })
            });
        }
    }

    // 关闭 MCP 连接池（断开所有 MCP 服务器连接，清理子进程）
    if let Some(pool) = app.services.mcp_pool.take() {
        tracing::info!("正在关闭 MCP 连接池...");
        tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(pool.shutdown()));
        tracing::info!("MCP 连接池已关闭");
    }

    // 等待最后一次 Langfuse flush 完成，防止 runtime drop 前 batcher 数据丢失
    if let Some(handle) = app
        .session_mgr
        .current_mut()
        .langfuse
        .langfuse_flush_handle
        .take()
    {
        let _ = handle.await;
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
        std::env::remove_var("TEST_C1");
    }

    #[test]
    fn test_inject_from_top_level_env() {
        // 测试顶层 env 格式（兼容旧格式/Claude Code 格式）
        let path = make_temp_file(r#"{"env": {"TEST_T1": "v2"}}"#);
        inject_env_from_file(&path, &[&["env"]]);
        assert_eq!(std::env::var("TEST_T1").unwrap(), "v2");
        std::env::remove_var("TEST_T1");
    }

    #[test]
    fn test_inject_fallback_order() {
        // 测试优先 config.env 再回退顶层 env
        // 只存在顶层 env 时应该回退成功
        let path = make_temp_file(r#"{"env": {"TEST_FB1": "from_fallback"}}"#);
        inject_env_from_file(&path, &[&["config", "env"], &["env"]]);
        assert_eq!(std::env::var("TEST_FB1").unwrap(), "from_fallback");
        std::env::remove_var("TEST_FB1");
    }

    #[test]
    fn test_inject_config_env_priority_over_top_level() {
        // config.env 存在时优先使用，不回退到顶层 env
        let path = make_temp_file(
            r#"{"config": {"env": {"TEST_PRI": "from_config"}}, "env": {"TEST_PRI": "from_top"}}"#,
        );
        inject_env_from_file(&path, &[&["config", "env"], &["env"]]);
        assert_eq!(std::env::var("TEST_PRI").unwrap(), "from_config");
        std::env::remove_var("TEST_PRI");
    }

    #[test]
    fn test_process_env_priority() {
        // 进程环境变量存在时不被 settings.json 覆盖
        std::env::set_var("TEST_PROC_PRI", "from_process");
        let path = make_temp_file(r#"{"env": {"TEST_PROC_PRI": "from_file"}}"#);
        inject_env_from_file(&path, &[&["env"]]);
        assert_eq!(std::env::var("TEST_PROC_PRI").unwrap(), "from_process");
        std::env::remove_var("TEST_PROC_PRI");
    }

    #[test]
    fn test_skip_non_string_values() {
        // 非字符串值应跳过不 panic
        let path = make_temp_file(r#"{"env": {"TEST_NUM": 123, "TEST_STR": "ok"}}"#);
        inject_env_from_file(&path, &[&["env"]]);
        // 数字值不应被注入
        assert!(std::env::var("TEST_NUM").is_err());
        assert_eq!(std::env::var("TEST_STR").unwrap(), "ok");
        std::env::remove_var("TEST_STR");
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
        std::env::remove_var("TEST_E2E_API_KEY");
        std::env::remove_var("TEST_E2E_BASE_URL");

        // 恢复之前保存的环境变量
        for (key, value) in saved {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}
// test
