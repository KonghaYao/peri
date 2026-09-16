//! Permission controls address the active session through the ACP lifecycle gate.

use crate::{acp_client::AcpTuiClient, kit::atoms};

pub(crate) fn request(mode: &str) {
    if let Some(client) = atoms::ACP_CLIENT_HANDLE.get() {
        send(client.as_ref().clone(), mode.to_string());
    }
}

pub(crate) fn cycle(client: AcpTuiClient) {
    let mode = atoms::SERVICE_SNAPSHOT
        .state()
        .read()
        .permission_mode
        .clone();
    let next = match mode.as_str() {
        "default" => "accept-edit",
        "accept-edit" => "auto-mode",
        "auto-mode" => "bypass",
        _ => "default",
    };
    send(client, next.into());
}

fn send(client: AcpTuiClient, mode: String) {
    tokio::spawn(async move {
        if let Err(error) = client.set_mode(&mode).await {
            atoms::NOTIFICATION.set(Some(atoms::Notification {
                message: crate::i18n::tr_args(
                    "permission-mode-update-failed",
                    &[("error".into(), error.to_string().into())],
                ),
                until: std::time::Instant::now() + std::time::Duration::from_secs(10),
            }));
        }
    });
}
