//! Fullscreen TUI startup and shutdown.

use super::*;

/// TUI 模式启动选项
#[allow(dead_code)] // 部分 CLI 桥接字段尚未接入
pub(super) struct TuiOptions {
    pub(super) permission_mode: Option<String>,
    pub(super) skip_permissions: bool,
    pub(super) model: Option<String>,
    pub(super) effort: Option<String>,
    pub(super) continue_session: bool,
    pub(super) resume_session: Option<String>,
    pub(super) session_id: Option<String>,
    pub(super) session_name: Option<String>,
    pub(super) settings: Option<String>,
    pub(super) allowed_tools: Vec<String>,
    pub(super) disallowed_tools: Vec<String>,
    /// 会话存储定位描述（已由入口归一；恢复会话不再重新解析存储）。
    pub(super) session_store: SessionStoreDeployment,
}

pub(super) fn propagate_tui_result(result: Result<()>) -> Result<()> {
    if let Err(e) = result {
        eprintln!("Error: {e}");
        return Err(e);
    }
    Ok(())
}

pub(super) fn run_tui(opts: TuiOptions) -> Result<()> {
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
            session_store: opts.session_store.clone(),
        };
        peri_tui::kit::entry::run_kit_fullscreen(launch_opts, panic_notify_rx).await
    });

    // 先 drop rt（关闭所有 tokio 任务），再 drop _telemetry
    drop(rt);
    drop(_telemetry);

    propagate_tui_result(result)
}
