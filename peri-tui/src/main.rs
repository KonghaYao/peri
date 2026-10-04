use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};

#[cfg(not(target_os = "windows"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

mod cli_args;
mod cli_mcp_start;
mod cli_meta;
mod cli_plugin;
mod cli_print;
mod cli_tui;
#[cfg(test)]
use cli_tui::propagate_tui_result;
use cli_tui::{TuiOptions, run_tui};
mod cli_workflow;

// ─── Panic Hook（TUI 专用）───────────────────────────────────────────────────
// 实现已移至 peri_tui::kit::panic（lib 侧），AppShell mount 后重装 hook，
// 覆盖 ratatui::init() 的包装 hook——见 kit/panic.rs 模块注释。
use cli_args::{argv_requests_settings_stdin, build_runtime};
use peri_acp::host::stdio::StdioInput;
use peri_acp_types::session_resources::AccessMode;
use peri_acp_types::session_store::SessionStoreDeployment;
use peri_tui::kit::panic::init_panic_notify;

// ─── CLI 定义 ──────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "peri", version, about = "Peri AI Agent")]
struct Cli {
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
    /// 全局配置文件路径（默认 ~/.peri/settings.json）
    #[arg(long = "config-file", visible_alias = "configFile")]
    config_file: Option<PathBuf>,
    /// SQLite 会话数据库路径（默认 ~/.peri/threads/threads.db）
    #[arg(long = "db-path", visible_alias = "dbPath")]
    db_path: Option<PathBuf>,
    /// 会话存储定位：本机路径、远程 locator 或 env:<变量名>（与 --db-path 互斥）
    #[arg(long = "session-store", visible_alias = "sessionStore")]
    session_store: Option<String>,
    /// 远程会话存储的凭证来源（环境变量名，不接受 token 字面量）
    #[arg(
        long = "session-store-token-env",
        visible_alias = "sessionStoreTokenEnv"
    )]
    session_store_token_env: Option<String>,
    /// locator 形态无法唯一决定引擎时显式指定（turso / libsql）
    #[arg(long = "session-store-engine", visible_alias = "sessionStoreEngine")]
    session_store_engine: Option<String>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// 查看或显式采用本机 Machine 身份
    Machine {
        #[command(subcommand)]
        action: MachineAction,
    },
    /// 启动独立 MCP 能力进程
    #[command(name = "mcp-start")]
    McpStart {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
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
        /// Read a trusted, length-prefixed settings JSON document from stdin before ACP.
        #[arg(long = "settings-stdin")]
        settings_stdin: bool,
    },
    /// 运行 workflow CLI 子命令（read/list/validate/boundary/adlc/help）
    #[command(disable_help_flag = true)]
    Workflow {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
    /// 查询 Peri 持久化 metadata
    Meta {
        #[command(subcommand)]
        action: MetaAction,
    },
    /// 更新：从 GitHub 下载并安装最新版本
    Update,
    /// 插件管理
    Plugin {
        #[command(subcommand)]
        action: PluginAction,
    },
}

