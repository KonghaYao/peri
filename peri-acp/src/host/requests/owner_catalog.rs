//! Host-only identity for the Workspace task owner used by a session execution.

use sha2::{Digest, Sha256};
use std::sync::Arc;

use peri_acp_types::workspace::{ExecutionWorkspaceOwnerRecord, WorkspaceExecutionDescriptor};
use serde_json::Value;

use crate::transport::types::AcpError;

/// A descriptor contains identity evidence, never a scope bearer or key path.
pub(super) struct TrustedWorkspaceIdentity {
    pub endpoint: String,
    pub key_identity: String,
    pub agent_generation_id: String,
}

pub(super) fn trusted_workspace_identity() -> Result<Option<TrustedWorkspaceIdentity>, String> {
    let endpoint = std::env::var("PERI_TRUSTED_WORKSPACE_URL").ok();
    let secret_file = std::env::var("PERI_TRUSTED_WORKSPACE_SCOPE_SECRET_FILE").ok();
    let agent_generation_id = std::env::var("PERI_AGENT_GENERATION_ID").ok();
    let (Some(endpoint), Some(secret_file), Some(agent_generation_id)) = (
        endpoint.as_ref(),
        secret_file.as_ref(),
        agent_generation_id.as_ref(),
    ) else {
        if endpoint.is_some() || secret_file.is_some() {
            return Err("trusted Workspace owner identity is incomplete".into());
        }
        return Ok(None);
    };
    if agent_generation_id.len() != 32
        || !agent_generation_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("trusted Agent generation ID is invalid".into());
    }
    let url = url::Url::parse(endpoint).map_err(|_| "trusted Workspace endpoint is invalid")?;
    if url.scheme() != "http"
        || url
            .host_str()
            .is_none_or(|host| host != "127.0.0.1" && host != "localhost" && host != "[::1]")
    {
        return Err("trusted Workspace endpoint must be loopback HTTP".into());
    }
    let path = std::path::Path::new(&secret_file);
    peri_mcp_workspace::TaskScopeAuthority::from_secret_file(path)
        .map_err(|_| "trusted Workspace scope authority is invalid")?;
    let key =
        std::fs::read(path).map_err(|_| "trusted Workspace scope authority is unavailable")?;
    let digest = Sha256::digest(&key);
    let key_identity = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(Some(TrustedWorkspaceIdentity {
        endpoint: url.to_string(),
        key_identity,
        agent_generation_id: agent_generation_id.clone(),
    }))
}

pub(super) async fn execution_descriptor(
    pool: Option<&Arc<peri_middlewares::mcp::McpClientPool>>,
    params: &Value,
) -> Result<WorkspaceExecutionDescriptor, AcpError> {
    let trusted = trusted_workspace_identity()
        .map_err(|error| AcpError::new(-32010, format!("Session admission incomplete: {error}")))?;
    let declared_other_owner = params
        .get("mcpServers")
        .and_then(Value::as_array)
        .is_some_and(|servers| {
            servers.iter().any(|server| {
                server.get("name").and_then(Value::as_str) != Some("workspace")
                    || server.get("type").and_then(Value::as_str) != Some("http")
            })
        });
    let unsupported = if let (Some(pool), Some(identity)) = (pool, trusted.as_ref()) {
        pool.has_unsupported_async_task_owner(Some(&identity.endpoint))
            .await
            .map_err(|error| {
                AcpError::new(-32010, format!("Session admission incomplete: {error}"))
            })?
            || declared_other_owner
    } else {
        true
    };
    let identity = trusted.unwrap_or_else(|| TrustedWorkspaceIdentity {
        endpoint: String::new(),
        key_identity: String::new(),
        agent_generation_id: std::env::var("PERI_AGENT_GENERATION_ID").unwrap_or_default(),
    });
    Ok(WorkspaceExecutionDescriptor {
        endpoint: identity.endpoint,
        key_identity: identity.key_identity,
        agent_generation_id: identity.agent_generation_id,
        unsupported_async_owners: unsupported,
    })
}

pub(super) fn verify_recoverable_owner(
    recorded: &ExecutionWorkspaceOwnerRecord,
    trusted: &TrustedWorkspaceIdentity,
) -> Result<(), String> {
    if recorded.descriptor.unsupported_async_owners
        || recorded.descriptor.endpoint != trusted.endpoint
        || recorded.descriptor.key_identity != trusted.key_identity
        || recorded.descriptor.agent_generation_id.is_empty()
    {
        return Err("execution-time task owner cannot be verified".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unloaded_close_requires_the_original_endpoint_key_and_supported_catalog() {
        let trusted = TrustedWorkspaceIdentity {
            endpoint: "http://127.0.0.1:8080/mcp".into(),
            key_identity: "key-a".into(),
            agent_generation_id: "b".repeat(32),
        };
        let mut record = ExecutionWorkspaceOwnerRecord {
            current_epoch: 2,
            descriptor_epoch: 1,
            descriptor: WorkspaceExecutionDescriptor {
                endpoint: trusted.endpoint.clone(),
                key_identity: trusted.key_identity.clone(),
                agent_generation_id: "a".repeat(32),
                unsupported_async_owners: false,
            },
        };
        assert!(verify_recoverable_owner(&record, &trusted).is_ok());
        record.descriptor.endpoint = "http://127.0.0.1:9090/mcp".into();
        assert!(verify_recoverable_owner(&record, &trusted).is_err());
        record.descriptor.endpoint = trusted.endpoint.clone();
        record.descriptor.key_identity = "key-b".into();
        assert!(verify_recoverable_owner(&record, &trusted).is_err());
        record.descriptor.key_identity = trusted.key_identity.clone();
        record.descriptor.unsupported_async_owners = true;
        assert!(verify_recoverable_owner(&record, &trusted).is_err());
    }
}
