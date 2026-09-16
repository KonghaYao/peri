//! 单进程本机 MCP server 入口（`local-mcp-server` bin）。
//!
//! 职责边界：解析并校验配置 → 初始化**只写 stderr** 的诊断 → 打开工作区根 →
//! 构造进程内工具集与执行缝 → 注册任务表与工具路由 → 交给传输层 → 关闭时按所有权
//! 回收（取消在跑任务、终止任务进程组）。本文件不做工具语义、不拼任何容器命令。
//!
//! ## 启动顺序为什么是这个顺序
//!
//! 1. 配置校验（退出码 [`exit_code::USAGE`]）：字段错误在接触文件系统之前就拒绝。
//! 2. 工作区根解析（同一退出码）：`std::fs::canonicalize` 解析真实路径后打开为
//!    capability 根——**宿主路径单表示**，七工具与产物落盘都用它。
//! 3. 进程内执行内核（同一退出码）：七工具的唯一执行面，日志目录必须在根内。
//! 4. 传输层：stdio 或 Streamable HTTP；两者消费**同一个**执行器，
//!    因此两种传输的工具语义与错误映射完全一致。
//!
//! ## 执行形态的诚实边界（D-003）
//!
//! 七工具在本进程内以**当前用户权限**执行：没有容器、没有 worker 子进程、没有沙箱，
//! 也不提供网络或进程隔离。工作区根约束的是文件类工具的路径解析，`Bash` 只是以该根
//! 作为 cwd，其命令能力与启动本进程的用户相同——工作区根是**能力边界，不是安全边界**。
//!
//! ## 关闭语义
//!
//! - stdio：stdin EOF（或信号）→ 停止接单 → `TaskRegistry::close`（对在跑任务
//!   进程组 TERM→KILL 并回收记录，含后台 `Bash` 任务）。
//! - HTTP：`HttpServer::shutdown`（取消会话 + 连接优雅关闭 + 宽限等待）后同上。
//!
//! 执行的唯一持有者是任务注册表：进程退出时只需关闭它一次，不存在第二个需要
//! 协调的执行面。

use std::sync::Arc;

use clap::Parser;
use local_mcp_server::capability::{RequestedPath, RootDir};
use local_mcp_server::config::{Config, ServerCli, TransportKind};
use local_mcp_server::error::exit_code;
use local_mcp_server::mcp::identity::ConnectionIdentity;
use local_mcp_server::mcp::resources::TaskStatusSource;
use local_mcp_server::mcp::server::SandboxServer;
use local_mcp_server::observe;
use local_mcp_server::output::DEFAULT_ARTIFACT_DIR;
use local_mcp_server::runtime::{InProcessExecutor, LOG_SUBDIR};
use local_mcp_server::tasks::log::DirOutputPersist;
use local_mcp_server::tasks::{BashTaskConfig, BashTasks};
use local_mcp_server::tasks::{SystemClock, TaskRegistry, TaskRegistryConfig};
use local_mcp_server::transport;

fn main() -> ! {
    observe::init_diagnostics();

    let cli = ServerCli::parse();
    let config = match cli.into_config() {
        Ok(config) => config,
        Err(err) => {
            // 配置错误属于启动期错误：只报告字段与原因，不回显任何 token 值
            // （配置结构里根本没有存放 token 值的字段）。
            tracing::error!(error = %err, "invalid configuration");
            std::process::exit(exit_code::USAGE);
        }
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::error!(error = %error, "cannot build tokio runtime");
            std::process::exit(exit_code::INTERNAL);
        }
    };

    let exit_code = runtime.block_on(async move {
        match run(config).await {
            Ok(()) => exit_code::OK,
            Err(failure) => {
                tracing::error!(code = failure.code, error = %failure.message, "server 退出");
                failure.code
            }
        }
    });
    // Tokio 的 stdio reader 可能持有无法被 runtime drop 中断的阻塞读；`run` 已关闭所有
    // owner 资源，此处直接以最终退出码结束，避免该 reader 延迟外部可观察的退出。
    std::process::exit(exit_code);
}

/// 启动期失败：退出码 + 已脱敏的说明。
struct StartupFailure {
    code: i32,
    message: String,
}

