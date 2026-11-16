use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serial_test::serial;

use crate::runner::WorkflowCommand;

/// 合成引擎入口：`--help` 退出 0，`validate` 退出 3（用于验证 CLI 包装层的退出码传播，
/// 不依赖任何真实引擎产物；仓库不读工作树 dist）。
fn stub_command(dir: &Path) -> WorkflowCommand {
    let entry = dir.join("stub-engine.mjs");
    std::fs::write(
        &entry,
        "process.exit(process.argv[2] === 'validate' ? 3 : 0)\n",
    )
    .expect("the test fixture file must be writable");
    WorkflowCommand {
        program: "node".into(),
        args: vec![entry.to_string_lossy().into_owned()],
    }
}

#[test]
fn workflow_cli_accepts_stable_subcommands() {
    for command in [
        "read", "list", "validate", "boundary", "adlc", "help", "--help",
    ] {
        crate::cli::validate_args(&[OsString::from(command)])
            .expect("the validate args fixture result is required for this assertion");
    }
}

#[test]
fn workflow_cli_rejects_empty_and_unknown_subcommands() {
    assert!(crate::cli::validate_args(&[]).is_err());
    assert!(crate::cli::validate_args(&[OsString::from("run")]).is_err());
}

#[tokio::test]
async fn workflow_cli_spawns_prepared_artifact_for_help() {
    let tmp =
        tempfile::TempDir::new().expect("the new fixture result is required for this assertion");
    let exit_code =
        crate::cli::run_prepared(&stub_command(tmp.path()), &[OsString::from("--help")])
            .await
            .expect("the run prepared fixture result is required for this assertion");
    assert_eq!(exit_code, 0);
}

#[tokio::test]
async fn workflow_cli_propagates_validate_failure() {
    let tmp =
        tempfile::TempDir::new().expect("the new fixture result is required for this assertion");
    let missing = tmp.path().join("missing-script.mjs");
    let exit_code = crate::cli::run_prepared(
        &stub_command(tmp.path()),
        &[OsString::from("validate"), missing.into_os_string()],
    )
    .await
    .expect("the run prepared fixture result is required for this assertion");
    assert_ne!(exit_code, 0);
}

/// 生产入口在没有任何已校验 artifact 时 fail closed（显式注入点被采纳，但不匹配即拒绝）。
///
/// 隔离 `HOME`（缓存前缀）并把注入点指向非法包，确保本用例与机器上既有安装无关。
#[tokio::test]
#[serial]
async fn workflow_cli_fails_closed_without_any_validated_artifact() {
    let tmp =
        tempfile::TempDir::new().expect("the new fixture result is required for this assertion");
    let invalid = tmp.path().join("not-a-workflow-package");
    std::fs::create_dir_all(&invalid)
        .expect("the create dir all fixture result is required for this assertion");
    let previous_home = std::env::var_os("HOME");
    std::env::set_var("HOME", tmp.path());
    std::env::set_var("PERI_WORKFLOW_ARTIFACT_DIR", &invalid);

    let result = crate::cli::run(&[OsString::from("--help")]).await;

    match previous_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
    std::env::remove_var("PERI_WORKFLOW_ARTIFACT_DIR");

    let error = result.expect_err("无任何已校验 artifact 时 CLI 必须 fail closed");
    assert!(
        error.to_string().contains("validated workflow artifact"),
        "错误须指出 artifact 不可用: {error}"
    );
}

/// 合成入口的路径必须真实存在才可能被 spawn（防夹具失真）。
#[test]
fn stub_engine_entry_exists() {
    let tmp =
        tempfile::TempDir::new().expect("the new fixture result is required for this assertion");
    let command = stub_command(tmp.path());
    assert!(PathBuf::from(&command.args[0]).is_file());
}
