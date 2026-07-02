//! Service snapshot 后台任务——周期性从 `ServiceRegistry` 派生轻量投影写入 atoms。
//!
//! 这是 kit 路径"读取 ServiceRegistry"的统一入口——所有面板（Model/Cron/Mcp 等）
//! 通过订阅 atoms 获取数据，不直接借 `&App`。
//!
//! ## 为什么不直接持 `&App`
//!
//! `App` 含 TextArea 等非 Send 字段，无法跨 tokio task 共享。本任务持的是
//! `ServiceRegistry` 中已经 Arc 化的共享字段（thread_store / peri_config /
//! permission_mode / cron / mcp_pool），它们天然 Send+Sync，可安全跨越 task 边界。
//!
//! ## 派生而非直读
//!
//! `provider_name` / `model_name` 在 `ServiceRegistry` 内是 `String`（非 Arc），
//! 在 task 边界外无法实时读。本任务从 `peri_config` 派生 provider，再查 LlmProvider
//! 得到 model alias——这与 `app::agent::LlmProvider::from_config` 的逻辑对齐。
//! 这意味着如果用户运行时切 provider/model（通过 TUI 命令），本任务下次 tick 会看到。

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use peri_middlewares::{
    cron::CronScheduler,
    hitl::{PermissionMode, SharedPermissionMode},
    mcp::{McpClientPool, McpInitStatus},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::app::service_registry::{ProcessResourceMonitor, SharedPeriConfig};
use crate::kit::atoms::{
    AcpStateSnapshot, CRON_JOBS, CronJobSummary, FILE_LIST, HOOK_LIST, Handle, HookSummary,
    MCP_SERVERS, MEMORY_LIST, McpInitPhase, McpServerSummary, McpStatusSnapshot, MemoryEntry,
    PLUGIN_LIST, PROVIDER_LIST, PluginSummary, ProviderSummary, SERVICE_SNAPSHOT, ServiceSnapshot,
    THREAD_LIST, ThreadSummary,
};
use crate::thread::ThreadStore;

/// 快照源——`build_app_and_acp` 后从 `ServiceRegistry` 抽出的 Arc 共享句柄。
///
/// 所有字段都是 `Arc` 或 `Clone 廉价的句柄`，可以安全移动到 tokio task。
#[derive(Clone)]
pub struct SnapshotSource {
    pub cwd: String,
    pub thread_store: Arc<dyn ThreadStore>,
    pub peri_config: SharedPeriConfig,
    pub permission_mode: Arc<SharedPermissionMode>,
    pub cron_scheduler: Arc<Mutex<CronScheduler>>,
    pub mcp_pool: Option<Arc<McpClientPool>>,
    /// MCP 初始化状态 watch receiver——`.borrow()` 即可读当前状态。
    /// 用 `tokio::sync::watch::Receiver` 而非 `Arc<watch::Sender<...>>` 因为
    /// receiver 自身 `Clone` 后仍指向同一 watch channel。
    pub mcp_init_rx: Option<tokio::sync::watch::Receiver<McpInitStatus>>,
    /// 独立的进程监控器（采样进程级数据，多实例不影响正确性）。
    /// 用 `Arc<Mutex<_>>` 让 task 间共享，避免每 tick 重建。
    pub resource_monitor: Arc<Mutex<ProcessResourceMonitor>>,
    /// H1b/c/f：插件加载结果（启动时一次性派生的静态数据）
    pub hooks: Vec<HookSummary>,
    pub plugins: Vec<PluginSummary>,
    pub providers: Vec<ProviderSummary>,
}

/// 启动 service snapshot 后台任务。
///
/// 默认 2 秒一拍：CPU/MEM 采样本身已是 2s 节流，更短间隔无收益；更长间隔会让
/// 面板（如 MCP/Cron）数据陈旧。返回 JoinHandle，调用方可丢弃（任务自管生命周期）。
pub fn spawn_service_snapshot(
    src: SnapshotSource,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        let mut slow_refresh = SlowSnapshotRefresh::default();
        // 起始 tick 立即触发一次（首次 interval.tick() 立即返回）——让 UI 启动后
        // 立即拿到首帧服务快照，而非 2s 后才出现数据。
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    debug!("service_snapshot: shutdown signal received, exiting");
                    break;
                }
                _ = interval.tick() => {
                    if let Err(e) = tick_once(&src, &mut slow_refresh).await {
                        warn!(error = %e, "service_snapshot: tick failed");
                    }
                }
            }
        }
    })
}

