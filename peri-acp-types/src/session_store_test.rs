//! 会话存储部署参数的确定性测试：默认值、构造归一、两类定位输入的语义区分与
//! `Debug` 不泄密。

use std::path::PathBuf;

use crate::session_resources::AccessMode;
use crate::session_store::{SessionStoreDeployment, SessionStoreLocator};

#[test]
fn default_is_local_read_write() {
    let deployment = SessionStoreDeployment::default_local();
    assert_eq!(deployment.locator(), &SessionStoreLocator::Default);
    assert!(deployment.engine_name().is_none());
    assert!(deployment.credential_env_name().is_none());
    assert_eq!(deployment.access(), AccessMode::ReadWrite);
    assert_eq!(SessionStoreDeployment::default(), deployment);
}

#[test]
fn builders_carry_locator_engine_credential_and_access() {
    let deployment = SessionStoreDeployment::from_locator("env:TURSO_URL")
        .with_engine("turso")
        .with_credential_env("TURSO_TOEKN")
        .with_access(AccessMode::ReadOnly);
    assert_eq!(
        deployment.locator(),
        &SessionStoreLocator::Locator("env:TURSO_URL".to_owned())
    );
    assert_eq!(deployment.engine_name(), Some("turso"));
    assert_eq!(deployment.credential_env_name(), Some("TURSO_TOEKN"));
    assert_eq!(deployment.access(), AccessMode::ReadOnly);
}

/// `--db-path` 归一后是**已确认本机路径**：平台拼写（Windows drive）原样保留在
/// `PathBuf` 里，不再被表达成一个可能被二次解释的字符串。
#[test]
fn local_path_keeps_platform_path_spelling() {
    let path = PathBuf::from("C:\\work\\threads.db");
    let deployment = SessionStoreDeployment::local_path(path.clone());
    assert_eq!(deployment.locator(), &SessionStoreLocator::LocalPath(path));
    assert!(deployment.engine_name().is_none());
}

/// 同一段字符走两条入口得到不同语义：`--db-path` 是文件名，`--session-store` 是原文。
#[test]
fn db_path_and_session_store_inputs_are_distinct() {
    let as_path = SessionStoreDeployment::local_path(PathBuf::from("env:UNSET"));
    let as_locator = SessionStoreDeployment::from_locator("env:UNSET");

    assert_eq!(
        as_path.locator(),
        &SessionStoreLocator::LocalPath(PathBuf::from("env:UNSET"))
    );
    assert_eq!(
        as_locator.locator(),
        &SessionStoreLocator::Locator("env:UNSET".to_owned())
    );
    assert_ne!(as_path, as_locator);
}

/// Unix 非 UTF-8 文件名：字节在部署参数里原样保留（`to_string_lossy` 会把它换成
/// U+FFFD，进而指向另一个文件）。
#[cfg(unix)]
#[test]
fn local_path_keeps_non_utf8_bytes() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let path = PathBuf::from(OsStr::from_bytes(b"threads-\xff\xfe.db"));
    assert!(path.to_str().is_none(), "用例本身要求非 UTF-8 路径");

    let deployment = SessionStoreDeployment::local_path(path.clone());
    match deployment.locator() {
        SessionStoreLocator::LocalPath(kept) => {
            assert_eq!(kept.as_os_str().as_bytes(), path.as_os_str().as_bytes());
        }
        other => panic!("期望 LocalPath，得到 {other:?}"),
    }
}

#[test]
fn debug_does_not_echo_locator_or_credential_name() {
    let deployment = SessionStoreDeployment::from_locator("turso://sentinel-db-sentinel.turso.io")
        .with_credential_env("PERI_SENTINEL_TOKEN_NAME");
    let rendered = format!("{deployment:?}");

    assert!(rendered.contains("<configured>"));
    assert!(rendered.contains("ReadWrite"));
    assert!(!rendered.contains("sentinel-db-sentinel"));
    assert!(!rendered.contains("PERI_SENTINEL_TOKEN_NAME"));
}

#[test]
fn debug_marks_default_local() {
    let rendered = format!("{:?}", SessionStoreDeployment::default_local());
    assert!(rendered.contains("<default-local>"));
    assert!(rendered.contains("engine_configured: false"));
}

/// 本机路径也不回显取值（路径含用户环境），只标形态。
#[test]
fn debug_does_not_echo_local_path_value() {
    let deployment =
        SessionStoreDeployment::local_path(PathBuf::from("/sentinel-home-sentinel/threads.db"));
    let rendered = format!("{deployment:?}");
    let locator_debug = format!("{:?}", deployment.locator());

    assert!(rendered.contains("<local-path>"));
    assert!(!rendered.contains("sentinel-home-sentinel"));
    assert!(!locator_debug.contains("sentinel-home-sentinel"));
}
