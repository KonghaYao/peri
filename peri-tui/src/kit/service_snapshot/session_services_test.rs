use super::*;
use peri_acp::transport::{
    AcpTransport,
    mpsc::mpsc_transport_pair,
    types::{AcpError, IncomingMessage},
};

#[tokio::test]
async fn successful_projection_then_partial_failure_preserves_last_good_and_reports_errors() {
    let (transport, server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    let server_task = tokio::spawn(async move {
        for round in 0..2 {
            for _ in 0..3 {
                let Some(IncomingMessage::Request { id, method, params }) = server.recv().await
                else {
                    panic!("expected projection request");
                };
                assert_eq!(params["sessionId"], "session-a");
                let response = match (round, method.as_str()) {
                    (0, "plugin/list") => Ok(
                        json!({"hooks":[{"event":"PreToolUse","plugin_name":"first","command":"echo","matcher":null}],"plugins":[]}),
                    ),
                    (0, "mcp/list") => Ok(
                        json!({"servers":[{"name":"first","transport":"stdio","connectionStatus":"connected","oauthStatus":"none","toolsCount":1}]}),
                    ),
                    (0, "cron/list") => Ok(
                        json!({"jobs":[{"id":"job-a","expression":"*/5 * * * *","prompt":"scheduled","enabled":true,"next_fire":null}]}),
                    ),
                    (1, "plugin/list") => Ok(json!({"hooks":"malformed"})),
                    (1, "mcp/list") => Ok(json!({"servers":[]})),
                    (1, "cron/list") => Err(AcpError::new(-32601, "unsupported cron")),
                    other => panic!("unexpected projection: {other:?}"),
                };
                server.send_response(id, response).await.unwrap();
            }
        }
    });
    let mut services = SessionServices::default();
    assert!(query(&client, "session-a", &mut services).await.is_none());
    assert_eq!(services.hooks[0].event, "pretooluse");
    assert_eq!(services.mcp.connected, 1);
    assert_eq!(services.cron_jobs[0].id, "job-a");
    let error = query(&client, "session-a", &mut services).await.unwrap();
    assert!(!error.is_empty());
    assert_eq!(services.hooks[0].plugin_name, "first");
    assert_eq!(services.cron_jobs[0].id, "job-a");
    assert!(services.mcp_servers.is_empty());
    assert_eq!(services.mcp.init_phase, McpInitPhase::Ready);
    server_task.await.unwrap();
}

#[tokio::test]
async fn disconnect_keeps_same_owner_data_but_marks_mcp_failed() {
    let (transport, _server) = mpsc_transport_pair();
    let (client, _, _) = AcpTuiClient::new(transport);
    client.close();
    let mut services = SessionServices::default();
    services.cron_jobs.push(CronJobSummary {
        id: "retained".into(),
        ..Default::default()
    });
    assert!(query(&client, "session-a", &mut services).await.is_some());
    assert_eq!(services.cron_jobs[0].id, "retained");
    assert_eq!(services.mcp.init_phase, McpInitPhase::Failed);
}

#[test]
fn last_good_projection_is_scoped_to_session_generation_not_only_session_id() {
    let mut refresh = super::super::SlowSnapshotRefresh::default();
    refresh.reset_services(&Some(("session-a".into(), 1)));
    refresh.services.cron_jobs.push(CronJobSummary {
        id: "last-good".into(),
        ..Default::default()
    });
    refresh.reset_services(&Some(("session-a".into(), 1)));
    assert_eq!(refresh.services.cron_jobs.len(), 1);
    refresh.reset_services(&Some(("session-a".into(), 2)));
    assert!(refresh.services.cron_jobs.is_empty());
    refresh.services.cron_jobs.push(CronJobSummary::default());
    refresh.reset_services(&Some(("session-b".into(), 2)));
    assert!(refresh.services.cron_jobs.is_empty());
    refresh.services.cron_jobs.push(CronJobSummary::default());
    refresh.reset_services(&None);
    assert!(refresh.services.cron_jobs.is_empty());
}
