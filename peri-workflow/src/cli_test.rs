use std::ffi::OsString;

use serial_test::serial;

use super::{run, run_prepared, validate_args};
use crate::runner::{repository_package_dir, repository_workflow_command};

/// 让生产解析路径（`PERI_WORKFLOW_ARTIFACT_DIR` 注入点）指向仓库入库 bundle。
///
/// 调用方必须 `#[serial]`：进程级 env 与 `std::env` 读取存在并发窗口。
fn use_repository_bundle() {
    std::env::set_var("PERI_WORKFLOW_ARTIFACT_DIR", repository_package_dir());
}

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
async fn workflow_cli_spawns_repository_bundle_for_help() {
    let exit_code = run_prepared(&repository_workflow_command(), &[OsString::from("--help")])
        .await
        .unwrap();
    assert_eq!(exit_code, 0);
}

#[tokio::test]
async fn workflow_cli_propagates_validate_failure() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("missing-script.mjs");
    let exit_code = run_prepared(
        &repository_workflow_command(),
        &[OsString::from("validate"), missing.into_os_string()],
    )
    .await
    .unwrap();
    assert_ne!(exit_code, 0);
}

/// 部署注入点经生产入口 `run` 生效：显式 `PERI_WORKFLOW_ARTIFACT_DIR` 即可离线执行。
#[tokio::test]
#[serial]
async fn workflow_cli_uses_explicit_artifact_dir_override() {
    use_repository_bundle();
    let exit_code = run(&[OsString::from("--help")]).await.unwrap();
    std::env::remove_var("PERI_WORKFLOW_ARTIFACT_DIR");
    assert_eq!(exit_code, 0);
}
