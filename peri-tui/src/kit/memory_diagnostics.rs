//! Opt-in TUI memory diagnostics for issue #127.
//!
//! Records contain only aggregate counters and allocator statistics. They never
//! serialize view-model contents, paths, session identifiers, or user data.

use std::io;
use std::mem::size_of;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use serde::Serialize;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::alloc_config;
use crate::kit::atoms::{THREAD_LIST, ThreadSummary, VIEW_MODELS};

pub const ENABLE_ENV: &str = "PERI_MEMORY_DIAGNOSTICS";
pub const PATH_ENV: &str = "PERI_MEMORY_DIAGNOSTICS_PATH";
pub const INTERVAL_ENV: &str = "PERI_MEMORY_DIAGNOSTICS_INTERVAL_MS";
pub const MAX_BYTES_ENV: &str = "PERI_MEMORY_DIAGNOSTICS_MAX_BYTES";
const DEFAULT_INTERVAL_MS: u64 = 5_000;
const MIN_INTERVAL_MS: u64 = 250;
const DEFAULT_MAX_BYTES: u64 = 64 * 1024 * 1024;
const MIN_MAX_BYTES: u64 = 1024;
const MAX_MAX_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryDiagnosticsConfig {
    pub output_path: PathBuf,
    pub interval: Duration,
    pub max_bytes: u64,
}

impl MemoryDiagnosticsConfig {
    pub fn from_env() -> Option<Self> {
        if !diagnostics_supported() {
            if std::env::var_os(ENABLE_ENV).is_some() {
                warn!("memory diagnostics are unsupported on this platform");
            }
            return None;
        }
        match parse_config(|key| std::env::var(key).ok()) {
            Ok(Some(mut config)) => match production_output_path(&config.output_path) {
                Ok(path) => {
                    config.output_path = path;
                    Some(config)
                }
                Err(error) => {
                    warn!(error = %error, "memory diagnostics disabled");
                    None
                }
            },
            Ok(None) => None,
            Err(error) => {
                warn!(error = %error, "memory diagnostics disabled");
                None
            }
        }
    }