struct SlowSnapshotRefresh {
    next_file_scan: Instant,
    next_thread_scan: Instant,
    next_memory_scan: Instant,
    files: Vec<String>,
    threads: Vec<ThreadSummary>,
    memory_entries: Vec<MemoryEntry>,
}

impl Default for SlowSnapshotRefresh {
    fn default() -> Self {
        Self {
            next_file_scan: Instant::now(),
            next_thread_scan: Instant::now(),
            next_memory_scan: Instant::now(),
            files: Vec::new(),
            threads: Vec::new(),
            memory_entries: Vec::new(),
        }
    }
}

/// 单次快照派发——读所有源 → 写所有 atom。返回 Err 仅在 thread_store I/O 失败时。
async fn tick_once(
    src: &SnapshotSource,
    slow: &mut SlowSnapshotRefresh,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // ── 1. CPU/MEM（同步采样，持锁时间极短） ────────────────────────────
    let (memory_mb, cpu_percent) = {
        let mut monitor = src.resource_monitor.lock();
        monitor.refresh_if_needed();
        (monitor.memory_mb(), monitor.cpu_percent())
    };

    // ── 2. provider/model 从 peri_config 派生 ──────────────────────────
    let (provider_name, model_alias) = derive_provider_and_model(&src.peri_config);

    // ── 3. permission_mode ─────────────────────────────────────────────
    let permission_mode = permission_mode_label(src.permission_mode.load());

    // ── 4. MCP 池状态 ────────────────────────────────────────────────────
    let mcp = derive_mcp_status(&src.mcp_pool, &src.mcp_init_rx);

    // ── 5. Cron 任务 ─────────────────────────────────────────────────────
    let (cron_total, cron_enabled, cron_jobs) = {
        let scheduler = src.cron_scheduler.lock();
        let tasks = scheduler.list_tasks();
        let total = tasks.len();
        let enabled = tasks.iter().filter(|t| t.enabled).count();
        let jobs: Vec<CronJobSummary> = tasks
            .iter()
            .map(|t| CronJobSummary {
                id: t.id.clone(),
                expression: t.expression.clone(),
                prompt: t.prompt.clone(),
                enabled: t.enabled,
                next_fire: t.next_fire,
            })
            .collect();
        (total, enabled, jobs)
    };

    // ── 6. Thread 列表：慢频刷新，避免空闲时持续打 SQLite / I/O ───────────────
    let now = Instant::now();
    if now >= slow.next_thread_scan {
        slow.threads = match src.thread_store.list_threads().await {
            Ok(metas) => metas
                .into_iter()
                .filter(|m| !m.hidden) // 主列表不显示子 agent
                .map(|m| ThreadSummary {
                    id: m.id.clone(),
                    title: m.title.clone(),
                    cwd: m.cwd.clone(),
                    message_count: m.message_count,
                    updated_at: Some(m.updated_at),
                })
                .collect(),
            Err(e) => {
                warn!(error = %e, "service_snapshot: list_threads failed");
                Vec::new()
            }
        };
        slow.next_thread_scan = now + Duration::from_secs(30);
    }
    let threads = slow.threads.clone();

    // ── 6b. C2: cwd 文件浅扫（@mention 补全用） ────────────────────────
    // 文件列表不需要 2 秒刷新；慢频刷新可避免大型 cwd 空闲时持续扫盘。
    if now >= slow.next_file_scan {
        let cwd_for_scan = src.cwd.clone();
        slow.files = match tokio::task::spawn_blocking(move || {
            scan_cwd_files_shallow(&cwd_for_scan)
        })
        .await
        {
            Ok(result) => result,
            Err(e) => {
                warn!(error = %e, "service_snapshot: spawn_blocking for cwd scan failed");
                Vec::new()
            }
        };
        slow.next_file_scan = now + Duration::from_secs(30);
    }
    let files = slow.files.clone();

    // ── 6c. H1d: MCP server 详细列表（从 pool 派生） ───────────────────
    let mcp_servers: Vec<McpServerSummary> = derive_mcp_servers(&src.mcp_pool);

    // ── 6d. H1h: ~/.claude/memory 文件扫描 ──────────────────────────────
    // Memory 面板数据慢频刷新即可，避免空闲时每 2 秒扫 ~/.claude/memory。
    if now >= slow.next_memory_scan {
        slow.memory_entries = match tokio::task::spawn_blocking(scan_memory_dir).await {
            Ok(result) => result,
            Err(e) => {
                warn!(error = %e, "service_snapshot: spawn_blocking for memory scan failed");
                Vec::new()
            }
        };
        slow.next_memory_scan = now + Duration::from_secs(30);
    }
    let memory_entries = slow.memory_entries.clone();

    // ── 7. 写入 atoms ───────────────────────────────────────────────────
    let snap = ServiceSnapshot {
        cwd: src.cwd.clone(),
        provider_name,
        model_alias,
        permission_mode: permission_mode.to_string(),
        memory_mb,
        cpu_percent: cpu_percent.round(),
        mcp,
        cron_total,
        cron_enabled,
    };

    write_if_changed(&SERVICE_SNAPSHOT.state(), snap);
    write_if_changed(&THREAD_LIST.state(), threads);
    write_if_changed(&CRON_JOBS.state(), cron_jobs);
    write_if_changed(&FILE_LIST.state(), files);
    // H1 系列：静态数据来自 launch 时派生的 src；MCP server 详细列表每 tick
    // 重新派生以反映连接状态；Memory 列表每 tick 重新扫描。所有 atom 都只在
    // 值变化时写入，避免 ratatui-kit Atom 对相同值写入也唤醒订阅组件。
    write_if_changed(&HOOK_LIST.state(), src.hooks.clone());
    write_if_changed(&PLUGIN_LIST.state(), src.plugins.clone());
    write_if_changed(&PROVIDER_LIST.state(), src.providers.clone());
    write_if_changed(&MCP_SERVERS.state(), mcp_servers);
    write_if_changed(&MEMORY_LIST.state(), memory_entries);

    Ok(())
}

