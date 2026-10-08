//! ACP Stdio 模式：通过 stdin/stdout JSON-RPC 与 IDE client 通信。
//!
//! 批 3（acp-host-unify）：stdio 业务处理整体切换到统一宿主
//! [`super::run_acp_server`]——删除 typed handler 层（`stdio/session/*`）与
//! `StdioContext`，[`StdioTransport`] 作为 [`AcpTransport`] 多态实现接入。
//! 装配（`assemble_server_config`，与 TUI/print 同源）收拢在
//! [`super::assemble`]，本模块只做部署装配点职责：协议面输入 → 装配 →
//! transport 挂载；无身份的 legacy cancel 不具有控制权限。
//!
//! stdio host 位于 ACP 层（部署装配点，`docs/top-level.md` §7/§19）；外部
//! 系统通道（会话资源门面）由部署单元（cli）打开后经 `session_resources` 注入，
//! ACP 层不直接依赖 Resources（§0 依赖方向）。

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use parking_lot::RwLock;
use peri_acp_types::permission::SharedPermissionMode;
use peri_acp_types::session_store::SessionStoreDeployment;
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::provider::LlmProvider;
use crate::transport::stdio::StdioTransport;
use crate::transport::AcpTransport;

/// stdio 宿主装配输入（M-TUI 收口：middlewares 具体实现由装配面内部
/// 构造——「ACP Host = 部署单元」；cli 只提供协议面输入）。
pub struct StdioInput {
    pub cwd: String,
    pub permission_mode: Arc<SharedPermissionMode>,
    /// 会话存储的部署参数（定位 + 凭证来源 + 访问意图）。装配时一次性打开；
    /// ACP 层不解释 locator、不读凭证值、不选择后端。
    pub session_store: SessionStoreDeployment,
    /// Trusted launcher sends a length-prefixed settings document before ACP begins.
    pub settings_stdin: bool,
}

const MAX_BOOTSTRAP_SETTINGS_BYTES: usize = 1024 * 1024;

async fn read_bootstrap_settings<R: AsyncRead + Unpin>(reader: &mut R) -> anyhow::Result<String> {
    let mut length = [0_u8; 4];
    reader.read_exact(&mut length).await?;
    let size = u32::from_be_bytes(length) as usize;
    anyhow::ensure!(
        (1..=MAX_BOOTSTRAP_SETTINGS_BYTES).contains(&size),
        "settings bootstrap size is invalid"
    );
    let mut bytes = vec![0_u8; size];
    reader.read_exact(&mut bytes).await?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("settings bootstrap must be valid JSON"))?;
    anyhow::ensure!(
        value.is_object(),
        "settings bootstrap must be a JSON object"
    );
    Ok(serde_json::to_string(&value)?)
}

/// 安装 ACP 宿主的 panic hook（**与 `peri-tui/src/kit/panic.rs:47-55` 同源语义**）。
///
/// 依赖方向为 `peri-tui` → `peri-acp`（见两侧 `Cargo.toml`），peri-acp 不能反向
/// 复用 TUI 侧实现，故本地按同一语义实现：payload + 位置 + backtrace 经
/// `tracing::error!` 结构化记录，不写 stderr。ACP 客户端（编辑器/IDE）消费的是
/// 日志文件而非宿主 stderr，默认 hook 的 panic 输出对客户端不可见。
///
/// 必须在 `init_tracing` **之后**调用：`tracing::error!` 在没有 subscriber 时
/// 不产生任何输出。修改 TUI 侧格式时须同步本处，避免语义漂移。
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|panic_info| {
        let msg = format_panic_message(panic_info);
        tracing::error!("thread panicked at {}", msg);
    }));
}

/// 格式化 panic 信息为可读字符串（消息 + 位置 + backtrace）。
///
/// 与 `peri-tui/src/kit/panic.rs::format_panic_message` 同源（依赖方向见上）。
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

