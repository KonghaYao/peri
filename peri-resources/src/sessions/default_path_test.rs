use super::{default_database_path, open_thread_store_read_only, ReadOnlyStoreErrorKind};
use std::path::PathBuf;

#[tokio::test]
async fn test_default_database_path_and_readonly_open_create_nothing() {
    let home = tempfile::tempdir().unwrap();
    let output = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "sessions::default_path_tests::test_default_database_path_child_process",
            "--nocapture",
        ])
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("PERI_TEST_DEFAULT_DATABASE_HOME", home.path())
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("running 1 test"));
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn test_default_database_path_child_process() {
    // HOME only changes in the child process, never in the parallel test runner.
    let Some(home) = std::env::var_os("PERI_TEST_DEFAULT_DATABASE_HOME") else {
        return;
    };
    let home = PathBuf::from(home);
    let expected = home.join(".peri").join("threads").join("threads.db");
    assert_eq!(default_database_path(), Some(expected));
    assert_eq!(std::fs::read_dir(&home).unwrap().count(), 0);
    match open_thread_store_read_only(None).await {
        Ok(_) => panic!("missing default database must not be created by read-only open"),
        Err(error) => assert_eq!(error.kind(), ReadOnlyStoreErrorKind::DatabaseNotFound),
    }
    assert_eq!(std::fs::read_dir(&home).unwrap().count(), 0);
}