fn write_if_changed<T>(state: &Handle<T>, next: T)
where
    T: PartialEq + Send + Sync + 'static,
{
    if *state.read() != next {
        *state.write() = next;
    }
}

/// 浅扫 cwd 文件列表（深度=2，最多 500 条），用于 @mention 补全。
///
/// 实现：先列顶层条目（文件 + 目录），文件直接入列；目录若不在忽略名单则进入
/// 一层。最终返回相对 cwd 的路径（POSIX 风格 `/`-分隔）。
///
/// 性能：典型 Rust 项目 ~200 文件 → 一次扫描 < 5ms；node_modules / target 等
/// 大目录被忽略，避免数十万文件遍历。失败时返回空 Vec（@mention 自动失效）。
fn scan_cwd_files_shallow(cwd: &str) -> Vec<String> {
    use std::fs;
    use std::path::Path;

    const MAX_FILES: usize = 500;
    /// 常见忽略目录——出现于任何层级都跳过（顶层 + 1 层子目录）。
    const IGNORED_DIRS: &[&str] = &[
        ".git",
        ".hg",
        ".svn",
        "node_modules",
        "target",
        "dist",
        "build",
        "__pycache__",
        ".next",
        ".nuxt",
        ".venv",
        "venv",
        ".cache",
        ".idea",
        ".vscode",
        ".DS_Store",
        ".gradle",
        ".maven",
        ".cargo",
        ".rust-analyzer",
        ".ruff_cache",
        ".pytest_cache",
        "coverage",
        "out",
        "bin",
        "obj",
        ".terraform",
        ".serverless",
        ".aws",
        ".kube",
    ];

    let root = Path::new(cwd);
    if !root.exists() || !root.is_dir() {
        return Vec::new();
    }

    let mut out: Vec<String> = Vec::new();

    // 顶层扫描
    let top_entries = match fs::read_dir(root) {
        Ok(it) => it,
        Err(_) => return Vec::new(),
    };

    // 收集顶层目录用于二层扫描（保持顺序，避免 HashMap 开销）
    let mut subdirs_to_scan: Vec<std::path::PathBuf> = Vec::new();

    for entry in top_entries.flatten() {
        if out.len() >= MAX_FILES {
            break;
        }
        let path = entry.path();
        let file_name = match entry.file_name().into_string() {
            Ok(s) => s,
            Err(_) => continue,
        };

        if IGNORED_DIRS.contains(&file_name.as_str()) {
            continue;
        }

        if path.is_dir() {
            subdirs_to_scan.push(path);
        } else {
            push_rel_path(&mut out, &path, root);
        }
    }

    // 二层扫描
    for subdir in subdirs_to_scan {
        if out.len() >= MAX_FILES {
            break;
        }
        let sub_entries = match fs::read_dir(&subdir) {
            Ok(it) => it,
            Err(_) => continue,
        };
        for entry in sub_entries.flatten() {
            if out.len() >= MAX_FILES {
                break;
            }
            let path = entry.path();
            let file_name = match entry.file_name().into_string() {
                Ok(s) => s,
                Err(_) => continue,
            };
            if IGNORED_DIRS.contains(&file_name.as_str()) {
                continue;
            }
            // 二层不再递归子目录（深度=2 截止）
            if path.is_file() {
                push_rel_path(&mut out, &path, root);
            }
        }
    }

    out
}

