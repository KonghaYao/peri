//! Session 撤销、连接清理与幂等 close registration。

use super::DynamicMcpRegistry;
use async_trait::async_trait;
use peri_acp_types::{
    dynamic_mcp::{
        DynamicMcpOperationState, DynamicMcpShutdownReport, SessionMcpCapabilitySnapshot,
    },
    ports::{SessionCloseRegistration, SessionMcpProjectionLease},
};
use std::{
    collections::BTreeSet,
    sync::{Arc, Weak},
};

impl DynamicMcpRegistry {
    pub fn close_registration(
        &self,
        session_id: impl Into<String>,
    ) -> Arc<dyn SessionCloseRegistration> {
        Arc::new(RegistrySessionClose {
            registry: self.self_weak.clone(),
            session_id: session_id.into(),
            closed: std::sync::atomic::AtomicBool::new(false),
        })
    }

    pub(super) async fn close_session_impl(&self, session_id: &str) -> DynamicMcpShutdownReport {
        let (instances, active) = {
            let mut state = self.state.lock();
            state.closed_sessions.insert(session_id.to_string());
            state.notification_sinks.remove(session_id);
            if let Some(projection) = state
                .projections
                .remove(session_id)
                .and_then(|p| p.upgrade())
            {
                projection.close();
            }
            let entries = state
                .entries
                .values_mut()
                .filter(|entry| entry.instance.logical.session_id == session_id)
                .collect::<Vec<_>>();
            let instances = entries
                .iter()
                .map(|entry| entry.instance.clone())
                .collect::<Vec<_>>();
            let active = entries
                .iter()
                .filter_map(|entry| {
                    entry
                        .active
                        .clone()
                        .map(|connection| (entry.instance.clone(), connection))
                })
                .collect::<Vec<_>>();
            for entry in entries {
                entry.state = DynamicMcpOperationState::Draining;
                if let Some(active) = &entry.active {
                    active.gate.begin_draining();
                }
            }
            let previous = state.capabilities.remove(session_id).unwrap_or_default();
            state.capabilities.insert(
                session_id.to_string(),
                Arc::new(SessionMcpCapabilitySnapshot {
                    generation: previous.generation.saturating_add(1),
                    ..Default::default()
                }),
            );
            (instances, active)
        };
        let mut unfinished = 0;
        let mut retained = BTreeSet::new();
        for (instance, connection) in &active {
            let drain_incomplete =
                tokio::time::timeout(self.drain_timeout, connection.gate.drain())
                    .await
                    .is_err();
            let close_incomplete = connection.close().await.is_err();
            if drain_incomplete || close_incomplete {
                unfinished += 1;
                retained.insert(instance.logical.clone());
            } else {
                connection.gate.close();
            }
            self.task_spawner.stop_instance(instance).await;
        }
        let active_instances = active
            .iter()
            .map(|(instance, _)| instance)
            .collect::<BTreeSet<_>>();
        for instance in instances
            .iter()
            .filter(|instance| !active_instances.contains(instance))
        {
            self.task_spawner.stop_instance(instance).await;
        }
        let mut state = self.state.lock();
        state
            .entries
            .retain(|key, _| key.session_id != session_id || retained.contains(key));
        state.operations.retain(|_, operation| {
            operation.instance.logical.session_id != session_id
                || retained.contains(&operation.instance.logical)
        });
        if retained.is_empty() {
            state.catalogs.remove(session_id);
        }
        if unfinished == 0 {
            DynamicMcpShutdownReport::Complete
        } else {
            DynamicMcpShutdownReport::Incomplete {
                unfinished_instances: unfinished,
            }
        }
    }
}
struct RegistrySessionClose {
    registry: Weak<DynamicMcpRegistry>,
    session_id: String,
    closed: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl SessionCloseRegistration for RegistrySessionClose {
    async fn revoke_and_cleanup(&self) -> DynamicMcpShutdownReport {
        if self.closed.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return DynamicMcpShutdownReport::Complete;
        }
        match self.registry.upgrade() {
            Some(registry) => registry.close_session_impl(&self.session_id).await,
            None => DynamicMcpShutdownReport::Complete,
        }
    }
}
