//! Session-local model tool adapter. All recovery data crosses tools/call.
use async_trait::async_trait;
use peri_acp_types::context_archive::ArchiveError;
use peri_agent::{
    context_archive::ContextArchiveReader,
    tools::{BaseTool, ToolContext},
};
use peri_mcp_common::context_archive::{self, ContextArchiveMcpServer};
use rmcp::{
    model::{CallToolRequestParams, ProtocolVersion},
    serve_client_with_lifecycle, serve_server, ClientLifecycleMode,
};
use std::{sync::Arc, time::Duration};

pub struct ReadContextArchiveTool;
#[async_trait]
impl BaseTool for ReadContextArchiveTool {
    fn name(&self) -> &str {
        context_archive::TOOL_NAME
    }
    fn description(&self) -> &str {
        context_archive::DESCRIPTION
    }
    fn parameters(&self) -> serde_json::Value {
        context_archive::parameters()
    }
    fn is_direct(&self) -> bool {
        true
    }
    // Archive read results keep their original source references and never produce recursive catalogs.
    fn context_retention(&self) -> peri_agent::tools::ContextRetention {
        peri_agent::tools::ContextRetention::ArchiveRead
    }
    async fn invoke(
        &self,
        input: serde_json::Value,
        ctx: ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let session_id = ctx.session_id.ok_or(ArchiveError::Forbidden)?;
        let resources = ctx.session_resources.ok_or(ArchiveError::Unsupported)?;
        let params = CallToolRequestParams::new(context_archive::TOOL_NAME).with_arguments(
            input
                .as_object()
                .cloned()
                .ok_or_else(|| ArchiveError::InvalidRequest("expected object".into()))?,
        );
        let server = ContextArchiveMcpServer::new(Arc::new(ContextArchiveReader::new(
            resources, session_id,
        )));
        let (client_io, server_io) = tokio::io::duplex(16 * 1024);
        let mut server_task = tokio::spawn(async move {
            let service = serve_server(server, tokio::io::split(server_io)).await?;
            service
                .waiting()
                .await
                .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)
        });
        // On cancellation/drop the child must not detach with a live store capability.
        let abort = server_task.abort_handle();
        struct AbortOnDrop(tokio::task::AbortHandle);
        impl Drop for AbortOnDrop {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let _guard = AbortOnDrop(abort);
        let handshake = tokio::select! {
            biased;
            _=ctx.cancellation.cancelled()=> {
                server_task.abort();
                let _ = (&mut server_task).await;
                return Err(ArchiveError::Unavailable("cancelled".into()).into());
            },
            result=serve_client_with_lifecycle((),tokio::io::split(client_io),ClientLifecycleMode::Auto {preferred_versions:vec![ProtocolVersion::V_2026_07_28],legacy_version:None})=>result,
        };
        let mut service = match handshake {
            Ok(service) => service,
            Err(error) => {
                server_task.abort();
                let _ = (&mut server_task).await;
                return Err(Box::new(error));
            }
        };
        let outcome = tokio::select! {
            biased;
            _=ctx.cancellation.cancelled()=>Err(ArchiveError::Unavailable("cancelled".into()).into()),
            result=service.peer().call_tool(params)=>result.map_err(|e|Box::new(e) as Box<dyn std::error::Error+Send+Sync>),
        };
        service.close_with_timeout(Duration::from_secs(2)).await?;
        match tokio::time::timeout(Duration::from_secs(2), &mut server_task).await {
            Ok(Ok(Ok(_))) => {}
            Ok(Ok(Err(error))) => return Err(error),
            Ok(Err(error)) => return Err(Box::new(error)),
            Err(_) => {
                server_task.abort();
                let _ = server_task.await;
                return Err(ArchiveError::Unavailable(
                    "MCP archive shutdown did not complete".into(),
                )
                .into());
            }
        }
        let output = outcome?;
        let text = output
            .content
            .iter()
            .filter_map(|block| block.as_text().map(|text| text.text.as_str()))
            .collect::<Vec<_>>()
            .join("\n");
        if output.is_error == Some(true) {
            return Err(ArchiveError::Unavailable(text).into());
        }
        Ok(text)
    }
}

#[cfg(test)]
#[path = "context_archive_test.rs"]
mod tests;