    pub fn for_experiment(output_path: PathBuf, interval: Duration) -> Self {
        Self {
            output_path,
            interval,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

const fn diagnostics_supported() -> bool {
    !cfg!(target_os = "windows")
}

fn is_safe_basename(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && Path::new(value).components().count() == 1
        && !value.contains(['/', '\\'])
}

fn production_output_path(basename: &Path) -> io::Result<PathBuf> {
    let home =
        dirs_next::home_dir().ok_or_else(|| io::Error::other("diagnostic home unavailable"))?;
    let peri_dir = home.join(".peri");
    create_diagnostics_dir(&peri_dir, DirectoryPolicy::PrivateParent)?;
    let diagnostics_dir = peri_dir.join("diagnostics");
    create_diagnostics_dir(&diagnostics_dir, DirectoryPolicy::PrivateDiagnostics)?;
    Ok(diagnostics_dir.join(basename))
}

#[derive(Clone, Copy)]
enum DirectoryPolicy {
    PrivateParent,
    PrivateDiagnostics,
}

fn create_diagnostics_dir(path: &Path, policy: DirectoryPolicy) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(io::Error::other("diagnostic directory is unsafe"));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(path).map_err(|error| {
                io::Error::new(error.kind(), "diagnostic directory creation failed")
            })?;
        }
        Err(error) => {
            return Err(io::Error::new(
                error.kind(),
                "diagnostic directory unavailable",
            ));
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::symlink_metadata(path)?;
        let current_uid = unsafe { libc::geteuid() };
        if !unix_directory_metadata_is_safe(metadata.uid(), metadata.mode(), current_uid, policy) {
            return Err(io::Error::other(
                "diagnostic directory permissions are unsafe",
            ));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn unix_directory_metadata_is_safe(
    owner_uid: u32,
    mode: u32,
    current_uid: u32,
    policy: DirectoryPolicy,
) -> bool {
    owner_uid == current_uid
        && match policy {
            DirectoryPolicy::PrivateParent => mode & 0o022 == 0,
            DirectoryPolicy::PrivateDiagnostics => mode & 0o077 == 0,
        }
}

fn parse_config(
    get: impl Fn(&str) -> Option<String>,
) -> Result<Option<MemoryDiagnosticsConfig>, String> {
    let enabled = match get(ENABLE_ENV).as_deref() {
        None | Some("0" | "false" | "no") => false,
        Some("1" | "true" | "yes") => true,
        Some(_) => return Err(format!("{ENABLE_ENV} has an unsupported value")),
    };
    if !enabled {
        return Ok(None);
    }
    let output_path = get(PATH_ENV)
        .filter(|value| is_safe_basename(value))
        .map(PathBuf::from)
        .ok_or_else(|| format!("{PATH_ENV} must be a safe file name"))?;
    let interval_ms = parse_u64(&get, INTERVAL_ENV, DEFAULT_INTERVAL_MS)?;
    if interval_ms < MIN_INTERVAL_MS {
        return Err(format!(
            "{INTERVAL_ENV} must be at least {MIN_INTERVAL_MS}ms"
        ));
    }
    let max_bytes = parse_u64(&get, MAX_BYTES_ENV, DEFAULT_MAX_BYTES)?;
    if !(MIN_MAX_BYTES..=MAX_MAX_BYTES).contains(&max_bytes) {
        return Err(format!(
            "{MAX_BYTES_ENV} must be between {MIN_MAX_BYTES} and {MAX_MAX_BYTES}"
        ));
    }
    Ok(Some(MemoryDiagnosticsConfig {
        output_path,
        interval: Duration::from_millis(interval_ms),
        max_bytes,
    }))
}

fn parse_u64(
    get: &impl Fn(&str) -> Option<String>,
    key: &str,
    default: u64,
) -> Result<u64, String> {
    get(key)
        .map(|value| {
            value
                .parse()
                .map_err(|_| format!("{key} must be an integer"))
        })
        .unwrap_or(Ok(default))
}

#[derive(Debug, Clone)]
pub struct DiagnosticPhase {
    pub name: &'static str,
    pub transition: bool,
    pub expected_view_models_items: usize,
    pub expected_thread_list_items: usize,
    pub requested_live_bytes: usize,
    pub released_fraction: f64,
    pub scan_count: u64,
    pub last_result_items: usize,
    pub result_checksum: u64,
    pub experimental: bool,
}

impl Default for DiagnosticPhase {
    fn default() -> Self {
        Self {
            name: "runtime",
            transition: false,
            expected_view_models_items: 0,
            expected_thread_list_items: 0,
            requested_live_bytes: 0,
            released_fraction: 0.0,
            scan_count: 0,
            last_result_items: 0,
            result_checksum: 0,
            experimental: false,
        }
    }
}

pub type SharedDiagnosticPhase = Arc<RwLock<DiagnosticPhase>>;

#[derive(Debug, Serialize)]
pub struct MemoryDiagnosticRecord {
    timestamp: String,
    elapsed_ms: u64,
    sample: u64,
    sample_duration_us: u64,
    rss_bytes: usize,
    physical_footprint_bytes: Option<u64>,
    jemalloc_allocated_bytes: usize,
    jemalloc_active_bytes: usize,
    jemalloc_resident_bytes: usize,
    jemalloc_retained_bytes: usize,
    jemalloc_metadata_bytes: usize,
    jemalloc_mapped_bytes: usize,
    view_models_items: usize,
    thread_list_items: usize,
    view_models_shallow_bytes_estimate: usize,
    thread_list_shallow_and_string_capacity_bytes_estimate: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transition: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state_matches_expected_counts: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    phase_sample: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    synthetic_requested_live_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    synthetic_released_fraction: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_view_models_items: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_thread_list_items: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scan_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_result_items: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result_checksum: Option<u64>,
}

pub fn sample_once(
    started_at: Instant,
    sample: u64,
    phase_sample: u64,
    phase: &DiagnosticPhase,
) -> Option<MemoryDiagnosticRecord> {
    let sample_started = Instant::now();
    let allocator = alloc_config::query_breakdown()?;
    let rss_bytes = alloc_config::query_stats()?.current_rss;
    let physical_footprint_bytes = alloc_config::physical_footprint_bytes();
    let (view_models_items, view_models_shallow_bytes_estimate) = {
        let state = VIEW_MODELS.state();
        let guard = state.read();
        let count = guard.items.len();
        (
            count,
            count * size_of::<crate::kit::tui_render_unit::TuiRenderUnit>(),
        )
    };
    let (thread_list_items, thread_list_shallow_and_string_capacity_bytes_estimate) = {
        let state = THREAD_LIST.state();
        let guard = state.read();
        (guard.len(), estimate_thread_list_bytes(&guard))
    };
    let experiment = phase.experimental;
    Some(MemoryDiagnosticRecord {
        timestamp: chrono::Utc::now().to_rfc3339(),
        elapsed_ms: started_at
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX),
        sample,
        sample_duration_us: sample_started
            .elapsed()
            .as_micros()
            .try_into()
            .unwrap_or(u64::MAX),
        rss_bytes,
        physical_footprint_bytes,
        jemalloc_allocated_bytes: allocator.allocated,
        jemalloc_active_bytes: allocator.active,
        jemalloc_resident_bytes: allocator.resident,
        jemalloc_retained_bytes: allocator.retained,
        jemalloc_metadata_bytes: allocator.metadata,
        jemalloc_mapped_bytes: allocator.mapped,
        view_models_items,
        thread_list_items,
        view_models_shallow_bytes_estimate,
        thread_list_shallow_and_string_capacity_bytes_estimate,
        phase: experiment.then_some(phase.name),
        transition: experiment.then_some(phase.transition),
        state_matches_expected_counts: experiment.then_some(
            view_models_items == phase.expected_view_models_items
                && thread_list_items == phase.expected_thread_list_items,
        ),
        phase_sample: experiment.then_some(phase_sample),
        synthetic_requested_live_bytes: experiment.then_some(phase.requested_live_bytes),
        synthetic_released_fraction: experiment.then_some(phase.released_fraction),
        expected_view_models_items: experiment.then_some(phase.expected_view_models_items),
        expected_thread_list_items: experiment.then_some(phase.expected_thread_list_items),
        scan_count: experiment.then_some(phase.scan_count),
        last_result_items: experiment.then_some(phase.last_result_items),
        result_checksum: experiment.then_some(phase.result_checksum),
    })
}

fn estimate_thread_list_bytes(threads: &[ThreadSummary]) -> usize {
    threads.iter().fold(0usize, |total, thread| {
        total
            .saturating_add(size_of::<ThreadSummary>())
            .saturating_add(thread.id.capacity())
            .saturating_add(thread.title.as_ref().map_or(0, String::capacity))
            .saturating_add(thread.cwd.capacity())
    })
}

pub fn spawn_memory_diagnostics(
    config: MemoryDiagnosticsConfig,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    spawn_memory_diagnostics_with_phase(config, Default::default(), shutdown)
}

pub fn spawn_memory_diagnostics_with_phase(
    config: MemoryDiagnosticsConfig,
    phase: SharedDiagnosticPhase,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(error) = run_sampler(config, phase, shutdown).await {
            warn!(error = %error, "memory diagnostics stopped");
        }
    })
}

pub async fn stop_memory_diagnostics(mut handle: tokio::task::JoinHandle<()>, timeout: Duration) {
    if tokio::time::timeout(timeout, &mut handle).await.is_err() {
        handle.abort();
        let _ = handle.await;
    }
}

fn secure_create(path: &Path) -> io::Result<std::fs::File> {
    if path
        .components()
        .any(|component| component == Component::ParentDir)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unsafe diagnostic output path",
        ));
    }
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid diagnostic output parent",
        )
    })?;
    let parent = parent.canonicalize().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "diagnostic output parent unavailable",
        )
    })?;
    if !parent.metadata()?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "diagnostic output parent is not a directory",
        ));
    }
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid diagnostic output name",
        )
    })?;
    let target = parent.join(name);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options
        .open(target)
        .map_err(|error| io::Error::new(error.kind(), "diagnostic output creation failed"))
}

