use std::fs;

use tempfile::TempDir;

use super::*;

fn logical_id(value: &str) -> LogicalId {
    LogicalId::parse(value).expect("logical id")
}

#[test]
fn omitted_cwd_is_created_under_the_workspace_root() {
    let root = TempDir::new().expect("root");
    let canonical_root = fs::canonicalize(root.path()).expect("canonical");
    let resolved =
        resolve_cwd(None, &canonical_root, &logical_id("agent-01"), true).expect("resolved");
    assert!(resolved.is_dir());
    assert_eq!(resolved, canonical_root.join("agent-01"));
}

#[test]
fn explicit_cwd_is_canonicalized() {
    let root = TempDir::new().expect("root");
    let canonical_root = fs::canonicalize(root.path()).expect("canonical");
    let requested = root.path().to_string_lossy().into_owned();
    let resolved = resolve_cwd(
        Some(&requested),
        &canonical_root,
        &logical_id("agent-01"),
        false,
    )
    .expect("resolved");
    assert_eq!(resolved, canonical_root);
}

#[test]
fn relative_cwd_is_rejected() {
    let root = TempDir::new().expect("root");
    let canonical_root = fs::canonicalize(root.path()).expect("canonical");
    let error = resolve_cwd(
        Some("relative/path"),
        &canonical_root,
        &logical_id("agent-01"),
        false,
    )
    .expect_err("relative");
    assert_eq!(error.code(), crate::domain::ErrorCode::CwdNotNormalizable);
}

#[test]
fn missing_cwd_is_not_found() {
    let root = TempDir::new().expect("root");
    let canonical_root = fs::canonicalize(root.path()).expect("canonical");
    let missing = canonical_root.join("missing");
    let error = resolve_cwd(
        Some(&missing.to_string_lossy()),
        &canonical_root,
        &logical_id("agent-01"),
        false,
    )
    .expect_err("missing");
    assert_eq!(error.code(), crate::domain::ErrorCode::CwdNotFound);
}

#[test]
fn file_cwd_is_not_normalizable() {
    let root = TempDir::new().expect("root");
    let canonical_root = fs::canonicalize(root.path()).expect("canonical");
    let file = canonical_root.join("file.txt");
    fs::write(&file, "not a directory").expect("write");
    let error = resolve_cwd(
        Some(&file.to_string_lossy()),
        &canonical_root,
        &logical_id("agent-01"),
        false,
    )
    .expect_err("file");
    assert_eq!(error.code(), crate::domain::ErrorCode::CwdNotNormalizable);
}