#[derive(Subcommand)]
enum MetaAction {
    /// 列出 Machine 身份及其 Workspace 路径
    Machines {
        /// JSON 输出
        #[arg(long)]
        json: bool,
    },
    /// 查询单条持久化 session metadata
    Session {
        /// Session ID（任意合法 UUID）
        session_id: String,
        /// JSON 输出
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum MachineAction {
    /// 查看候选身份；--apply 后写入身份文件并要求重启
    Adopt {
        /// 待采用的已登记 Machine ID
        target: String,
        /// 当前 Machine ID；写入时必须再次明确提供
        #[arg(long)]
        current: Option<String>,
        /// 执行身份文件替换
        #[arg(long)]
        apply: bool,
        /// 确认所有 Peri 执行进程已停止
        #[arg(long)]
        confirm_no_active_executions: bool,
    },
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
/// 路径跟随 config_path()（支持 set_global_config_path 重定向）
fn inject_env_from_settings() {
    inject_env_from_file(
        &peri_tui::config::config_path(),
        &[&["config", "env"], &["env"]],
    );
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

/// 轻量预扫描 argv，识别 `--config-file` / `--configFile`（gate 决策 Option A：
/// env 注入前先按重定向路径执行，使 `--config-file` 文件内的 `env` 字段
/// 注入进程——"该文件就是全局配置"语义）。
///
/// - 支持空格形式（下一 token 为值）与 `=` 形式（`--config-file=path`）。
/// - 下一 token 以 `-` 开头视为缺值 → 返回 None（fail-open，交给 clap 报错）。
/// - `--` 之后停止扫描；重复 flag 取最后一次（last-wins）。
/// - 非 UTF-8 的 OsString 值直接构造 PathBuf，无需 utf8 转换。
fn pre_scan_config_file(args: impl Iterator<Item = std::ffi::OsString>) -> Option<PathBuf> {
    let mut result: Option<PathBuf> = None;
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        let Some(s) = arg.to_str() else {
            // 非 UTF-8 token 无法匹配 flag 前缀，跳过（fail-open）
            continue;
        };
        if s == "--" {
            break;
        }
        if let Some(value) = s.strip_prefix("--config-file=") {
            result = Some(PathBuf::from(value));
        } else if let Some(value) = s.strip_prefix("--configFile=") {
            result = Some(PathBuf::from(value));
        } else if s == "--config-file" || s == "--configFile" {
            match args.peek() {
                Some(next) if next.to_str().is_some_and(|n| n.starts_with('-')) => {
                    // 下一 token 是 option-like → 缺值，fail-open 交给 clap 报错
                    return None;
                }
                Some(next) => {
                    result = Some(PathBuf::from(next));
                    args.next();
                }
                None => return None,
            }
        }
    }
    result
}

fn validate_cli(cli: &Cli) -> std::result::Result<(), &'static str> {
    if matches!(
        cli.command,
        Some(Commands::Acp {
            settings_stdin: true,
            ..
        })
    ) && cli.config_file.is_some()
    {
        return Err("--settings-stdin cannot be combined with --config-file");
    }
    if cli.print.is_some() && cli.command.is_some() {
        return Err("--print cannot be used with a subcommand");
    }
    if cli.db_path.is_some() && cli.session_store.is_some() {
        // 两个定位入口没有隐式覆盖顺序：同时出现直接是参数错误（早于任何 I/O）。
        return Err("--db-path cannot be combined with --session-store");
    }
    if matches!(cli.command, Some(Commands::Meta { .. }))
        && (cli.print.is_some()
            || cli.output_format.is_some()
            || cli.max_turns.is_some()
            || cli.bare
            || cli.permission_mode.is_some()
            || cli.skip_permissions
            || cli.model.is_some()
            || cli.effort.is_some()
            || cli.cont
            || cli.resume.is_some()
            || cli.session_id.is_some()
            || cli.session_name.is_some()
            || cli.no_session_persistence
            || cli.allowed_tools.is_some()
            || cli.disallowed_tools.is_some()
            || cli.settings.is_some()
            || cli.config_file.is_some())
    {
        return Err(
            "meta only accepts session store options (--db-path / --session-store*) and session --json",
        );
    }
    Ok(())
}

/// 部署参数 → 中性定位描述（D-04）。
///
/// 这里只做 CLI grammar 归一（本机路径 / locator 原文 / 默认 + 可选引擎与凭证来源），
/// **不解析 locator、不读环境变量、不判断后端**：那些纯解析与后端选择只发生在资源
/// 装配层（`Resources::open_deployment`）。两个定位入口同时出现是参数错误，不设
/// 隐式覆盖顺序。
fn session_store_deployment(
    cli: &Cli,
    access: AccessMode,
) -> std::result::Result<SessionStoreDeployment, &'static str> {
    let deployment = match (cli.db_path.as_ref(), cli.session_store.as_deref()) {
        (Some(_), Some(_)) => return Err("--db-path cannot be combined with --session-store"),
        (Some(path), None) => SessionStoreDeployment::local_path(path.clone()),
        (None, Some(raw)) => SessionStoreDeployment::from_locator(raw),
        (None, None) => SessionStoreDeployment::default_local(),
    };
    let deployment = match cli.session_store_engine.as_deref() {
        Some(engine) => deployment.with_engine(engine),
        None => deployment,
    };
    let deployment = match cli.session_store_token_env.as_deref() {
        Some(name) => deployment.with_credential_env(name),
        None => deployment,
    };
    Ok(deployment.with_access(access))
}

#[derive(Clone, Copy)]
enum TopLevelOptionShape {
    Flag,
    RequiredValue,
    OptionalValue,
}

fn top_level_option_shape(arg: &OsStr) -> Option<TopLevelOptionShape> {
    use clap::ArgAction;

    let raw = arg.to_str()?;
    let name = raw.split_once('=').map_or(raw, |(name, _)| name);
    let command = Cli::command();
    command.get_arguments().find_map(|declared| {
        let long_match = declared
            .get_long()
            .is_some_and(|long| name == format!("--{long}"))
            || declared.get_all_aliases().is_some_and(|aliases| {
                aliases
                    .into_iter()
                    .any(|alias| name == format!("--{alias}"))
            });
        let short_match = declared
            .get_short()
            .is_some_and(|short| name == format!("-{short}"))
            || declared.get_all_short_aliases().is_some_and(|aliases| {
                aliases.into_iter().any(|alias| name == format!("-{alias}"))
            });
        if !long_match && !short_match {
            return None;
        }
        if raw.contains('=') {
            return Some(TopLevelOptionShape::Flag);
        }
        if matches!(
            declared.get_action(),
            ArgAction::SetTrue
                | ArgAction::SetFalse
                | ArgAction::Count
                | ArgAction::Help
                | ArgAction::HelpShort
                | ArgAction::HelpLong
                | ArgAction::Version
        ) {
            return Some(TopLevelOptionShape::Flag);
        }
        match declared.get_num_args() {
            Some(range) if range.min_values() == 0 => Some(TopLevelOptionShape::OptionalValue),
            _ => Some(TopLevelOptionShape::RequiredValue),
        }
    })
}

/// 按已接受的顶层 grammar走到 subcommand槽位。已知 option严格消费 `Cli` 声明的值；
/// malformed option-like token只消费自身，使后续明确的 `meta` shape保持可见，同时
/// 不把任意 option value或普通 positional中的 `meta`误判为 command。
fn malformed_argv_requests_meta(args: &[OsString]) -> bool {
    let mut index = 1;
    while index < args.len() {
        let arg = &args[index];
        if arg == "meta" {
            return true;
        }
        if arg == "--" {
            return args.get(index + 1).is_some_and(|next| next == "meta");
        }
        if ["-h", "--help", "-V", "--version"]
            .iter()
            .any(|terminal| arg == terminal)
        {
            return false;
        }

        match top_level_option_shape(arg) {
            Some(TopLevelOptionShape::Flag) => index += 1,
            Some(TopLevelOptionShape::RequiredValue) => {
                index += 1;
                if args
                    .get(index)
                    .is_some_and(|value| !value.to_string_lossy().starts_with('-'))
                {
                    index += 1;
                }
            }
            Some(TopLevelOptionShape::OptionalValue) => {
                index += 1;
                if args
                    .get(index)
                    .is_some_and(|value| !value.to_string_lossy().starts_with('-'))
                {
                    if args[index] == "meta"
                        && args.get(index + 1).is_some_and(|next| next == "session")
                    {
                        return true;
                    }
                    index += 1;
                }
            }
            None if arg.to_string_lossy().starts_with('-') => index += 1,
            None => return false,
        }
    }
    false
}

fn argv_requests_meta(args: &[OsString]) -> bool {
    match Cli::try_parse_from(args) {
        Ok(cli) => matches!(cli.command, Some(Commands::Meta { .. })),
        Err(_) => malformed_argv_requests_meta(args),
    }
}

fn argv_requests_meta_json(args: &[OsString]) -> bool {
    args.iter().any(|arg| arg == OsStr::new("--json"))
}

fn emit_meta_outcome(outcome: cli_meta::MetaCommandOutcome) -> Result<()> {
    if let Some(output) = outcome.stdout {
        print!("{output}");
    }
    if let Some(error) = outcome.stderr {
        eprint!("{error}");
    }
    if outcome.exit_code != 0 {
        std::process::exit(i32::from(outcome.exit_code));
    }
    Ok(())
}

fn try_run_meta_before_configuration(args: &[OsString]) -> Option<Result<()>> {
    if !argv_requests_meta(args) {
        return None;
    }
    let json = argv_requests_meta_json(args);
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(_) => return Some(emit_meta_outcome(cli_meta::invalid_argument_outcome(json))),
    };
    if validate_cli(&cli).is_err() {
        return Some(emit_meta_outcome(cli_meta::invalid_argument_outcome(json)));
    }
    // meta 是显式只读入口：只读意图 + 同一份定位描述，先 UUID/grammar 再打开；
    // 不加载 provider/MCP/Agent，也不新建本机执行登记。
    let deployment = match session_store_deployment(&cli, AccessMode::ReadOnly) {
        Ok(deployment) => deployment,
        Err(_) => return Some(emit_meta_outcome(cli_meta::invalid_argument_outcome(json))),
    };
    let Some(Commands::Meta { action }) = cli.command else {
        return Some(emit_meta_outcome(cli_meta::invalid_argument_outcome(json)));
    };
    let runtime = match build_runtime() {
        Ok(runtime) => runtime,
        Err(_) => return Some(emit_meta_outcome(cli_meta::internal_error_outcome(json))),
    };
    let outcome = match action {
        MetaAction::Session { session_id, json } => {
            runtime.block_on(cli_meta::run_meta_session(deployment, session_id, json))
        }
        MetaAction::Machines { json } => {
            runtime.block_on(cli_meta::run_meta_machines(deployment, json))
        }
    };
    Some(emit_meta_outcome(outcome))
}