impl StartupFailure {
    fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

async fn run(config: Config) -> Result<(), StartupFailure> {
    tracing::info!(
        transport = ?config.transport,
        loopback = config.bind_is_loopback(),
        workspace = %config.workspace.root.display(),
        "local-mcp-server 配置校验通过（单进程本机执行；无沙箱/容器隔离）"
    );

    // ① 工作区根：解析真实路径后作为**唯一**能力边界（文件类工具限根、Bash 的 cwd）。
    //    先 canonicalize 再使用，保证工具集、落盘器与任务注册表看到同一个字符串
    //    （宿主路径单表示：D-003 起不存在第二种路径表示，也没有映射表）；
    //    符号链接本身在这一步被解析，根内逐组件的符号链接防护仍在 capability 层。
    let workspace = std::fs::canonicalize(&config.workspace.root).map_err(|error| {
        StartupFailure::new(
            exit_code::USAGE,
            format!(
                "工作区根 {} 无法解析为真实路径：{error}",
                config.workspace.root.display()
            ),
        )
    })?;
    if !workspace.is_dir() {
        return Err(StartupFailure::new(
            exit_code::USAGE,
            format!("工作区根 {} 不是目录", workspace.display()),
        ));
    }
    tracing::info!(workspace = %workspace.display(), "工作区根就绪（能力边界；非安全边界）");

    // ② 工作区根作为 capability 根打开：文件类工具的授权判定与
    //    Bash 的日志/产物落盘共用同一套判定（F2）。
    let capability = Arc::new(RootDir::open(&workspace).map_err(|error| {
        StartupFailure::new(
            exit_code::USAGE,
            format!("工作区无法作为工具授权根打开：{}", error.message()),
        )
    })?);
    // ③ 回收上一轮残留：日志与截断产物都在工作区根内，运行期不清理就会累积。
    let pruned = prune_stale_artifacts(&workspace);
    if pruned > 0 {
        tracing::info!(files = pruned, "已回收上一轮产物");
    }

    // ④ Bash 进程执行器：**唯一** spawn `bash -c` 并持有子进程的地方。
    //    cwd 与日志目录都取宿主工作区根（日志落在根内，`Read` 才读得到）。
    let bash = Arc::new(BashTasks::new(
        BashTaskConfig::new(workspace.clone(), workspace.join(LOG_SUBDIR))
            .with_capability(Arc::clone(&capability), LOG_SUBDIR),
    ));

    // ⑤ 唯一任务注册表：`Bash` 的 owner 绑定、容量、TTL、停止与关闭清理都在这里，
    //    它直接持有上一步的进程句柄（不存在第二张任务表）。
    //
    //    截断落盘同样走 capability 校验：产物写在工作区内的私有目录，
    //    路径解析与七工具一套判定。
    let persist = Arc::new(DirOutputPersist::rooted(
        Arc::clone(&capability),
        DEFAULT_ARTIFACT_DIR,
    ));
    // 注册表配置来自 `Config::tasks`：CLI 的 `--task-ttl-secs` / `--task-retention`
    // 与并发上限在这里**真正生效**（否则它们只是被校验、被忽略的空旋钮）。
    let mut registry_config = TaskRegistryConfig::new(persist);
    registry_config.terminal_ttl = std::time::Duration::from_secs(config.tasks.ttl_secs);
    registry_config.max_terminal_entries = config.tasks.retention;
    registry_config.shell_limit = config.tasks.max_concurrent_shell_tasks;
    let registry = TaskRegistry::new(Arc::clone(&bash), Arc::new(SystemClock), registry_config);

    // ⑥ 进程内执行内核：七工具的唯一执行面（无子进程、无管道、无帧）。
    let executor: Arc<dyn local_mcp_server::wire::ToolExecutor> = Arc::new(
        InProcessExecutor::new(workspace.clone(), Arc::clone(&registry)).map_err(|error| {
            StartupFailure::new(
                exit_code::USAGE,
                format!("工作区无法作为工具授权根打开：{}", error.message()),
            )
        })?,
    );
    // 任务资源来源：与七工具共用同一个注册表，因此句柄所有权完全一致。
    let tasks: Arc<dyn TaskStatusSource> = registry.clone();

    // ⑦ 传输层（两种传输消费同一个 core 与同一个 executor）。
    let transport_result = match config.transport {
        TransportKind::Stdio => {
            serve_stdio(&config, Arc::clone(&executor), Arc::clone(&tasks)).await
        }
        TransportKind::Http => serve_http(&config, Arc::clone(&executor), Arc::clone(&tasks)).await,
    };

    // ⑧ 按所有权回收：唯一任务表停止在跑任务（进程组 TERM→KILL）并回收记录。
    if let Err(error) = registry.close().await {
        tracing::error!(error = %error, "任务注册表关闭失败");
        if transport_result.is_ok() {
            return Err(StartupFailure::new(
                exit_code::INTERNAL,
                format!("任务注册表关闭失败：{error}"),
            ));
        }
    }

    transport_result
}

/// stdio 传输：EOF 即正常结束（退出码 0）。
async fn serve_stdio(
    config: &Config,
    executor: Arc<dyn local_mcp_server::wire::ToolExecutor>,
    tasks: Arc<dyn TaskStatusSource>,
) -> Result<(), StartupFailure> {
    // 非回环绑定对 stdio 没有意义：这里不额外校验，配置校验已覆盖 HTTP 暴露面。
    let _ = config;
    let server = SandboxServer::for_stdio(executor, Some(tasks));
    match transport::stdio::serve_stdio(server).await {
        Ok(reason) => {
            tracing::info!(reason = ?reason, "stdio 传输结束");
            Ok(())
        }
        Err(error) => Err(StartupFailure::new(
            exit_code::TRANSPORT_FAILED,
            format!("stdio 传输失败：{error}"),
        )),
    }
}

/// Streamable HTTP 传输：每个会话/请求构造一个 handler，身份由认证层逐请求注入。
async fn serve_http(
    config: &Config,
    executor: Arc<dyn local_mcp_server::wire::ToolExecutor>,
    tasks: Arc<dyn TaskStatusSource>,
) -> Result<(), StartupFailure> {
    let factory_executor = Arc::clone(&executor);
    let factory_tasks = Arc::clone(&tasks);
    let factory = move || {
        // HTTP 连接身份在**请求**里（认证层注入的可信主体）；这里的兜底身份永远不匹配
        // 任何真实任务 owner，因此"没有注入"只会得到空结果，不会越权。
        let unbound = ConnectionIdentity::new("http-unauthenticated", "http-unbound");
        Ok::<SandboxServer, std::io::Error>(SandboxServer::new(
            Arc::clone(&factory_executor),
            unbound,
            Some(Arc::clone(&factory_tasks)),
        ))
    };

    let server = match transport::http::serve_http(config, factory).await {
        Ok(server) => server,
        Err(error) => {
            let code = match &error {
                transport::http::HttpTransportError::Auth { .. }
                | transport::http::HttpTransportError::NonLoopbackBind => exit_code::USAGE,
                _ => exit_code::TRANSPORT_FAILED,
            };
            return Err(StartupFailure::new(
                code,
                format!("HTTP 传输启动失败：{error}"),
            ));
        }
    };
    tracing::info!(addr = %server.local_addr(), "Streamable HTTP 已就绪");

    // 进程生命周期 = 服务生命周期：两种 Unix 终止信号都由 transport 统一收口。
    let shutdown_result = server.wait_for_shutdown().await;
    server.shutdown().await;
    if let Err(error) = shutdown_result {
        return Err(StartupFailure::new(
            exit_code::TRANSPORT_FAILED,
            format!("HTTP 传输信号关闭失败：{error}"),
        ));
    }
    tracing::info!("HTTP 传输结束");
    Ok(())
}

/// 回收工作区里上一轮运行留下的产物（日志与截断输出）。
///
/// 只删除 `.local-mcp/logs` 与 `.local-mcp/artifacts` 两个**本产品私有**目录下的
/// 普通文件，不递归、不跟随符号链接、不触碰用户其它文件；返回删除数量。
///
/// 解析与删除都经 capability 层（F2 同类修复）：目录本身若被换成符号链接，
/// [`RootDir::open_dir`] 直接拒绝，绝不会顺着链接删到工作区之外。
fn prune_stale_artifacts(workspace: &std::path::Path) -> usize {
    let Ok(root) = RootDir::open(workspace) else {
        return 0;
    };
    let mut removed = 0usize;
    for subdir in [LOG_SUBDIR, DEFAULT_ARTIFACT_DIR] {
        let Ok(requested) = RequestedPath::parse(subdir, root.base()) else {
            continue;
        };
        // 符号链接祖先（或越界目标）在这里被拒绝 → 跳过该目录，不做任何删除。
        let Ok(dir) = root.open_dir(&requested) else {
            continue;
        };
        let Ok(entries) = dir.entries() else {
            continue;
        };
        for entry in entries {
            // 只删普通文件：目录与符号链接一律跳过（`lstat` 不跟随链接）。
            if !entry.metadata.is_file() {
                continue;
            }
            let Ok(child_requested) =
                RequestedPath::parse(&format!("{subdir}/{}", entry.name), root.base())
            else {
                continue;
            };
            let Ok(child) = root.resolve(&child_requested) else {
                continue;
            };
            // 解析后再复核一次：末段必须仍是普通文件（防解析与删除之间的交换）。
            if !child.lstat().map(|meta| meta.is_file()).unwrap_or(false) {
                continue;
            }
            if child.unlink().is_ok() {
                removed += 1;
            }
        }
    }
    removed
}
