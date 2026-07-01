use std::{path::PathBuf, sync::Arc};

use parking_lot::RwLock;
use peri_middlewares::{
    mcp::{McpClientPool, McpInitStatus},
    plugin::PluginLoadResult,
    prelude::SharedPermissionMode,
};

use super::cron_state::CronState;
use crate::{config::PeriConfig, thread::ThreadStore};

/// `ServiceRegistry` 中共享的配置类型：单一来源（Single Source of Truth）。
///
/// TUI 与 ACP Server 共享同一个 `Arc<RwLock<PeriConfig>>`，写入会即时传播，
/// 无需手动调用 `sync_acp_config`。
pub type SharedPeriConfig = Arc<RwLock<PeriConfig>>;

/// 进程资源采样器：每 2 秒采样一次当前进程的 CPU 和内存
pub struct ProcessResourceMonitor {
    sys: sysinfo::System,
    pid: sysinfo::Pid,
    /// 上次采样时间
    last_sample: std::time::Instant,
    /// 缓存的内存使用量（MB）
    memory_mb: u64,
    /// 缓存的 CPU 占用百分比（0.0-100.0，单核；可超过 100 表示多核）
    cpu_percent: f32,
}

impl Default for ProcessResourceMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessResourceMonitor {
    pub fn new() -> Self {
        let mut sys = sysinfo::System::new();
        let pid = sysinfo::get_current_pid().expect("failed to get current PID");
        sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
        Self {
            sys,
            pid,
            last_sample: std::time::Instant::now() - std::time::Duration::from_secs(3), // 确保首次调用立即采样
            memory_mb: 0,
            cpu_percent: 0.0,
        }
    }

    /// 刷新缓存（仅当距上次采样 ≥ 2 秒时才执行系统调用）
    pub fn refresh_if_needed(&mut self) {
        if self.last_sample.elapsed() >= std::time::Duration::from_secs(2) {
            self.sys
                .refresh_processes(sysinfo::ProcessesToUpdate::Some(&[self.pid]), true);
            if let Some(proc) = self.sys.process(self.pid) {
                self.memory_mb = proc.memory() / 1024 / 1024;
                self.cpu_percent = proc.cpu_usage();
            }
            self.last_sample = std::time::Instant::now();
        }
    }

    pub fn memory_mb(&self) -> u64 {
        self.memory_mb
    }

    pub fn cpu_percent(&self) -> f32 {
        self.cpu_percent
    }
}

/// 全局服务/状态聚合：跨 session 共享的服务字段。
pub struct ServiceRegistry {
    /// 共享配置：TUI 与 ACP Server 持有同一 `Arc`，写入即时传播。
    pub peri_config: SharedPeriConfig,
    pub cwd: String,
    pub provider_name: String,
    pub model_name: String,
    pub permission_mode: Arc<SharedPermissionMode>,
    pub thread_store: Arc<dyn ThreadStore>,
    pub mcp_pool: Option<Arc<McpClientPool>>,
    pub mcp_init_rx: Option<tokio::sync::watch::Receiver<McpInitStatus>>,
    pub cron: CronState,
    pub plugin_data: Option<PluginLoadResult>,
    pub config_path_override: Option<PathBuf>,
    pub claude_settings_override: Option<PathBuf>,
    /// 进程内存监控（2s 刷新）
    pub resource_monitor: parking_lot::Mutex<ProcessResourceMonitor>,
    /// i18n 语言注册表（跨 session 共享）
    pub lc: crate::i18n::LcRegistry,
    /// panic hook 通知 receiver（TUI 模式专用，由 main.rs init_panic_notify 初始化）
    pub panic_notify_rx: Option<tokio::sync::mpsc::UnboundedReceiver<String>>,
    /// ACP SessionManager 句柄（Arc 共享，clone 廉价）。
    /// 由 main.rs 构建 ACP server 时注入；None 时（如未启动 ACP server）降级为无 v2 queue 注入。
    /// 用于 TUI 侧 cron/channel 异步触发 push 到共享 v2 MessageQueue。
    pub acp_session_manager: Option<peri_acp::session::SessionManager>,
}