/// 把 `path` 相对 `root` 的路径压入 `out`（POSIX 风格分隔符），跳过根路径本身。
fn push_rel_path(out: &mut Vec<String>, path: &std::path::Path, root: &std::path::Path) {
    if let Ok(rel) = path.strip_prefix(root) {
        let s = rel.to_string_lossy().replace('\\', "/");
        if !s.is_empty() {
            out.push(s);
        }
    }
}

/// PermissionMode → 稳定小写字符串（用于 UI 显示 + atom 存储）。
///
/// `Default` 在 status bar 历史上显示为空字符串，但 atom 化后需要明确语义，
/// 这里统一返回 "default"——UI 渲染时如果想保持空白，可在渲染层判断。
fn permission_mode_label(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Default => "default",
        PermissionMode::AcceptEdit => "accept-edit",
        PermissionMode::AutoMode => "auto-mode",
        PermissionMode::Bypass => "bypass",
    }
}

/// 从 `peri_config` 派生 provider 显示名 + model alias。
///
/// - provider：取 `active_provider_id`，再从 `providers` 列表查同 ID 的 `provider_type`
///   （如 "anthropic" / "openai"）；查不到则回退 active_provider_id 本身。
/// - model：直接用 `active_alias`（"opus"/"sonnet"/"haiku"）。
fn derive_provider_and_model(peri_config: &SharedPeriConfig) -> (String, String) {
    let cfg = peri_config.read();
    let active_id = cfg.config.active_provider_id.clone();
    let active_alias = cfg.config.active_alias.clone();

    let provider_type = cfg
        .config
        .providers
        .iter()
        .find(|p| p.id == active_id)
        .map(|p| p.provider_type.clone())
        .unwrap_or(active_id);

    (provider_type, active_alias)
}

/// 从 `McpClientPool` 派生详细 MCP server 列表（H1d）。
fn derive_mcp_servers(pool: &Option<Arc<McpClientPool>>) -> Vec<McpServerSummary> {
    let Some(p) = pool else {
        return Vec::new();
    };
    p.all_server_infos()
        .into_iter()
        .map(|info| McpServerSummary {
            name: info.name.clone(),
            status: format!("{:?}", info.status).to_lowercase(),
            transport: info.transport_type.clone(),
            tools_count: info.tool_count,
        })
        .collect()
}

/// 扫描 ~/.claude/memory 目录（H1h）。
///
/// 返回每个文件的相对路径、字节数、最后修改时间。失败时返回空 Vec。
fn scan_memory_dir() -> Vec<MemoryEntry> {
    use std::fs;
    use std::path::PathBuf;

    let Some(home) = dirs_next::home_dir() else {
        return Vec::new();
    };
    let mem_dir: PathBuf = home.join(".claude").join("memory");
    if !mem_dir.exists() || !mem_dir.is_dir() {
        return Vec::new();
    }

    let mut out: Vec<MemoryEntry> = Vec::new();
    let entries = match fs::read_dir(&mem_dir) {
        Ok(it) => it,
        Err(_) => return Vec::new(),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        // 只看 .md 文件
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let rel = path
            .strip_prefix(&mem_dir)
            .ok()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        if rel.is_empty() {
            continue;
        }
        let size_bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
        let modified = entry.metadata().and_then(|m| m.modified()).ok().map(|t| {
            let dt: chrono::DateTime<chrono::Utc> = t.into();
            dt
        });
        out.push(MemoryEntry {
            path: rel,
            size_bytes,
            modified,
        });
        if out.len() >= 200 {
            break;
        }
    }
    out
}