/// 启动 ACP stdio 宿主（批 3：统一宿主 `run_acp_server` 接管全部业务处理）。
///
/// 装配输入（cron/MCP 池/工具检索索引/插件数据等具体实现）由部署装配点
/// （cli 白名单文件，见 `peri-tui/src/main.rs`）构造后经 [`StdioInput`] 注入；
/// ACP 层只持端口接口（3.0 批 2 波 2，§0 依赖方向）。
pub async fn run_acp_stdio(input: StdioInput) -> anyhow::Result<()> {
    let _telemetry = peri_agent::telemetry::init_tracing("peri-acp");
    // ACP 模式下 panic 必须落日志：客户端（编辑器/IDE）读日志文件而非宿主
    // stderr，默认 hook 的输出对客户端不可见。装在 `init_tracing` **之后**——
    // 若在 subscriber 就绪前安装，`init_tracing` 自身的启动失败 panic
    // （日志目录不可写）反而会从 stderr 变成无声失败（TUI 路径同此顺序，
    // 见 `cli_tui.rs`：init_tracing → init_panic_notify）。
    install_panic_hook();
    let mut stdin = tokio::io::stdin();
    let injected_settings = if input.settings_stdin {
        Some(read_bootstrap_settings(&mut stdin).await?)
    } else {
        None
    };
    let cfg = assemble_stdio_config(input, injected_settings.as_deref()).await?;
    let sessions: super::SharedSessions = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let transport = StdioTransport::from_reader_writer(stdin, tokio::io::stdout());
    let transport: Arc<dyn AcpTransport> = Arc::new(transport);
    super::run_acp_server_with_sessions(transport, cfg, sessions).await;
    Ok(())
}

/// 批 3 并入 `run_acp_stdio`（原 `host/stdio/init.rs::init_stdio_context`，已删）。
///
/// 执行顺序：cwd 解析（canonicalize）→ config/provider → thread store →
/// config source → `assemble_server_config`（与 TUI `launch.rs` 同构的
/// **统一装配**，middlewares/插件/Langfuse/session_manager 全数由
/// [`crate::host::assemble::assemble_server_config`] 构造；stdio 无 bare 语义、
/// 无 cron tick）。
fn load_stdio_config_source(
    cwd: &std::path::Path,
    global_path: PathBuf,
) -> crate::provider::ConfigSource {
    crate::provider::ConfigSource::load_at(cwd, global_path.clone())
        .unwrap_or_else(|_| crate::provider::ConfigSource::load_at_lenient(cwd, global_path))
}

async fn assemble_stdio_config(
    input: StdioInput,
    injected_settings: Option<&str>,
) -> anyhow::Result<super::AcpServerConfig> {
    // 解析工作目录
    let cwd = std::path::Path::new(&input.cwd)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(&input.cwd))
        .to_string_lossy()
        .to_string();

    // 加载配置。Provider 与后续 ConfigSource 必须共享同一个 canonical cwd，
    // 避免从进程 cwd 启动 `peri acp --cwd <other-workspace>` 时配置来源错位。
    let config_source = Arc::new(match injected_settings {
        Some(settings) => crate::provider::ConfigSource::load_injected_at(
            std::path::Path::new(&cwd),
            crate::provider::config_path(),
            settings.to_owned(),
        )?,
        None => {
            load_stdio_config_source(std::path::Path::new(&cwd), crate::provider::config_path())
        }
    });
    let peri_config = config_source.loaded_merged();
    let provider = LlmProvider::from_source(&config_source).ok_or_else(|| {
        if injected_settings.is_some() {
            anyhow::anyhow!("No LLM provider configured in injected settings")
        } else {
            anyhow::anyhow!("No LLM provider configured. Configure ~/.peri/settings.json and, if selecting by environment, set both MODEL_PROVIDER and MODEL_TYPE")
        }
    })?;

    tracing::info!(
        provider = %provider.display_name(),
        model = %provider.model_name(),
        cwd = %cwd,
        "ACP stdio mode starting"
    );

    let StdioInput {
        cwd: input_cwd,
        permission_mode,
        session_store,
        settings_stdin: _,
    } = input;
    let _ = input_cwd;

    // 存储经 peri-agent 工厂构造（§0：ACP 层不直接依赖 Resources；M-res 收口——
    // 存储实例化点归 Agent 层声明边）。定位参数按部署描述解析一次、打开一次：
    // 协议面、Agent transcript/subagent 与 Controller 共用同一个库句柄与同一份
    // owner 登记；后续恢复会话不重新解析存储。
    let (session_resources, session_store_shutdown) =
        peri_agent::resources::open_session_resources_deployment(&session_store)
            .await
            .map_err(|e| anyhow::anyhow!("无法初始化 Resources 层: {e}"))?;

    // 配置源已在 Provider 选择前按 canonical cwd 冻结，后续读写与装配复用
    // 同一实例，保证配置 provenance 一致。

    // ── M-TUI 收口：middlewares 具体实现（CronScheduler / McpClientPool /
    //    ToolSearchIndex / AgentCatalogProvider / PluginManager / SettingsHooksLoader /
    //    插件聚合数据 / Langfuse / SessionManager）由 host 装配面统一构造
    //    （与 TUI/print 的 `assemble_server_config` 同源）；stdio 无 bare
    //    语义、无 cron tick。MCP 初始化（run_initialize）、孤儿插件清理即
    //    在 assemble 内部完成，此处不再重复 spawn。──
    let apps_enabled = std::env::var_os(peri_acp_types::mcp_apps::MCP_APPS_ENV).is_some();
    let mut cfg = crate::host::assemble::assemble_server_config_with_mcp_apps(
        crate::host::assemble::HostAssemblyInput {
            provider,
            peri_config: Arc::new(RwLock::new(peri_config)),
            config_source,
            permission_mode,
            session_resources,
            workspace_id: None,
            // 部署关闭权留在宿主装配里：stdio 宿主的任务排空之后由它关闭会话存储。
            session_store_shutdown: Some(session_store_shutdown),
            cwd: cwd.clone(),
            bare: false,
            drive_cron_tick: false,
            // stdio 顶层装配不构造 builtin 上下文（`session_resources = false`）：
            // session 级输入（AW3-11）由每 session 的 `SessionEnvironment::assemble`
            // 产生，此处恒为 `None`。
            workspace_input: None,
            workspace_bash_default_run_in_background: false,
            // 资源面输入与 session 级输入同源（见上）：顶层装配无会话消费者，恒为
            // `None`（资源面未接线），不改变任何既有行为。
            workspace_resources: None,
            // 同理：A24 关闭集是**会话级** frozen policy 的投影，顶层装配无会话上下文，
            // 恒为空集（订阅建立门在本层无对象——顶层不建 MCP 池）。
            builtin_closed: Default::default(),
            // 宿主技能面关闭位与关闭集同源（会话级 frozen policy 的投影）：顶层装配
            // 无会话上下文、不建池，恒为假（发现管线在本层无对象）。
            skills_face_closed: false,
            plugin_face_closed: false,
            // host 级装配：无准备路径提供的插件聚合，按既有语义自行加载。
            prepared_plugins: None,
            session_mcp_servers: None,
        },
        apps_enabled,
    )
    .await;

    // stdio 部署过滤 rewind/clear（IDE 客户端自管理；不拦截，fall-through 进模型）。
    cfg.stdio_command_filter = true;

    Ok(cfg)
}

