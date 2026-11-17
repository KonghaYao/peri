//! MCP 操作后的主动查询：沿用服务投影解码与唯一提交门。

use std::sync::atomic::Ordering;
use std::time::Duration;

use peri_acp::transport::types::AcpError;
use serde_json::json;

use crate::acp_client::AcpTuiClient;

use super::commit::ProjectionIdentity;
use super::commit::ProjectionUpdate;
use super::commit::SnapshotPatch;

pub(crate) async fn refresh_mcp_projection(
    client: &AcpTuiClient,
    identity: &(String, u64),
    bridge_reset: u64,
) -> Result<bool, AcpError> {
    if client.stable_session_identity().as_ref() != Some(identity)
        || ACTIVE_SESSION_ID.state().read().as_str() != identity.0
        || BRIDGE_RESET_COUNTER.get() != bridge_reset
    {
        return Ok(false);
    }
    let revision = super::MCP_REVISION.fetch_add(1, Ordering::AcqRel) + 1;
    let expected = ProjectionIdentity {
        active_cwd: ACTIVE_EXECUTION_CWD.state().read().clone(),
        session_id: identity.0.clone(),
        session_generation: Some(identity.clone()),
        scope: THREAD_BROWSER_SCOPE.get(),
        archived: THREAD_BROWSER_ARCHIVED.get(),
        page_count: THREAD_LIST_PAGE_COUNT.get(),
        bridge_reset,
        mcp_revision: revision,
    };
    let value = peri_time::timeout(
        Duration::from_secs(10),
        client.send_session_request(identity, "mcp/list", json!({"sessionId": identity.0})),
    )
    .await
    .map_err(|_| AcpError::new(-32603, "MCP projection refresh timed out"))??;
    let (mcp, servers) = super::session_services::decode_mcp(value, client.supports_mcp_control())?;
    Ok(super::commit::commit_projection(
        Some(client),
        &expected,
        &[SnapshotPatch::Mcp(mcp)],
        ProjectionUpdate::Mcp(servers),
    ))
}

use crate::kit::atoms::ACTIVE_EXECUTION_CWD;

use crate::kit::atoms::ACTIVE_SESSION_ID;

use crate::kit::atoms::BRIDGE_RESET_COUNTER;

use crate::kit::atoms::THREAD_BROWSER_ARCHIVED;

use crate::kit::atoms::THREAD_BROWSER_SCOPE;

use crate::kit::atoms::THREAD_LIST_PAGE_COUNT;
