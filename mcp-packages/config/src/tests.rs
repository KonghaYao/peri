use std::io;

use crate::ConfigurationClient;

struct RemoteConfiguration;

impl rmcp::ServerHandler for RemoteConfiguration {
    async fn on_custom_request(
        &self,
        request: rmcp::model::CustomRequest,
        _: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::CustomResult, rmcp::ErrorData> {
        use peri_acp_types::configuration::{
            ConfigurationErrorKind, ConfigurationFailure, ConfigurationRequest,
            ConfigurationResponse, ConfigurationValue,
        };
        let request: ConfigurationRequest =
            serde_json::from_value(request.params.unwrap()).unwrap();
        let response: ConfigurationResponse = match request {
            ConfigurationRequest::ReadText { path } if path.ends_with("denied.json") => {
                Err(ConfigurationFailure {
                    kind: ConfigurationErrorKind::PermissionDenied,
                    message: "remote denied".into(),
                })
            }
            ConfigurationRequest::ReadText { .. } => {
                Ok(ConfigurationValue::Text("remote configuration".into()))
            }
            ConfigurationRequest::WriteTextAtomic { .. } => Ok(ConfigurationValue::Written),
            _ => Ok(ConfigurationValue::Bool(false)),
        };
        Ok(rmcp::model::CustomResult::new(
            serde_json::to_value(response).unwrap(),
        ))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_configuration_never_reads_or_writes_the_callers_filesystem() {
    use rmcp::ServiceExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (shutdown, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let service = RemoteConfiguration.serve(stream).await.unwrap();
        stopped.await.unwrap();
        service.cancel().await.unwrap();
    });
    let directory = tempfile::tempdir().unwrap();
    let file_path = directory.path().join("settings.json");
    let denied = directory.path().join("denied.json");
    std::fs::write(&file_path, "host configuration").unwrap();
    std::fs::write(&denied, "host secret").unwrap();
    let client = ConfigurationClient::connect_tcp(address.to_string()).unwrap();
    crate::install_client(client.clone()).unwrap();
    assert_eq!(
        crate::read_text(&file_path).unwrap(),
        "remote configuration"
    );
    assert_eq!(
        crate::install_client(client.clone()).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(
        client.read_text(&file_path).unwrap(),
        "remote configuration"
    );
    assert_eq!(
        client.read_text(&denied).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    client
        .write_text_atomic(&file_path, "remote change")
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&file_path).unwrap(),
        "host configuration"
    );
    assert!(!client.exists(&file_path).unwrap());
    shutdown.send(()).unwrap();
    drop(client);
    tokio::time::timeout(std::time::Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
    assert!(crate::read_text(&file_path).is_err());
    assert_eq!(
        std::fs::read_to_string(&file_path).unwrap(),
        "host configuration"
    );
}

#[test]
fn configuration_round_trip_preserves_text_errors_and_atomic_replacement() {
    let directory = tempfile::tempdir().unwrap();
    let file_path = directory.path().join("nested/settings.json");
    let client = ConfigurationClient::local().unwrap();
    assert!(!client.exists(&file_path).unwrap());
    assert_eq!(
        client.read_text(&file_path).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    let content = "{\"config\":{\"token\":\"秘密\"},\"unknown\":42}\n";
    client.write_text_atomic(&file_path, content).unwrap();
    assert!(client.exists(&file_path).unwrap());
    assert_eq!(client.read_text(&file_path).unwrap(), content);
    client.write_text_atomic(&file_path, "replacement").unwrap();
    assert_eq!(client.read_text(&file_path).unwrap(), "replacement");
    assert_eq!(
        std::fs::read_dir(file_path.parent().unwrap())
            .unwrap()
            .count(),
        1
    );
    let invalid = file_path.join("child");
    assert!(client.write_text_atomic(&invalid, "no").is_err());
    assert_eq!(client.read_text(&file_path).unwrap(), "replacement");
}

#[cfg(unix)]
#[test]
fn same_file_is_resolved_by_the_provider() {
    let directory = tempfile::tempdir().unwrap();
    let file_path = directory.path().join("settings.json");
    let link = directory.path().join("linked.json");
    std::fs::write(&file_path, "config").unwrap();
    std::os::unix::fs::symlink(&file_path, &link).unwrap();
    let client = ConfigurationClient::local().unwrap();
    assert!(client.same_file(&file_path, &link).unwrap());
    assert!(!client
        .same_file(&file_path, &directory.path().join("missing"))
        .unwrap());
}

#[tokio::test(flavor = "current_thread")]
async fn synchronous_client_does_not_require_the_callers_runtime_to_progress() {
    let directory = tempfile::tempdir().unwrap();
    let client = ConfigurationClient::local().unwrap();
    let file_path = directory.path().join("settings.json");
    client.write_text_atomic(&file_path, "bootstrap").unwrap();
    assert_eq!(client.read_text(&file_path).unwrap(), "bootstrap");
}

#[test]
fn shared_configuration_path_redirect_can_be_reset() {
    let client = ConfigurationClient::local().unwrap();
    let original = client.paths().unwrap().global_settings;
    let directory = tempfile::tempdir().unwrap();
    let redirect = directory.path().join("settings.json");
    client
        .set_global_config_path(Some(redirect.clone()))
        .unwrap();
    assert_eq!(client.paths().unwrap().global_settings, redirect);
    client.set_global_config_path(None).unwrap();
    assert_eq!(client.paths().unwrap().global_settings, original);
}
