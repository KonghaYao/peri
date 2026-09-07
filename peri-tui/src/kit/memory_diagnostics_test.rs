use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use crate::kit::atoms;

use super::*;

fn parse(values: &[(&str, &str)]) -> Result<Option<MemoryDiagnosticsConfig>, String> {
    let values: HashMap<_, _> = values.iter().copied().collect();
    parse_config(|key| values.get(key).map(ToString::to_string))
}

fn config(output_path: PathBuf, max_bytes: u64) -> MemoryDiagnosticsConfig {
    MemoryDiagnosticsConfig {
        output_path,
        interval: Duration::from_millis(MIN_INTERVAL_MS),
        max_bytes,
    }
}

#[test]
fn production_enable_truth_table_rejects_experiment() {
    for disabled in [None, Some("0"), Some("false"), Some("no")] {
        let result = parse_config(|key| {
            (key == ENABLE_ENV)
                .then(|| disabled.map(str::to_string))
                .flatten()
        });
        assert_eq!(result.unwrap(), None);
    }
    for enabled in ["1", "true", "yes"] {
        assert!(
            parse(&[(ENABLE_ENV, enabled), (PATH_ENV, "new.jsonl")])
                .unwrap()
                .is_some()
        );
    }
    assert!(parse(&[(ENABLE_ENV, "experiment"), (PATH_ENV, "new.jsonl")]).is_err());
}

#[test]
fn enabled_without_path_fails_closed() {
    let error = parse(&[(ENABLE_ENV, "1")]).unwrap_err();
    assert!(error.contains(PATH_ENV), "unexpected error: {error}");
}

#[test]
fn valid_config_uses_requested_values() {
    let config = parse(&[
        (ENABLE_ENV, "true"),
        (PATH_ENV, "peri-memory.jsonl"),
        (INTERVAL_ENV, "750"),
        (MAX_BYTES_ENV, "4096"),
    ])
    .unwrap()
    .unwrap();
    assert_eq!(config.output_path, PathBuf::from("peri-memory.jsonl"));
    assert_eq!(config.interval, Duration::from_millis(750));
    assert_eq!(config.max_bytes, 4096);
}

#[test]
fn invalid_interval_and_size_limits_are_rejected() {
    assert!(
        parse(&[
            (ENABLE_ENV, "yes"),
            (PATH_ENV, "/tmp/x"),
            (INTERVAL_ENV, "249")
        ])
        .is_err()
    );
    for value in ["0", "1023", "1073741825", "invalid"] {
        assert!(
            parse(&[
                (ENABLE_ENV, "yes"),
                (PATH_ENV, "/tmp/x"),
                (MAX_BYTES_ENV, value)
            ])
            .is_err()
        );
    }
}

#[test]
fn production_record_uses_explicit_field_allowlist() {
    atoms::init_atoms();
    let record = sample_once(Instant::now(), 0, 0, &DiagnosticPhase::default()).unwrap();
    let value = serde_json::to_value(record).unwrap();
    let actual: BTreeSet<_> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let expected = BTreeSet::from([
        "elapsed_ms",
        "jemalloc_active_bytes",
        "jemalloc_allocated_bytes",
        "jemalloc_mapped_bytes",
        "jemalloc_metadata_bytes",
        "jemalloc_resident_bytes",
        "jemalloc_retained_bytes",
        "physical_footprint_bytes",
        "rss_bytes",
        "sample",
        "sample_duration_us",
        "thread_list_items",
        "thread_list_shallow_and_string_capacity_bytes_estimate",
        "timestamp",
        "view_models_items",
        "view_models_shallow_bytes_estimate",
    ]);
    assert_eq!(actual, expected);
}

#[test]
fn secure_create_rejects_existing_parent_escape_and_non_regular_targets() {
    let dir = tempfile::tempdir().unwrap();
    let existing = dir.path().join("existing.jsonl");
    std::fs::write(&existing, b"old").unwrap();
    assert!(secure_create(&existing).is_err());
    assert!(secure_create(&dir.path().join("../escape.jsonl")).is_err());
    assert!(secure_create(dir.path()).is_err());
}

#[cfg(unix)]
#[test]
fn secure_create_rejects_symlink_target() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    std::fs::write(&target, b"old").unwrap();
    let link = dir.path().join("link");
    symlink(target, &link).unwrap();
    assert!(secure_create(&link).is_err());
}

#[tokio::test]
async fn size_limit_stops_before_partial_line() {
    atoms::init_atoms();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("limited.jsonl");
    let shutdown = CancellationToken::new();
    run_sampler(
        config(path.clone(), MIN_MAX_BYTES),
        Default::default(),
        shutdown,
    )
    .await
    .unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert!(bytes.len() <= MIN_MAX_BYTES as usize);
    for line in bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        serde_json::from_slice::<serde_json::Value>(line).unwrap();
    }
}

#[tokio::test]
async fn cancellation_leaves_only_complete_json_lines() {
    atoms::init_atoms();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cancel.jsonl");
    let shutdown = CancellationToken::new();
    let handle = spawn_memory_diagnostics(config(path.clone(), 64 * 1024), shutdown.clone());
    tokio::time::sleep(Duration::from_millis(20)).await;
    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(1), handle)
        .await
        .unwrap()
        .unwrap();
    let text = std::fs::read_to_string(path).unwrap();
    assert!(text.ends_with('\n'));
    for line in text.lines() {
        serde_json::from_str::<serde_json::Value>(line).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn parent_directory_policy_accepts_readable_but_not_writable_shared_modes() {
    let uid = unsafe { libc::geteuid() };
    for mode in [0o700, 0o750, 0o755] {
        assert!(unix_directory_metadata_is_safe(
            uid,
            mode,
            uid,
            DirectoryPolicy::PrivateParent
        ));
    }
    for mode in [0o720, 0o702, 0o777] {
        assert!(!unix_directory_metadata_is_safe(
            uid,
            mode,
            uid,
            DirectoryPolicy::PrivateParent
        ));
    }
    assert!(!unix_directory_metadata_is_safe(
        uid.wrapping_add(1),
        0o700,
        uid,
        DirectoryPolicy::PrivateParent
    ));
    assert!(!unix_directory_metadata_is_safe(
        uid,
        0o750,
        uid,
        DirectoryPolicy::PrivateDiagnostics
    ));
}

#[test]
fn production_path_accepts_only_safe_basename() {
    for value in ["", ".", "..", "a/b", "a\\b", "/tmp/x"] {
        assert!(parse(&[(ENABLE_ENV, "yes"), (PATH_ENV, value)]).is_err());
    }
    assert!(parse(&[(ENABLE_ENV, "yes"), (PATH_ENV, "memory.jsonl")]).is_ok());
}

#[tokio::test]
async fn stop_helper_aborts_and_finishes_timed_out_task() {
    let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let task_finished = finished.clone();
    let handle = tokio::spawn(async move {
        struct FinishGuard(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for FinishGuard {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let _guard = FinishGuard(task_finished);
        std::future::pending::<()>().await;
    });
    tokio::task::yield_now().await;
    stop_memory_diagnostics(handle, Duration::from_millis(1)).await;
    assert!(finished.load(std::sync::atomic::Ordering::SeqCst));
}

#[test]
fn platform_support_matches_windows_fail_closed_policy() {
    assert_eq!(diagnostics_supported(), !cfg!(target_os = "windows"));
}
