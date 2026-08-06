use anyhow::Result;
use clap::{Parser, Subcommand};

#[cfg(not(target_os = "windows"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

mod cli_args;
mod cli_plugin;
mod cli_print;

// ─── Panic Hook（TUI 专用）───────────────────────────────────────────────────
// 实现已移至 peri_tui::kit::panic（lib 侧），AppShell mount 后重装 hook，
// 覆盖 ratatui::init() 的包装 hook——见 kit/panic.rs 模块注释。
use peri_acp::host::stdio::StdioAssemblyInput;
use peri_tui::kit::panic::init_panic_notify;

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
        /// Server URL（仅 HTTPS；http/ws/wss 一律拒绝）
        #[arg(
            long,
            default_value = "https://peri-sync.claude-code-best.win",
            global = true
        )]
        server: String,
        /// 显式加密 keystore 文件路径（仅打开已存在的加密 keystore）
        #[arg(long, global = true)]
        keystore_path: Option<String>,
    },
    /// 插件管理
    Plugin {
        #[command(subcommand)]
        action: PluginAction,
    },
    /// 启动 Web PTY 终端服务
    Web {
        /// 监听地址（默认 0.0.0.0，监听所有网卡）
        #[arg(long, default_value = "0.0.0.0", env = "HOST")]
        host: String,
        /// 监听端口（默认 0 = 随机分配）
        #[arg(long, default_value_t = 0, env = "PORT")]
        port: u16,
    },
}

#[derive(Subcommand)]
enum SyncAction {
    /// 设备身份与信任管理
    Device {
        #[command(subcommand)]
        action: peri_tui::sync::device_cli::DeviceAction,
    },
    /// 发送本地配置到已信任远端设备
    Send {
        /// 目标设备 ID（trusted peers 中）
        #[arg(long)]
        to: String,
    },
    /// 从已信任远端设备接收配置（掩码输入同步码）
    Receive,
    /// 旧 WebSocket 发送模式（Slice 4 移除）
    Sender,
    /// 旧 WebSocket 接收模式（Slice 4 移除）
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
    /// 管理 marketplace 注册
    Marketplace {
        #[command(subcommand)]
        action: MarketplaceAction,
    },
    /// 启用插件
    Enable {
        /// 插件 ID（格式: name@marketplace）
        plugin: String,
        /// 作用范围：user / project / local
        #[arg(long, short)]
        scope: Option<String>,
    },
    /// 禁用插件
    Disable {
        /// 插件 ID（格式: name@marketplace）
        plugin: String,
        /// 作用范围：user / project / local
        #[arg(long, short)]
        scope: Option<String>,
    },
    /// 更新已安装的插件
    Update {
        /// 插件名称（格式: name@marketplace）
        plugin: String,
        /// 安装范围：user / project / local
        #[arg(long, short)]
        scope: Option<String>,
    },
    /// 查看插件详细信息
    Info {
        /// 插件 ID（格式: name@marketplace）
        plugin: String,
    },
    /// 搜索 marketplace 插件
    Search {
        /// 搜索关键词
        query: String,
    },
    /// 清理 7 天未使用的孤儿插件文件
    Cleanup,
}

