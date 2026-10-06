use super::{McpError, ShellTasks};
use rmcp::model::RequestMetaObject;
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct OwnerInvocation {
    metadata: serde_json::Value,
    owner_task_id: Option<String>,
    response_prepared: bool,
}

impl ShellTasks {
    pub(crate) fn accept_invocation(
        &self,
        scope: &str,
        metadata: &RequestMetaObject,
        arguments: &serde_json::Value,
    ) -> Result<Option<String>, McpError> {
        let Some(value) = metadata.0 .0.get("peri.invocation") else {
            return Ok(None);
        };
        let invocation_id = value
            .get("invocationId")
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| McpError::invalid_params("immutable invocation ID missing", None))?;
        let serialized = value
            .get("argumentsJson")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                McpError::invalid_params("immutable invocation arguments missing", None)
            })?;
        let parsed: serde_json::Value = serde_json::from_str(serialized).map_err(|_| {
            McpError::invalid_params("immutable invocation arguments invalid", None)
        })?;
        let mut state = self.state.lock();
        let epoch = state.epochs.get(scope).copied().unwrap_or_default();
        if value.get("version").and_then(serde_json::Value::as_u64) != Some(1)
            || value.get("scopeId").and_then(serde_json::Value::as_str) != Some(scope)
            || value
                .get("initiatorSessionId")
                .and_then(serde_json::Value::as_str)
                != Some(scope)
            || value
                .get("ownerIdentity")
                .and_then(serde_json::Value::as_str)
                != Some(self.owner_identity.as_ref())
            || value.get("scopeEpoch").and_then(serde_json::Value::as_u64) != Some(epoch)
            || value
                .get("recipientLifecycle")
                .and_then(serde_json::Value::as_u64)
                .is_none_or(|life| life == 0)
            || value
                .get("authorizationRef")
                .and_then(serde_json::Value::as_str)
                .is_none_or(str::is_empty)
            || value
                .get("argumentsDigest")
                .and_then(serde_json::Value::as_str)
                != Some(format!("{:x}", Sha256::digest(serialized.as_bytes())).as_str())
            || parsed != *arguments
            || state.closing.contains(scope)
        {
            return Err(McpError::invalid_params(
                "immutable invocation identity or epoch conflicts",
                None,
            ));
        }
        let key = (scope.to_owned(), invocation_id.to_owned());
        if state.invocations.contains_key(&key) {
            return Err(McpError::invalid_params(
                "invocation already accepted; discover original outcome, do not replay",
                None,
            ));
        }
        state.invocations.insert(
            key,
            OwnerInvocation {
                metadata: value.clone(),
                owner_task_id: None,
                response_prepared: false,
            },
        );
        Ok(Some(invocation_id.into()))
    }

    pub(crate) fn bind_invocation_task(&self, scope: &str, invocation_id: &str, task_id: &str) {
        if let Some(record) = self
            .state
            .lock()
            .invocations
            .get_mut(&(scope.into(), invocation_id.into()))
        {
            record.owner_task_id = Some(task_id.into());
        }
    }

    pub(crate) fn observe_invocation_response(&self, scope: &str, invocation_id: &str) {
        if let Some(record) = self
            .state
            .lock()
            .invocations
            .get_mut(&(scope.into(), invocation_id.into()))
        {
            record.response_prepared = true;
        }
    }

    pub(crate) fn discover_invocations(&self, scope: &str) -> serde_json::Value {
        let state = self.state.lock();
        serde_json::json!({"ownerIdentity": self.owner_identity, "scopeId": scope,
            "invocations": state.invocations.iter().filter(|((candidate, _), _)| candidate == scope)
                .map(|(_, record)| record.clone()).collect::<Vec<_>>()})
    }
}

#[cfg(test)]
#[path = "shell_tasks_invocations_test.rs"]
mod tests;
