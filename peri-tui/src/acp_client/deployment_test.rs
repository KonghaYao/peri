use super::*;
use peri_acp::transport::{
    AcpTransport,
    mpsc::mpsc_transport_pair,
    types::{AcpError, IncomingMessage},
};
use serde_json::json;
use std::{sync::Arc, time::Duration};

#[tokio::test]
async fn test_deployment_error_exit_closes_pump_and_joins_host() {
    for method in ["session/new", "session/prompt"] {
        let (transport, server) = mpsc_transport_pair();
        let (client, tx, mut notifications) = AcpTuiClient::new(transport);
        client.spawn_pump(tx);
        let survivor = client.clone();
        let server = Arc::new(server);
        let host = tokio::spawn(async move {
            let Some(IncomingMessage::Request {
                id,
                method: received,
                ..
            }) = server.recv().await
            else {
                panic!("宿主必须收到实际请求");
            };
            assert_eq!(received, method);
            server
                .send_response(
                    id,
                    Err(AcpError {
                        code: -32603,
                        message: "controlled request failure".into(),
                        data: None,
                    }),
                )
                .await
                .unwrap();
            assert!(
                server.recv().await.is_none(),
                "错误返回后显式 close 必须让宿主收到 EOF"
            );
            AcpHostShutdownReport::Complete
        });
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            finish_operation(
                async {
                    client.send_raw_request(method, json!({})).await?;
                    Ok(())
                },
                &client,
                async { host.await.expect("必须 join 真实宿主任务") },
            ),
        )
        .await
        .unwrap();
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("controlled request failure")
        );
        assert!(
            survivor
                .send_raw_request("after-close", json!({}))
                .await
                .is_err()
        );
        while tokio::time::timeout(Duration::from_secs(5), notifications.recv())
            .await
            .unwrap()
            .is_some()
        {}
    }
}

#[tokio::test]
async fn test_deployment_incomplete_exit_is_visible_and_preserves_business_error() {
    for business_failed in [false, true] {
        for report in [
            AcpHostShutdownReport::Incomplete,
            AcpHostShutdownReport::TaskFailed { cancelled: true },
        ] {
            let (transport, server) = mpsc_transport_pair();
            let (client, tx, _notifications) = AcpTuiClient::new(transport);
            client.spawn_pump(tx);
            let host = tokio::spawn(async move {
                let Some(IncomingMessage::Request { id, .. }) = server.recv().await else {
                    panic!("必须收到实际请求")
                };
                let result = if business_failed {
                    Err(AcpError::new(-32603, "original business failure"))
                } else {
                    Ok(json!({}))
                };
                server.send_response(id, result).await.unwrap();
                assert!(server.recv().await.is_none());
                report
            });
            let error = finish_operation(
                async {
                    client.send_raw_request("session/prompt", json!({})).await?;
                    Ok(())
                },
                &client,
                async { host.await.unwrap() },
            )
            .await
            .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("ACP deployment shutdown did not complete")
            );
            assert_eq!(
                format!("{error:#}").contains("original business failure"),
                business_failed
            );
        }
    }
}
