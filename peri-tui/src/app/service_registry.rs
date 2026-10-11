use std::sync::Arc;

use parking_lot::RwLock;
use peri_acp_types::permission::SharedPermissionMode;
use peri_acp_types::plugin::PluginLoadResult;
use peri_acp_types::session_resources::SessionResources;

use super::cron_state::CronState;
use crate::config::PeriConfig;

/// `ServiceRegistry` 中共享的配置类型：单一来源（Single Source of Truth）。
///
/// TUI 与 ACP Server 共享同一个 `Arc<RwLock<PeriConfig>>`，写入会即时传播，
/// 无需手动调用 `sync_acp_config`。
pub type SharedPeriConfig = Arc<RwLock<PeriConfig>>;

/// 进程内存告警阈值（MiB）：RSS 超过该值即记录 warning 日志。
const MEMORY_WARN_THRESHOLD_MB: u64 = 200;

/// 持续超阈值时的最小重复告警间隔（避免每次采样都刷同一条日志）。
const MEMORY_WARN_REPEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// 内存告警决策：由 [`memory_warning_action`] 纯函数产出，调用方据此更新节流状态。
#[derive(Debug, PartialEq, Eq)]
enum MemoryWarning {
    /// 未超阈值：清除节流状态，下次超阈立即告警。
    Reset,
    /// 超阈值且允许告警（首次，或距上次告警已达 [`MEMORY_WARN_REPEAT_INTERVAL`]）。
    Emit,
    /// 超阈值但仍在重复间隔内：不重复记录。
    Throttled,
}

/// 判定内存采样是否应触发告警（阈值 + 节流规则唯一权威）。
fn memory_warning_action(
    memory_mb: u64,
    last_warn: Option<std::time::Instant>,
    now: std::time::Instant,
) -> MemoryWarning {
    if memory_mb <= MEMORY_WARN_THRESHOLD_MB {
        return MemoryWarning::Reset;
    }
    match last_warn {
        Some(last) if now.saturating_duration_since(last) < MEMORY_WARN_REPEAT_INTERVAL => {
            MemoryWarning::Throttled
        }
        _ => MemoryWarning::Emit,
    }
}

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
    /// 上次内存超阈告警时刻（节流状态；低于阈值时重置）
    last_memory_warn: Option<std::time::Instant>,
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
            last_sample: peri_time::monotonic_now() - std::time::Duration::from_secs(3), // 确保首次调用立即采样
            memory_mb: 0,
            cpu_percent: 0.0,
            last_memory_warn: None,
        }
    }

    /// 刷新缓存（仅当距上次采样 ≥ 2 秒时才执行系统调用）
    pub fn refresh_if_needed(&mut self) {
        if peri_time::elapsed_since(self.last_sample) >= std::time::Duration::from_secs(2) {
            self.sys
                .refresh_processes(sysinfo::ProcessesToUpdate::Some(&[self.pid]), true);
            if let Some(proc) = self.sys.process(self.pid) {
                self.memory_mb = proc.memory() / 1024 / 1024;
                self.cpu_percent = proc.cpu_usage();
            }
            self.warn_if_memory_above_threshold();
            self.last_sample = peri_time::monotonic_now();
        }
    }

    /// 采样后按阈值记录进程内存告警；节流规则见 [`memory_warning_action`]。
    fn warn_if_memory_above_threshold(&mut self) {
        let now = peri_time::monotonic_now();
        match memory_warning_action(self.memory_mb, self.last_memory_warn, now) {
            MemoryWarning::Reset => self.last_memory_warn = None,
            MemoryWarning::Throttled => {}
            MemoryWarning::Emit => {
                self.last_memory_warn = Some(now);
                tracing::warn!(
                    target: "peri.mem",
                    rss_mb = self.memory_mb,
                    threshold_mb = MEMORY_WARN_THRESHOLD_MB,
                    cpu_percent = self.cpu_percent,
                    "process memory above threshold"
                );
            }
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
///
/// (I17-D) 大幅瘦身：model_name / config_path_override /
/// claude_settings_override / lc / panic_notify_rx / acp_session_manager
/// 6 字段退役——前者 service_snapshot 派生不需要，中两者仅 mod.rs 设为 None
/// 无任何读写，lc 无 services.lc 访问，后两者 launch 写入后无消费者。
pub struct ServiceRegistry {
    /// 共享配置：TUI 与 ACP Server 持有同一 `Arc`，写入即时传播。
    pub peri_config: SharedPeriConfig,
    pub cwd: String,
    pub provider_name: String,
    pub permission_mode: Arc<SharedPermissionMode>,
    /// 会话资源门面：Agent transcript/subagent、middleware 与协议面的唯一会话行为入口。
    pub session_resources: Arc<dyn SessionResources>,
    pub mcp_pool: Option<Arc<peri_middlewares::mcp::McpClientPool>>,
    pub mcp_task_owner: Option<peri_middlewares::mcp::McpTaskOwner>,
    pub mcp_init_rx: Option<tokio::sync::watch::Receiver<peri_middlewares::mcp::McpInitStatus>>,
    pub cron: CronState,
    pub plugin_data: Option<PluginLoadResult>,
    /// 进程内存监控（2s 刷新）
    pub resource_monitor: parking_lot::Mutex<ProcessResourceMonitor>,
}

#[cfg(test)]
#[path = "service_registry_test.rs"]
mod tests;
