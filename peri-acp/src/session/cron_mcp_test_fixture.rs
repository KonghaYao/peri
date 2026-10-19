use std::sync::Arc;

use peri_acp_types::ports::{McpPoolPort, McpPoolShutdownReport};
use rmcp::service::{Peer, RunningService};
use rmcp::{ClientLifecycleMode, RoleClient};

struct CronWirePool {
    peer: Peer<RoleClient>,
    service: parking_lot::Mutex<Option<RunningService<RoleClient, ()>>>,
    server: tokio::task::AbortHandle,
}

impl Drop for CronWirePool {
    fn drop(&mut self) {
        self.server.abort();
    }
}

pub(super) async fn connect(
    scheduler: Arc<parking_lot::Mutex<peri_mcp_cron::CronScheduler>>,
) -> Arc<dyn McpPoolPort> {
    let (client_io, server_io) = tokio::io::duplex(8192);
    let server = tokio::spawn(async move {
        rmcp::serve_server(
            peri_mcp_cron::CronMcpServer::new(scheduler),
            tokio::io::split(server_io),
        )
        .await
        .unwrap()
        .waiting()
        .await
        .unwrap();
    });
    let service = rmcp::serve_client_with_lifecycle(
        (),
        tokio::io::split(client_io),
        ClientLifecycleMode::Auto {
            preferred_versions: vec![rmcp::model::ProtocolVersion::V_2026_07_28],
            legacy_version: None,
        },
    )
    .await
    .unwrap();
    Arc::new(CronWirePool {
        peer: service.peer().clone(),
        service: parking_lot::Mutex::new(Some(service)),
        server: server.abort_handle(),
    })
}

#[async_trait::async_trait]
impl McpPoolPort for CronWirePool {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({})
    }
    async fn shutdown(&self) -> McpPoolShutdownReport {
        let service = self.service.lock().take();
        if let Some(mut service) = service {
            let _ = service
                .close_with_timeout(std::time::Duration::from_secs(1))
                .await;
        }
        self.server.abort();
        McpPoolShutdownReport::Complete {
            settled_services: 1,
            failed_services: 0,
        }
    }
    async fn read_instance_resource(&self, instance: &str, uri: &str) -> Result<String, String> {
        assert_eq!(instance, "cron");
        let result = self
            .peer
            .read_resource(rmcp::model::ReadResourceRequestParams::new(uri))
            .await
            .map_err(|error| error.to_string())?;
        match &result.contents[0] {
            rmcp::model::ResourceContents::TextResourceContents { text, .. } => Ok(text.clone()),
            _ => Err("not text".into()),
        }
    }
    async fn call_instance_tool(
        &self,
        instance: &str,
        tool: &str,
        arguments: serde_json::Value,
        _meta: Option<serde_json::Value>,
    ) -> Result<String, String> {
        assert_eq!(instance, "cron");
        let result = self
            .peer
            .call_tool(
                rmcp::model::CallToolRequestParams::new(tool.to_owned())
                    .with_arguments(arguments.as_object().unwrap().clone()),
            )
            .await
            .map_err(|error| error.to_string())?;
        let text = result
            .content
            .iter()
            .filter_map(|content| content.as_text())
            .map(|text| text.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if result.is_error == Some(true) {
            Err(text)
        } else {
            Ok(text)
        }
    }
}
