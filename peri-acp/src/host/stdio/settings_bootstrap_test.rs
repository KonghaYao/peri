//! Trusted stdio settings bootstrap, before ACP framing begins.

use tokio::io::AsyncWriteExt;

use super::{assemble_stdio_config, read_bootstrap_settings, StdioInput};

struct ConfigPathGuard;

impl Drop for ConfigPathGuard {
    fn drop(&mut self) {
        crate::provider::set_global_config_path(None);
    }
}

#[tokio::test]
async fn settings_bootstrap_is_separate_from_following_acp_frames() {
    let (mut writer, mut reader) = tokio::io::duplex(4096);
    let settings = br#"{"config":{"active_alias":"sonnet"}}"#;
    writer
        .write_all(&(settings.len() as u32).to_be_bytes())
        .await
        .unwrap();
    writer.write_all(settings).await.unwrap();
    writer
        .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"initialize\"}\n")
        .await
        .unwrap();

    let injected = read_bootstrap_settings(&mut reader).await.unwrap();
    assert_eq!(injected, std::str::from_utf8(settings).unwrap());
    let mut acp_line = String::new();
    let mut buffered = tokio::io::BufReader::new(reader);
    tokio::io::AsyncBufReadExt::read_line(&mut buffered, &mut acp_line)
        .await
        .unwrap();
    assert!(acp_line.contains("\"method\":\"initialize\""));
}

#[tokio::test]
async fn settings_bootstrap_rejects_oversized_frame_before_reading_body() {
    let (mut writer, mut reader) = tokio::io::duplex(8);
    writer
        .write_all(&(super::MAX_BOOTSTRAP_SETTINGS_BYTES as u32 + 1).to_be_bytes())
        .await
        .unwrap();
    assert!(read_bootstrap_settings(&mut reader).await.is_err());
}

#[tokio::test]
#[serial_test::serial]
async fn injected_settings_assemble_without_settings_file() {
    let tmp = tempfile::tempdir().unwrap();
    let _guard = ConfigPathGuard;
    crate::provider::set_global_config_path(Some(tmp.path().join("missing-settings.json")));
    let settings = r#"{"config":{"active_alias":"sonnet","providers":[{"id":"injected","type":"openai","apiKey":"test"}],"profiles":{"sonnet":{"provider":"injected","model":"injected-model"}}}}"#;
    let assembled = assemble_stdio_config(
        StdioInput {
            cwd: tmp.path().to_string_lossy().into_owned(),
            settings_stdin: true,
            permission_mode: peri_acp_types::permission::SharedPermissionMode::new(
                peri_acp_types::permission::PermissionMode::Bypass,
            ),
            session_store: peri_acp_types::session_store::SessionStoreDeployment::local_path(
                tmp.path().join("threads.db"),
            ),
        },
        Some(settings),
    )
    .await
    .unwrap();
    assert_eq!(assembled.provider.read().model_name(), "injected-model");
    assert!(!tmp.path().join("missing-settings.json").exists());
}
