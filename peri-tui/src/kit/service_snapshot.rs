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
//! 在 task 边界外无法实时读。本任务从 `peri_config` 派生 provider，再通过
//! `ProviderModels.get_model(active_alias)` 查询实际 model name——若查到非空字符串
//! 则使用该值，否则 fallback 到 active_alias 本身。
//! 这意味着如果用户运行时切 provider/model（通过 TUI 命令），本任务下次 tick 会看到。

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use peri_acp_types::permission::{PermissionMode, SharedPermissionMode};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::app::service_registry::{ProcessResourceMonitor, SharedPeriConfig};
use crate::kit::atoms::{
    ACTIVE_SESSION_ID, CRON_JOBS, CURRENT_SESSION_TITLE, CronJobSummary, FILE_LIST, HOOK_LIST,
    Handle, HookSummary, MCP_SERVERS, MEMORY_LIST, McpInitPhase, McpServerSummary,
    McpStatusSnapshot, MemoryEntry, PLUGIN_LIST, PROVIDER_LIST, PluginSummary, ProviderSummary,
    SERVICE_SNAPSHOT, ServiceSnapshot, THREAD_LIST, ThreadSummary,
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
    /// Cron/MCP 资源句柄直读（C 类豁免至 M-TUI，见批 3 tui-deps 未做项）
    pub cron_scheduler: Arc<Mutex<peri_middlewares::cron::CronScheduler>>,
    pub mcp_pool: Option<Arc<peri_middlewares::mcp::McpClientPool>>,
    /// MCP 初始化状态 watch receiver——`.borrow()` 即可读当前状态。
    /// 用 `tokio::sync::watch::Receiver` 而非 `Arc<watch::Sender<...>>` 因为
    /// receiver 自身 `Clone` 后仍指向同一 watch channel。
    pub mcp_init_rx: Option<tokio::sync::watch::Receiver<peri_middlewares::mcp::McpInitStatus>>,
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
    /// 当前会话标题派生缓存：上次查询的 session id + 结果 + 下次刷新时刻。
    current_title_session_id: String,
    current_title: String,
    next_title_refresh: Instant,
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
            current_title_session_id: String::new(),
            current_title: String::new(),
            // 首 tick 立即查询（Instant::now() 已过期）
            next_title_refresh: Instant::now(),
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
    let (provider_name, model_alias, model_name, effort) =
        derive_provider_and_model(&src.peri_config);

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
                .filter(|m| m.message_count > 0) // 不显示无消息的空 thread
                .filter(|m| m.cwd == src.cwd) // 只显示当前工作目录的 thread
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

    // ── 6e. 当前会话标题：从 thread_store 派生（节流查询） ───────────────
    // session id 变化立即查询；同 id 每 10s 刷新一次（覆盖标题中途被
    // 自动生成 / rename 的情况）。load_meta 是主键查询，开销极低。
    let session_id = ACTIVE_SESSION_ID.state().read().clone();
    let session_changed = session_id != slow.current_title_session_id;
    if session_changed && session_id.is_empty() {
        // 会话已关闭（id 清空）：清空标题缓存，避免旧会话标题残留到状态栏
        slow.current_title.clear();
        slow.current_title_session_id.clear();
        slow.next_title_refresh = now + Duration::from_secs(10);
    } else if (session_changed || now >= slow.next_title_refresh) && !session_id.is_empty() {
        match src.thread_store.load_meta(&session_id).await {
            Ok(meta) => {
                slow.current_title = meta.title.unwrap_or_default();
                slow.current_title_session_id = session_id;
            }
            Err(e) => {
                warn!(error = %e, sid = %session_id, "service_snapshot: load_meta for session title failed");
            }
        }
        slow.next_title_refresh = now + Duration::from_secs(10);
    }
    let current_title = slow.current_title.clone();
    write_if_changed(&CURRENT_SESSION_TITLE.state(), current_title);

    // ── 7. 写入 atoms ───────────────────────────────────────────────────
    let snap = ServiceSnapshot {
        cwd: src.cwd.clone(),
        provider_name,
        model_alias,
        model_name,
        effort,
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
    // H1 系列：hooks/plugins 来自 launch 时派生的静态数据；providers 每 tick
    // 从 peri_config 动态派生以反映 is_active 最新状态。MCP server 详细列表
    // 每 tick 重新派生以反映连接状态；Memory 列表每 tick 重新扫描。
    write_if_changed(&HOOK_LIST.state(), src.hooks.clone());
    write_if_changed(&PLUGIN_LIST.state(), src.plugins.clone());
    write_if_changed(&PROVIDER_LIST.state(), derive_providers(&src.peri_config));
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

/// 从 PeriConfig 派生 (provider_type, active_alias, model_name)。
/// model_name 优先取 Profile.model；其次 provider.models.get_model(alias)；
/// 若都为空，回退到 active_alias。
/// 从 peri_config 派生 (provider_type, active_alias, model_name, effort)。
///
/// 全部取自 active Profile：provider 类型（Profile.provider 指向的 provider，
/// 空则回退第一个 provider）、alias、模型名（Profile.model > ProviderModels 映射
/// > alias 本身）、effort（Profile.effort，缺省 "xhigh"）。
fn derive_provider_and_model(peri_config: &SharedPeriConfig) -> (String, String, String, String) {
    let cfg = peri_config.read();
    let active_alias = cfg.config.active_alias.clone();
    let profile = cfg.config.profiles.get(&active_alias);

    let provider = profile.and_then(|pf| {
        if pf.provider.is_empty() {
            cfg.config.providers.first()
        } else {
            cfg.config.providers.iter().find(|p| p.id == pf.provider)
        }
    });

    let provider_type = provider
        .map(|p| p.provider_type.clone())
        .unwrap_or_else(|| {
            profile
                .map(|pf| pf.provider.clone())
                .unwrap_or_else(|| active_alias.clone())
        });

    let model_name = if let Some(m) = profile
        .and_then(|pf| pf.model.as_ref())
        .filter(|m| !m.is_empty())
    {
        m.clone()
    } else {
        provider
            .and_then(|p| p.models.get_model(&active_alias))
            .map(str::to_string)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| active_alias.clone())
    };

    let effort = profile
        .map(|pf| pf.effort.clone())
        .filter(|e| !e.is_empty())
        .unwrap_or_else(|| "xhigh".to_string());

    (provider_type, active_alias, model_name, effort)
}

/// 从 peri_config 动态派生 provider 列表——PROVIDER_LIST atom 的数据源。
///
/// 每次 tick 重新读取 active profile 的 provider，确保 is_active 标记反映最新状态。
/// 之前使用 SnapshotSource.providers（启动时静态快照），导致 is_active 永不过期。
fn derive_providers(peri_config: &SharedPeriConfig) -> Vec<ProviderSummary> {
    let cfg = peri_config.read();
    let active_profile_provider = cfg
        .config
        .profiles
        .get(&cfg.config.active_alias)
        .map(|p| p.provider.clone())
        .unwrap_or_default();
    cfg.config
        .providers
        .iter()
        .map(|p| {
            let env_key = format!("{}_API_KEY", p.provider_type.to_uppercase());
            let has_api_key = !p.api_key.is_empty() || std::env::var(env_key).is_ok();
            let base_url = if p.base_url.is_empty() {
                None
            } else {
                Some(p.base_url.clone())
            };
            ProviderSummary {
                id: p.id.clone(),
                provider_type: p.provider_type.clone(),
                is_active: p.id == active_profile_provider,
                has_api_key,
                base_url,
            }
        })
        .collect()
}

/// 从 `McpClientPool` 派生详细 MCP server 列表（H1d）。
fn derive_mcp_servers(
    pool: &Option<Arc<peri_middlewares::mcp::McpClientPool>>,
) -> Vec<McpServerSummary> {
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
            needs_auth: matches!(
                info.oauth_status,
                peri_middlewares::mcp::OAuthStatus::NeedsAuthorization
            ),
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
    pool: &Option<Arc<peri_middlewares::mcp::McpClientPool>>,
    init_rx: &Option<tokio::sync::watch::Receiver<peri_middlewares::mcp::McpInitStatus>>,
) -> McpStatusSnapshot {
    let init_phase = match init_rx {
        Some(rx) => match *rx.borrow() {
            peri_middlewares::mcp::McpInitStatus::Pending => McpInitPhase::Pending,
            peri_middlewares::mcp::McpInitStatus::Initializing { .. } => McpInitPhase::Initializing,
            peri_middlewares::mcp::McpInitStatus::Ready { .. } => McpInitPhase::Ready,
            peri_middlewares::mcp::McpInitStatus::Failed(_) => McpInitPhase::Failed,
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

#[cfg(test)]
#[path = "service_snapshot_test.rs"]
mod tests;
