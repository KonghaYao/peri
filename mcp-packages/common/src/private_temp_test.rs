use super::*;
#[cfg(unix)]
use std::time::{Duration, SystemTime};

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[cfg(unix)]
#[test]
fn created_directory_and_file_are_owner_only() {
    let parent = tempfile::tempdir().unwrap();
    let dir = create_private_dir(parent.path()).unwrap();
    let path = write_private_file(&dir.path, "payload.txt", b"secret").unwrap();

    assert_eq!(mode(&dir.path), 0o700);
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(&dir.path.join(LOCK_FILE)), 0o600);
    assert_eq!(std::fs::read(&path).unwrap(), b"secret");
}

#[test]
fn persisted_directory_is_process_private_and_stable() {
    let first = write_output("first").expect("private output file");
    let second = write_output("second").expect("private output file");
    let first_dir = first.parent().expect("file parent");
    let second_dir = second.parent().expect("file parent");

    assert_eq!(first_dir, second_dir);
    assert_ne!(first_dir, std::env::temp_dir());
    assert!(first_dir
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with(OUTPUT_PREFIX));
}

#[cfg(unix)]
#[test]
fn initialization_failure_is_retried_after_recovery() {
    use std::os::unix::fs::PermissionsExt;

    // Root bypasses directory permissions, so the injected failure would not
    // occur there.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }

    let parent = tempfile::tempdir().unwrap();
    let cache = PrivateDirCache::default();

    std::fs::set_permissions(parent.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let error = write_output_to(&cache, parent.path(), "first").unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(cache.dir.lock().unwrap().is_none());

    std::fs::set_permissions(parent.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = write_output_to(&cache, parent.path(), "recovered").expect("retry after recovery");

    assert_eq!(std::fs::read(&path).unwrap(), b"recovered");
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(path.parent().unwrap()), 0o700);
}

#[test]
fn existing_file_is_never_overwritten() {
    let parent = tempfile::tempdir().unwrap();
    let dir = create_private_dir(parent.path()).unwrap();
    write_private_file(&dir.path, "payload.txt", b"first").unwrap();

    let error = write_private_file(&dir.path, "payload.txt", b"second").unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(
        std::fs::read(dir.path.join("payload.txt")).unwrap(),
        b"first"
    );
}

#[cfg(unix)]
#[test]
fn sweep_removes_abandoned_directories_only() {
    let parent = tempfile::tempdir().unwrap();
    let stale = parent.path().join(format!("{OUTPUT_PREFIX}stale"));
    std::fs::create_dir(&stale).unwrap();
    std::fs::write(stale.join("payload.txt"), b"old").unwrap();
    age_directory(&stale, DIR_RETENTION + Duration::from_secs(60));

    let fresh = parent.path().join(format!("{OUTPUT_PREFIX}fresh"));
    std::fs::create_dir(&fresh).unwrap();

    let live = parent.path().join(format!("{OUTPUT_PREFIX}live"));
    std::fs::create_dir(&live).unwrap();
    let lock = std::fs::File::create(live.join(LOCK_FILE)).unwrap();
    lock.try_lock().unwrap();
    age_directory(&live, DIR_RETENTION + Duration::from_secs(60));

    let unrelated = parent.path().join("peri-other-data");
    std::fs::create_dir(&unrelated).unwrap();
    age_directory(&unrelated, DIR_RETENTION + Duration::from_secs(60));

    sweep_stale_dirs(parent.path());

    assert!(
        !stale.exists(),
        "abandoned output directory should be removed"
    );
    assert!(fresh.exists(), "recent output directory should be kept");
    assert!(
        live.exists(),
        "locked output directory belongs to a live process"
    );
    assert!(unrelated.exists(), "unrelated temp entries are not touched");
    drop(lock);
}

#[cfg(unix)]
fn age_directory(directory: &Path, age: Duration) {
    // Opening a directory is Unix-only, which is why the sweep tests are too.
    let file = std::fs::File::open(directory).unwrap();
    file.set_modified(SystemTime::now() - age).unwrap();
}