#[derive(Subcommand)]
enum MarketplaceAction {
    /// 添加一个 marketplace
    Add {
        /// marketplace 来源（GitHub 简写 "user/repo"、URL、本地路径等）
        source: String,
    },
    /// 列出已注册的 marketplace
    List,
    /// 删除一个 marketplace
    Remove {
        /// marketplace 名称
        name: String,
    },
    /// 更新 marketplace 缓存
    Update {
        /// marketplace 名称
        name: String,
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
        if let Some(value_str) = value.as_str()
            && std::env::var(key).is_err()
        {
            unsafe {
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

    if let Some(env_obj) = json.get("config").and_then(|c| c.get("env"))
        && let Some(env_map) = env_obj.as_object()
    {
        inject_env_map(env_map);
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
    // 优先级：进程环境 > 项目本地配置 > Peri 全局配置 > Claude Code 配置
    // 项目本地配置（./.peri/settings.json），项目覆盖全局
    if let Some(path) = peri_tui::config::workspace_config_path() {
        inject_env_from_file(&path, &[&["config", "env"], &["env"]]);
    }
    inject_env_from_settings(); // ~/.peri/settings.json
    inject_env_from_claude_settings(); // ~/.claude/settings.json

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
            rt.block_on(async {
                // stdio host 位于 ACP 层（部署装配点），cli 仅作为启动入口调用；
                // thread 存储经 Resources 门面打开后注入（§0：ACP 层不直接依赖 Resources）
                let resources = peri_resources::Resources::open()
                    .await
                    .map_err(|e| anyhow::anyhow!("无法初始化 Resources 层: {e}"))?;
                // 3.0 批 2 波 2：装配点（cli 白名单文件）构造具体实现后以端口
                // 注入（§0 依赖方向；ACP 侧只持接口，不直接 new 资源类）。
                let cwd_str = cwd.clone();
                let cron_scheduler = std::sync::Arc::new(parking_lot::Mutex::new(
                    peri_middlewares::cron::CronScheduler::new(
                        tokio::sync::mpsc::unbounded_channel().0,
                    ),
                ));
                let claude_dir = dirs_next::home_dir()
                    .unwrap_or_else(|| std::path::PathBuf::from("."))
                    .join(".claude");
                let plugin_data = peri_middlewares::plugin::load_enabled_plugins_aggregated(
                    &claude_dir,
                    Some(std::path::Path::new(&cwd_str)),
                );
                peri_acp::host::stdio::run_acp_stdio(StdioAssemblyInput {
                    cwd: cwd_str,
                    thread_store: resources.thread_store(),
                    permission_mode: peri_acp_types::permission::SharedPermissionMode::new(
                        peri_acp_types::permission::PermissionMode::Bypass,
                    ),
                    cron_scheduler: Some(std::sync::Arc::new(
                        peri_middlewares::cron::CronSchedulerPortHandle(cron_scheduler),
                    )),
                    mcp_pool: None,
                    tool_search_index: std::sync::Arc::new(
                        peri_middlewares::tool_search::ToolSearchIndex::new(),
                    ),
                    skills: std::sync::Arc::new(peri_middlewares::host_ports::SkillsProvider),
                    settings_hooks: std::sync::Arc::new(
                        peri_middlewares::host_ports::SettingsHooksLoader,
                    ),
                    plugin_skill_roots: plugin_data.all_skill_roots,
                    plugin_agent_dirs: plugin_data.all_agent_dirs,
                    plugin_hooks: plugin_data.all_hooks,
                    plugin_loaded: plugin_data.plugins,
                    plugin_lsp_servers: plugin_data.all_lsp_servers,
                })
                .await
            })
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
        Some(Commands::Sync {
            action,
            server,
            keystore_path,
        }) => {
            // 限制 worker 数（默认=CPU 核数，18 核=72MB 栈空间浪费），4 MB stack
            let rt = build_runtime()?;
            let keystore_path = keystore_path.as_deref().map(std::path::Path::new);
            rt.block_on(async {
                match action {
                    SyncAction::Device { action } => {
                        peri_tui::sync::device_cli::dispatch(action, keystore_path)
                    }
                    SyncAction::Send { to } => {
                        peri_tui::sync::channel_flow::run_send_cli(&server, keystore_path, &to)
                            .await
                    }
                    SyncAction::Receive => {
                        peri_tui::sync::channel_flow::run_receive_cli(&server, keystore_path).await
                    }
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
                    PluginAction::Enable { plugin, scope } => {
                        let scope = scope.as_deref().unwrap_or("user");
                        cli_plugin::run_plugin_enable(&plugin, scope)
                    }
                    PluginAction::Disable { plugin, scope } => {
                        let scope = scope.as_deref().unwrap_or("user");
                        cli_plugin::run_plugin_disable(&plugin, scope)
                    }
                    PluginAction::Update { plugin, scope } => {
                        let scope = scope.as_deref().unwrap_or("user");
                        cli_plugin::run_plugin_update(&plugin, scope).await
                    }
                    PluginAction::Info { plugin } => cli_plugin::run_plugin_info(&plugin),
                    PluginAction::Search { query } => cli_plugin::run_plugin_search(&query),
                    PluginAction::Cleanup => {
                        let claude_dir = dirs_next::home_dir()
                            .unwrap_or_else(|| std::path::PathBuf::from("."))
                            .join(".claude");
                        cli_plugin::run_plugin_cleanup(&claude_dir).await
                    }
                    PluginAction::Marketplace { action } => match action {
                        MarketplaceAction::Add { source } => {
                            cli_plugin::run_marketplace_add(&source).await
                        }
                        MarketplaceAction::List => cli_plugin::run_marketplace_list(),
                        MarketplaceAction::Remove { name } => {
                            cli_plugin::run_marketplace_remove(&name)
                        }
                        MarketplaceAction::Update { name } => {
                            cli_plugin::run_marketplace_update(&name).await
                        }
                    },
                }
            })
        }
        Some(Commands::Web { host, port }) => {
            let mut config = peri_web_pty::config::Config::from_env();
            config.host = host;
            config.port = port;
            // peri web 模式下默认启动 peri 对话
            if config.initial_cmd.is_none() {
                config.initial_cmd = Some("peri".to_string());
            }
            let rt = build_runtime()?;
            rt.block_on(async { peri_web_pty::start_server(config).await })
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

    // 在创建 tokio runtime 之前初始化 tracing，确保 reqwest::blocking::Client
    // 的内部 runtime 与应用 runtime 完全隔离，避免嵌套 runtime drop panic。
    let _telemetry = peri_acp::telemetry::init_tracing("agent-tui");

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
#[path = "main_test.rs"]
mod tests;
