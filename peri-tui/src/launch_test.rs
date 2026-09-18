use super::*;
use peri_acp::transport::mpsc::mpsc_transport_pair;

#[tokio::test]
async fn attach_acp_rejects_second_attachment_before_spawning_host() {
    let temp = tempfile::tempdir().expect("temp db directory");
    const CHILD_ENV: &str = "PERI_TEST_DUPLICATE_ACP_ATTACH";
    const TEST_NAME: &str =
        "launch::tests::attach_acp_rejects_second_attachment_before_spawning_host";
    if std::env::var_os(CHILD_ENV).is_none() {
        // App::new reads config and environment before the duplicate guard.
        // Isolate those process-wide inputs, not just its SQLite database.
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", TEST_NAME, "--nocapture"])
            .env_clear()
            .env(CHILD_ENV, "1")
            .env("HOME", temp.path())
            .current_dir(temp.path());
        #[cfg(windows)]
        for key in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        let output = command.output().expect("isolated test process");
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        return;
    }
    // Explicit config redirection also isolates Windows, where dirs_next
    // resolves the system profile independently of the HOME override.
    crate::config::set_global_config_path(Some(temp.path().join("settings.json")));
    let db_path = temp.path().join("threads.db");
    let mut app = App::new(Some(db_path)).await.expect("app");
    let (client_transport, _server_transport) = mpsc_transport_pair();
    let (client, _notification_tx, _notification_rx) =
        AcpTuiClient::new_interactive(client_transport);
    app.acp_client = Some(client);

    let error = match attach_acp(&mut app).await {
        Ok(_) => panic!("duplicate attach unexpectedly succeeded"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("already attached"),
        "unexpected duplicate attachment error: {error}"
    );
}