async fn run_sampler(
    config: MemoryDiagnosticsConfig,
    phase: SharedDiagnosticPhase,
    shutdown: CancellationToken,
) -> io::Result<()> {
    if !diagnostics_supported() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "memory diagnostics unsupported",
        ));
    }
    let output = secure_create(&config.output_path)?;
    let mut output = tokio::fs::File::from_std(output);
    let started_at = Instant::now();
    let mut sample = 0;
    let mut phase_sample = 0;
    let mut written = 0_u64;
    let mut previous_phase = phase.read().name;
    let mut interval = tokio::time::interval(config.interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,
            _ = interval.tick() => {}
        }
        let phase = phase.read().clone();
        if phase.name != previous_phase {
            previous_phase = phase.name;
            phase_sample = 0;
        }
        if let Some(record) = sample_once(started_at, sample, phase_sample, &phase) {
            let mut line = serde_json::to_vec(&record).map_err(io::Error::other)?;
            line.push(b'\n');
            let line_len = u64::try_from(line.len()).unwrap_or(u64::MAX);
            if written.saturating_add(line_len) > config.max_bytes {
                warn!(
                    max_bytes = config.max_bytes,
                    "memory diagnostics size limit reached"
                );
                break;
            }
            output.write_all(&line).await?;
            output.flush().await?;
            written += line_len;
            sample = sample.saturating_add(1);
            phase_sample = phase_sample.saturating_add(1);
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "memory_diagnostics_test.rs"]
mod tests;