#[cfg(test)]
#[path = "run_server_integration_test.rs"]
mod run_server_integration_tests;

#[cfg(test)]
#[path = "settings_bootstrap_test.rs"]
mod settings_bootstrap_tests;

/// panic hook 的日志出口验收（P1-3）：ACP 宿主 panic 必须经 `tracing::error!`
/// 可见，而不是默认 hook 的 stderr。
#[cfg(test)]
mod panic_hook_tests {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    use serial_test::serial;

    use super::install_panic_hook;

    #[derive(Clone, Default)]
    struct LogBuffer(Arc<Mutex<Vec<u8>>>);

    impl Write for LogBuffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl LogBuffer {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }

        fn subscriber(&self) -> impl tracing::Subscriber + Send + Sync {
            let buffer = self.clone();
            tracing_subscriber::fmt()
                .without_time()
                .with_ansi(false)
                .with_max_level(tracing::Level::ERROR)
                .with_writer(move || buffer.clone())
                .finish()
        }
    }

    /// 仓库没有现成的 tracing 捕获工具，这里内联最小 `MakeWriter`（与
    /// `host/diagnostics_test.rs` 同形）。hook 是进程全局状态，故 `serial`。
    #[test]
    #[serial]
    fn panic_hook_records_panic_through_tracing() {
        let buffer = LogBuffer::default();
        let _subscriber = tracing::subscriber::set_default(buffer.subscriber());
        let previous = std::panic::take_hook();
        install_panic_hook();

        let result = std::panic::catch_unwind(|| panic!("acp panic hook probe"));
        std::panic::set_hook(previous);

        assert!(result.is_err(), "catch_unwind 必须捕获 panic");
        let logged = buffer.text();
        assert!(
            logged.contains("thread panicked at"),
            "日志缺少 panic 记录：{logged}"
        );
        assert!(
            logged.contains("acp panic hook probe"),
            "日志缺少 panic 载荷：{logged}"
        );
        assert!(logged.contains("mod.rs"), "日志缺少 panic 位置：{logged}");
    }
}