// ─── 入口 ──────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let args: Vec<OsString> = std::env::args_os().collect();
    if args.get(1).is_some_and(|arg| arg == "mcp-start") {
        return cli_mcp_start::run(&args[2..]);
    }
    if args.iter().skip(2).any(|arg| arg == "mcp-start")
        && Cli::try_parse_from(&args)
            .is_ok_and(|cli| matches!(cli.command, Some(Commands::McpStart { .. })))
    {
        anyhow::bail!("mcp-start must be the first argument");
    }
    if cli_workflow::argv_requests_workflow(&args) {
        return cli_workflow::run_before_configuration(&args);
    }
    if argv_requests_meta(&args) {
        return try_run_meta_before_configuration(&args)
            .expect("Meta argv detection and dispatch must agree");
    }

    // Set jemalloc MALLOC_CONF env vars before ordinary runtime startup.
    peri_tui::alloc_config::init_alloc_conf();

    let settings_stdin = argv_requests_settings_stdin(&args);

    // 预扫描 argv 重定向全局配置路径，必须在 env 注入之前（gate 决策 Option A）：
    // --config-file 文件内的 env 字段需注入进程。fail-open：扫描不到时保持
    // 默认路径，后续 clap 解析报错兜底。
    if !settings_stdin {
        peri_tui::config::set_global_config_path(pre_scan_config_file(
            args.iter().skip(1).cloned(),
        ));
    }

    // 最先注入环境变量（进程环境变量优先）
    // 优先级：进程环境 > 项目本地配置 > Peri 全局配置 > Claude Code 配置
    // 项目本地配置（./.peri/settings.json），项目覆盖全局
    if !settings_stdin {
        if let Some(path) = peri_tui::config::workspace_config_path() {
            inject_env_from_file(&path, &[&["config", "env"], &["env"]]);
        }
        inject_env_from_settings(); // ~/.peri/settings.json
        inject_env_from_claude_settings(); // ~/.claude/settings.json
    }

    let cli = Cli::parse();
    if let Err(message) = validate_cli(&cli) {
        Cli::command()
            .error(clap::error::ErrorKind::ArgumentConflict, message)
            .exit();
    }

    // 部署参数在这里归一一次（D-04）：之后 TUI / print / ACP stdio 共享同一个定位描述，
    // 各入口不再各自解释存储位置，恢复会话也不会重新解析出另一个存储。
    let session_store = match session_store_deployment(&cli, AccessMode::ReadWrite) {
        Ok(deployment) => deployment,
        Err(message) => Cli::command()
            .error(clap::error::ErrorKind::ArgumentConflict, message)
            .exit(),
    };

    // 以 clap 解析结果为准（幂等；prescan 与 clap 同源 argv，二者一致）
    peri_tui::config::set_global_config_path(if settings_stdin {
        None
    } else {
        cli.config_file.clone()
    });

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
            session_store,
        ));
    }

    match cli.command {
        Some(Commands::Machine { action }) => {
            let rt = build_runtime()?;
            rt.block_on(cli_meta::run_machine_adopt(
                action,
                session_store.with_access(AccessMode::ReadOnly),
            ))
        }
        Some(Commands::McpStart { .. }) => anyhow::bail!("mcp-start must be the first argument"),
        None => match run_tui(TuiOptions {
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
            session_store,
        }) {
            Ok(()) => Ok(()),
            Err(_) => std::process::exit(1),
        },
        Some(Commands::Acp {
            cwd,
            model: _,
            agent: _,
            settings_stdin,
        }) => {
            // 限制 worker 数（默认=CPU 核数，18 核=72MB 栈空间浪费），4 MB stack
            let rt = build_runtime()?;
            rt.block_on(async {
                // stdio host 位于 ACP 层（部署装配点），cli 仅作为启动入口调用；
                // thread 存储与 middlewares 具体实现（CronScheduler / McpClientPool /
                // 插件数据等）由 host 装配面（assemble_server_config）内部构造
                // （§0 依赖方向，docs/top-level.md §7/§8）；cli 只提供协议面输入。
                peri_acp::host::stdio::run_acp_stdio(StdioInput {
                    cwd,
                    settings_stdin,
                    permission_mode: peri_acp_types::permission::SharedPermissionMode::new(
                        peri_acp_types::permission::PermissionMode::Bypass,
                    ),
                    session_store,
                })
                .await
            })
        }
        Some(Commands::Meta { .. }) => unreachable!("Meta 在配置读取前完成 dispatch"),
        Some(Commands::Workflow { .. }) => {
            unreachable!("Workflow 在配置读取前完成 dispatch")
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
    }
}

// ─── TUI 模式 ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod cli_integration_test;

#[cfg(test)]
#[path = "main_test.rs"]
mod tests;
