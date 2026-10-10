use serde_json::json;

use crate::{
    acp_client::AcpTuiClient,
    i18n,
    kit::atoms::{ACP_CLIENT_HANDLE, ACTIVE_SESSION_ID, CRON_ACTION_ERROR},
};

pub(super) fn dispatch(method: &'static str, id: &str) {
    let Some(client) = ACP_CLIENT_HANDLE.get().cloned() else {
        fail(i18n::tr("panel-cron-no-client"));
        return;
    };
    let Some(identity) = client
        .stable_session_identity()
        .filter(|owner| ACTIVE_SESSION_ID.state().read().as_str() == owner.0)
    else {
        fail(i18n::tr("panel-cron-no-session"));
        return;
    };
    CRON_ACTION_ERROR.set(None);
    let id = id.to_string();
    tokio::spawn(async move {
        let result = request(&client, &identity, method, &id).await;
        if client.stable_session_identity().as_ref() != Some(&identity)
            || ACTIVE_SESSION_ID.state().read().as_str() != identity.0
        {
            if let Err(error) = result {
                tracing::warn!(%error, method, "stale cron operation failed");
            }
            return;
        }
        match result {
            Ok(()) => CRON_ACTION_ERROR.set(None),
            Err(error) => fail(i18n::tr_args(
                "panel-cron-action-failed",
                &[(
                    "detail".into(),
                    fluent_bundle::FluentValue::from(error.to_string()),
                )],
            )),
        }
    });
}

async fn request(
    client: &AcpTuiClient,
    identity: &(String, u64),
    method: &str,
    id: &str,
) -> Result<(), peri_acp::transport::types::AcpError> {
    let response = client
        .send_session_request(
            identity,
            method,
            json!({
                "sessionId": identity.0, "id": id,
            }),
        )
        .await?;
    if response.get("success").and_then(serde_json::Value::as_bool) != Some(true)
        || response.get("id").and_then(serde_json::Value::as_str) != Some(id)
    {
        return Err(peri_acp::transport::types::AcpError::new(
            -32603,
            "invalid cron operation response",
        ));
    }
    Ok(())
}

fn fail(message: String) {
    tracing::warn!(%message, "cron panel operation unavailable");
    CRON_ACTION_ERROR.set(Some(message));
}

#[cfg(test)]
#[path = "actions_test.rs"]
mod tests;