/// 从 `McpClientPool` + `McpInitStatus` watch 派生 MCP 池状态。
fn derive_mcp_status(
    pool: &Option<Arc<McpClientPool>>,
    init_rx: &Option<tokio::sync::watch::Receiver<McpInitStatus>>,
) -> McpStatusSnapshot {
    let init_phase = match init_rx {
        Some(rx) => match *rx.borrow() {
            McpInitStatus::Pending => McpInitPhase::Pending,
            McpInitStatus::Initializing { .. } => McpInitPhase::Initializing,
            McpInitStatus::Ready { .. } => McpInitPhase::Ready,
            McpInitStatus::Failed(_) => McpInitPhase::Failed,
        },
        None => McpInitPhase::Pending,
    };

    let (total, connected) = match pool {
        Some(p) => {
            let infos = p.all_server_infos();
            let total = infos.len();
            let connected = infos
                .iter()
                .filter(|info| {
                    matches!(info.status, peri_middlewares::mcp::ClientStatus::Connected)
                })
                .count();
            (total, connected)
        }
        None => (0, 0),
    };

    McpStatusSnapshot {
        init_phase,
        total,
        connected,
    }
}

/// 同步派生 ACP state 投影的辅助函数——保留供 entry.rs 在 build_app_and_acp 后调用，
/// 让 ACP_STATE atom 首帧就有合理值（avoid 初始 0/Empty UI 闪动）。
#[allow(dead_code)]
pub fn initial_acp_state() -> AcpStateSnapshot {
    AcpStateSnapshot::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::service_registry::ProcessResourceMonitor;
    use chrono::Utc;
    use peri_agent::thread::SqliteThreadStore;
    use peri_middlewares::cron::CronScheduler;
    use serial_test::serial;

    /// 创建一个 SQLite in-memory thread store 用于测试。
    async fn make_sqlite_store() -> Arc<dyn ThreadStore> {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test_threads.db");
        // SAFETY: tempdir 保持到函数返回前有效；我们 leak 它让 store 在测试期间存活。
        // 测试结束后 OS 会清理 tempfile。
        std::mem::forget(tmp);
        Arc::new(SqliteThreadStore::new(path).await.unwrap())
    }

    fn make_minimal_source(thread_store: Arc<dyn ThreadStore>) -> SnapshotSource {
        let peri_config = Arc::new(parking_lot::RwLock::new(
            crate::config::PeriConfig::default(),
        ));
        let permission_mode = SharedPermissionMode::new(PermissionMode::Default);
        let scheduler = Arc::new(Mutex::new(CronScheduler::new(
            tokio::sync::mpsc::unbounded_channel().0,
        )));
        let monitor = Arc::new(Mutex::new(ProcessResourceMonitor::new()));

        SnapshotSource {
            cwd: ".".into(),
            thread_store,
            peri_config,
            permission_mode,
            cron_scheduler: scheduler,
            mcp_pool: None,
            mcp_init_rx: None,
            resource_monitor: monitor,
            hooks: Vec::new(),
            plugins: Vec::new(),
            providers: Vec::new(),
        }
    }

    #[tokio::test]
    #[serial]
    async fn test_tick_once_writes_atoms() {
        // 先 init atoms（避免 SERVICE_SNAPSHOT.get() 返回 None）
        crate::kit::atoms::init_atoms();

        let store = make_sqlite_store().await;
        let src = make_minimal_source(store);
        let mut slow = SlowSnapshotRefresh::default();
        let result = tick_once(&src, &mut slow).await;
        assert!(result.is_ok(), "tick_once should succeed");

        let snap = SERVICE_SNAPSHOT.state().read().clone();
        assert_eq!(snap.cwd, ".");
        assert_eq!(snap.cron_total, 0);
        assert_eq!(snap.cron_enabled, 0);
        assert_eq!(snap.mcp.total, 0);
        assert_eq!(snap.mcp.connected, 0);
        assert_eq!(snap.mcp.init_phase, McpInitPhase::Pending);
    }

    #[tokio::test]
    #[serial]
    async fn test_tick_once_empty_thread_list() {
        crate::kit::atoms::init_atoms();

        let store = make_sqlite_store().await;
        let src = make_minimal_source(store);
        // 空 SQLite store——list_threads 返回空 Vec
        let mut slow = SlowSnapshotRefresh::default();
        let result = tick_once(&src, &mut slow).await;
        assert!(result.is_ok());

        let threads = THREAD_LIST.state().read().clone();
        assert!(threads.is_empty());
    }

    #[tokio::test]
    #[serial]
    async fn test_cron_tasks_collected() {
        crate::kit::atoms::init_atoms();

        let store = make_sqlite_store().await;
        let src = make_minimal_source(store);
        // 注册两个 cron 任务（一个 disabled）
        {
            let mut scheduler = src.cron_scheduler.lock();
            let _ = scheduler.register("*/5 * * * *", "test prompt 1").unwrap();
            let id2 = scheduler.register("*/10 * * * *", "test prompt 2").unwrap();
            scheduler.toggle(&id2); // disable
        }

        let mut slow = SlowSnapshotRefresh::default();
        let result = tick_once(&src, &mut slow).await;
        assert!(result.is_ok());

        let jobs = CRON_JOBS.state().read().clone();
        assert_eq!(jobs.len(), 2);

        let snap = SERVICE_SNAPSHOT.state().read().clone();
        assert_eq!(snap.cron_total, 2);
        assert_eq!(snap.cron_enabled, 1);
    }

    #[tokio::test]
    async fn test_derive_provider_and_model_default() {
        let peri_config = Arc::new(parking_lot::RwLock::new(
            crate::config::PeriConfig::default(),
        ));
        let (provider, model) = derive_provider_and_model(&peri_config);
        // 默认 AppConfig::default() 的 active_alias 和 active_provider_id 均为空
        assert!(provider.is_empty());
        assert!(model.is_empty());
    }

    #[tokio::test]
    async fn test_derive_provider_and_model_set() {
        use peri_acp::provider::config::{AppConfig, ProviderConfig};

        let cfg = crate::config::PeriConfig {
            config: AppConfig {
                active_alias: "sonnet".into(),
                active_provider_id: "p1".into(),
                providers: vec![ProviderConfig {
                    id: "p1".into(),
                    provider_type: "anthropic".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
            ..Default::default()
        };
        let peri_config = Arc::new(parking_lot::RwLock::new(cfg));
        let (provider, model) = derive_provider_and_model(&peri_config);
        assert_eq!(provider, "anthropic");
        assert_eq!(model, "sonnet");
    }

    #[test]
    fn test_initial_acp_state_default() {
        let state = initial_acp_state();
        assert_eq!(state.variant, 0);
        assert!(!state.is_loading);
    }

    #[test]
    fn test_chrono_datetime_conversion() {
        // 验证 chrono::DateTime<chrono::Utc> 与 ThreadMeta.updated_at 类型一致
        let now = Utc::now();
        let _dt: chrono::DateTime<Utc> = now;
        // 验证 ThreadSummary.updated_at 是 Option<DateTime<Utc>>
        let summary = ThreadSummary {
            id: "x".into(),
            title: None,
            cwd: ".".into(),
            message_count: 0,
            updated_at: Some(now),
        };
        assert!(summary.updated_at.is_some());
    }

    /// 编译期断言：SnapshotSource 字段全部 Clone（trait bound 验证）。
    #[test]
    fn test_snapshot_source_is_clone() {
        fn assert_clone<T: Clone>() {}
        assert_clone::<SnapshotSource>();
    }

    /// C2 回归测试：scan_cwd_files_shallow 在临时目录正确收集文件相对路径。
    #[test]
    fn test_scan_cwd_files_shallow_collects_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // 顶层文件
        std::fs::write(root.join("a.txt"), "x").unwrap();
        std::fs::write(root.join("b.rs"), "x").unwrap();
        // 子目录文件（深度=2）
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src").join("mod.rs"), "x").unwrap();
        // 忽略目录：node_modules 内文件不应出现
        std::fs::create_dir_all(root.join("node_modules").join("pkg")).unwrap();
        std::fs::write(
            root.join("node_modules").join("pkg").join("ignored.js"),
            "x",
        )
        .unwrap();

        let files = scan_cwd_files_shallow(root.to_str().unwrap());
        assert!(files.contains(&"a.txt".to_string()));
        assert!(files.contains(&"b.rs".to_string()));
        assert!(files.contains(&"src/mod.rs".to_string()));
        assert!(
            !files.iter().any(|f| f.contains("node_modules")),
            "ignored dir should be filtered out, got: {:?}",
            files
        );
    }

    /// C2 回归测试：不存在的目录返回空 Vec，不 panic。
    #[test]
    fn test_scan_cwd_files_shallow_nonexistent() {
        let files = scan_cwd_files_shallow("/this/path/does/not/exist");
        assert!(files.is_empty());
    }

    /// C2 回归测试：MAX_FILES 上限防止无限增长。
    #[test]
    fn test_scan_cwd_files_shallow_caps_at_max() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // 创建 600 个文件（超过 MAX_FILES=500）
        for i in 0..600 {
            std::fs::write(root.join(format!("f{i}.txt")), "x").unwrap();
        }
        let files = scan_cwd_files_shallow(root.to_str().unwrap());
        assert!(files.len() <= 500, "should cap at 500, got {}", files.len());
    }
}
