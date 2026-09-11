//! Host OAuth subscription and independently negotiated safe/legacy delivery.

use std::sync::Arc;

use super::{task_scope, AcpServerConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct OAuthDeliveryPolicy {
    pub(super) safe: bool,
    pub(super) legacy: bool,
}

pub(super) fn oauth_delivery_policy(caps: &peri_acp_types::PeriCaps) -> OAuthDeliveryPolicy {
    OAuthDeliveryPolicy {
        safe: caps.oauth,
        legacy: caps.agent_event,
    }
}

async fn send_safe_oauth_event(
    transport: &Arc<dyn crate::transport::AcpTransport>,
    notification: Result<
        crate::event::oauth::OAuthWireNotification,
        crate::event::oauth::OAuthWireError,
    >,
) {
    match notification {
        Ok(notification) => {
            if let Err(error) = transport
                .send_notification("peri/oauth", notification.into_params())
                .await
            {
                tracing::debug!(error = %error, "safe OAuth notification send failed");
            }
        }
        Err(error) => tracing::warn!(
            error = %error,
            "OAuth notification rejected by safe wire boundary"
        ),
    }
}

async fn send_legacy_oauth_event(
    transport: &Arc<dyn crate::transport::AcpTransport>,
    event: crate::event::AcpEvent,
) {
    let event_json = match serde_json::to_string(&event) {
        Ok(json) => json,
        Err(error) => {
            tracing::error!(error = %error, "legacy OAuth event serialize failed");
            return;
        }
    };
    if let Err(error) = transport
        .send_notification(
            "peri/agent_event",
            serde_json::json!({
                "sessionId": "",
                "event_json": event_json,
            }),
        )
        .await
    {
        tracing::debug!(error = %error, "legacy OAuth notification send failed");
    }
}

pub(super) fn spawn_oauth_consumer(
    oauth_event_rx: Option<
        tokio::sync::mpsc::UnboundedReceiver<crate::event::oauth::HostOAuthEvent>,
    >,
    transport: &Arc<dyn crate::transport::AcpTransport>,
    cfg: &AcpServerConfig,
) {
    if let Some(mut rx) = oauth_event_rx {
        let oauth_transport = Arc::clone(transport);
        let oauth_sessions = cfg.session_manager.clone();
        let dynamic_mcp = cfg.dynamic_mcp.clone();
        let shutdown = cfg.host_task_spawner.shutdown_token();
        let _ = cfg.host_task_spawner.spawn(
            task_scope::HostTaskOwnerKind::Host,
            task_scope::HostTaskKind::OAuthConsumer,
            async move {
                loop {
                    let event = tokio::select! {
                        _ = shutdown.cancelled() => break,
                        event = rx.recv() => match event { Some(event) => event, None => break },
                    };
                    let caps = oauth_sessions.effective_host_caps();
                    let policy = oauth_delivery_policy(&caps);
                    deliver_oauth_event(event, policy, &oauth_transport, dynamic_mcp.as_ref())
                        .await;
                }
            },
        );
    }
}

async fn deliver_oauth_event(
    event: crate::event::oauth::HostOAuthEvent,
    policy: OAuthDeliveryPolicy,
    oauth_transport: &Arc<dyn crate::transport::AcpTransport>,
    dynamic_mcp: Option<&Arc<dyn peri_acp_types::ports::DynamicMcpDeploymentPort>>,
) {
    match event {
        crate::event::oauth::HostOAuthEvent::DynamicAuthorizationNeeded {
            instance,
            flow_id,
            server_name: _,
            authorization_url,
        } => {
            let Some(deployment) = dynamic_mcp else {
                return;
            };
            if policy.safe {
                let _ =
                    deployment.notify_authorization_needed(&instance, &flow_id, &authorization_url);
            }
        }
        crate::event::oauth::HostOAuthEvent::AuthorizationNeeded {
            flow_id,
            server_name,
            authorization_url,
        } => {
            if policy.safe {
                match crate::event::oauth::OAuthWireNotification::authorization_needed(
                    flow_id.clone(),
                    server_name.clone(),
                    authorization_url.clone(),
                ) {
                    Ok(notification) => {
                        let _ = oauth_transport
                            .send_notification("peri/oauth", notification.into_params())
                            .await;
                    }
                    Err(error) => tracing::warn!(
                        error = %error,
                        "OAuth notification rejected by safe wire boundary"
                    ),
                }
            }
            if policy.legacy {
                send_legacy_oauth_event(
                    oauth_transport,
                    crate::event::AcpEvent::OauthNeeded {
                        server_name,
                        auth_url: authorization_url,
                    },
                )
                .await;
            }
        }
        crate::event::oauth::HostOAuthEvent::Completed {
            flow_id,
            server_name,
        } => {
            if policy.safe {
                send_safe_oauth_event(
                    oauth_transport,
                    crate::event::oauth::OAuthWireNotification::terminal(
                        flow_id,
                        server_name.clone(),
                        crate::event::oauth::OAuthWireStatus::Completed,
                    ),
                )
                .await;
            }
            if policy.legacy {
                send_legacy_oauth_event(
                    oauth_transport,
                    crate::event::AcpEvent::OauthCompleted { server_name },
                )
                .await;
            }
        }
        crate::event::oauth::HostOAuthEvent::Failed {
            flow_id,
            server_name,
            failure_class,
            legacy_error,
        } => {
            if policy.safe {
                send_safe_oauth_event(
                    oauth_transport,
                    crate::event::oauth::OAuthWireNotification::failed(
                        flow_id,
                        server_name.clone(),
                        failure_class,
                    ),
                )
                .await;
            }
            if policy.legacy {
                send_legacy_oauth_event(
                    oauth_transport,
                    crate::event::AcpEvent::OauthFailed {
                        server_name,
                        error: legacy_error,
                    },
                )
                .await;
            }
        }
        crate::event::oauth::HostOAuthEvent::Cancelled {
            flow_id,
            server_name,
        } => {
            if policy.safe {
                send_safe_oauth_event(
                    oauth_transport,
                    crate::event::oauth::OAuthWireNotification::terminal(
                        flow_id,
                        server_name.clone(),
                        crate::event::oauth::OAuthWireStatus::Cancelled,
                    ),
                )
                .await;
            }
            if policy.legacy {
                send_legacy_oauth_event(
                    oauth_transport,
                    crate::event::AcpEvent::OauthFailed {
                        server_name,
                        error: "OAuth authorization cancelled".to_string(),
                    },
                )
                .await;
            }
        }
        crate::event::oauth::HostOAuthEvent::Restored {
            flow_id,
            server_name,
        } => {
            if policy.safe {
                send_safe_oauth_event(
                    oauth_transport,
                    crate::event::oauth::OAuthWireNotification::terminal(
                        flow_id,
                        server_name.clone(),
                        crate::event::oauth::OAuthWireStatus::Restored,
                    ),
                )
                .await;
            }
            if policy.legacy {
                send_legacy_oauth_event(
                    oauth_transport,
                    crate::event::AcpEvent::OauthRestored { server_name },
                )
                .await;
            }
        }
    }
}
