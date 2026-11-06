use super::*;
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
    let first = private_output_dir().expect("private output directory");
    let second = private_output_dir().expect("private output directory");

    assert_eq!(first, second);
    assert_ne!(first, std::env::temp_dir());
    assert!(first
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with(OUTPUT_PREFIX));
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
