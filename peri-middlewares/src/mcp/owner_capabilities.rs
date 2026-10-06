use peri_acp_types::plugin::McpServerConfig;
use peri_acp_types::session_resources::work::WorkSnapshot;
use serde::Deserialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OwnerRecoveryGuarantee {
    DiscoverableWhileOwnerAlive,
    BestEffortAtLeastOnce,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct OwnerCapabilities {
    pub version: u32,
    pub owner_identity: String,
    pub scope_id: String,
    pub scope_epoch: u64,
    pub invocation_discovery: bool,
    pub retained_tasks: bool,
    pub scope_close_barrier: bool,
    pub resource_settlement: bool,
    pub invocation_idempotency: bool,
}

impl OwnerCapabilities {
    pub(crate) fn validate_scope(&self, session_id: &str) -> Result<(), String> {
        if self.version != 1 || self.owner_identity.is_empty() || self.scope_id != session_id {
            return Err(
                "Unroutable: owner capability evidence conflicts with trusted scope".into(),
            );
        }
        Ok(())
    }

    pub(crate) fn recovery_guarantee(&self) -> OwnerRecoveryGuarantee {
        if self.invocation_discovery && self.retained_tasks {
            OwnerRecoveryGuarantee::DiscoverableWhileOwnerAlive
        } else {
            OwnerRecoveryGuarantee::BestEffortAtLeastOnce
        }
    }

    pub(crate) fn require_close_barrier(&self) -> Result<(), String> {
        if !self.scope_close_barrier || !self.resource_settlement {
            return Err(
                "Incomplete: owner cannot prove scope close and resource settlement".into(),
            );
        }
        Ok(())
    }
}

pub(crate) fn trusted_owner_configuration(
    snapshot: &WorkSnapshot,
    recipient_lifecycle: u64,
    server: &str,
) -> Result<(McpServerConfig, String), String> {
    let binding = snapshot
        .state
        .resource_owners
        .get(&recipient_lifecycle)
        .ok_or("Incomplete: durable trusted owner declarations unavailable")?;
    if binding.recipient_lifecycle != recipient_lifecycle || binding.authorization_ref.is_empty() {
        return Err("Incomplete: owner declaration lifecycle or authorization unavailable".into());
    }
    let declarations: serde_json::Value = serde_json::from_str(&binding.connections_json)
        .map_err(|_| "Incomplete: durable owner declarations invalid")?;
    let configuration = declarations
        .as_object()
        .and_then(|servers| servers.get(server))
        .ok_or("Unroutable: task owner is not in the lifecycle's trusted declarations")?;
    let config = serde_json::from_value(configuration.clone())
        .map_err(|_| "Incomplete: durable trusted owner configuration invalid")?;
    Ok((config, binding.authorization_ref.clone()))
}

#[cfg(test)]
#[path = "owner_capabilities_test.rs"]
mod tests;
