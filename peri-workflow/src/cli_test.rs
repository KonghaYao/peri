use std::ffi::OsString;

use super::validate_args;

#[test]
fn workflow_cli_accepts_stable_subcommands() {
    for command in [
        "read", "list", "validate", "boundary", "adlc", "help", "--help",
    ] {
        validate_args(&[OsString::from(command)]).unwrap();
    }
}

#[test]
fn workflow_cli_rejects_empty_and_unknown_subcommands() {
    assert!(validate_args(&[]).is_err());
    assert!(validate_args(&[OsString::from("run")]).is_err());
}

#[tokio::test]
async fn workflow_cli_spawns_embedded_artifact_for_help() {
    let exit_code = super::run(&[OsString::from("--help")]).await.unwrap();
    assert_eq!(exit_code, 0);
}

#[tokio::test]
async fn workflow_cli_propagates_validate_failure() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("missing-script.mjs");
    let exit_code = super::run(&[OsString::from("validate"), missing.into_os_string()])
        .await
        .unwrap();
    assert_ne!(exit_code, 0);
}
