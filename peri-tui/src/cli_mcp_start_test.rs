use super::worker_path;

#[test]
fn worker_must_be_packaged_beside_peri() {
    let directory = tempfile::tempdir().unwrap();
    let peri = directory.path().join("peri");
    let error = worker_path(peri).unwrap_err().to_string();
    assert!(error.contains("Workspace MCP executable is missing"));
}

#[test]
fn worker_resolution_uses_the_executable_directory() {
    let directory = tempfile::tempdir().unwrap();
    let peri = directory.path().join("peri");
    let worker = directory.path().join(super::WORKSPACE_BINARY);
    std::fs::write(&worker, "").unwrap();
    assert_eq!(worker_path(peri).unwrap(), worker);
}
