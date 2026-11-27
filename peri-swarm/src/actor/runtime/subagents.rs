//! Read-only access to the Peri session store for `sessions.subagents`.
//!
//! A live actor reuses the `SessionResources` handle its ACP host already
//! holds. A cold actor (or one whose root was never attached) lazily opens the
//! configured store read-only, so listing subagents never takes write access
//! and never creates a missing local database.
//!
//! This module lives under `actor/runtime/` because it is part of the single
//! runtime boundary that may import `peri-acp-types` / `peri-resources`
//! (`design §1.2`).

use std::path::PathBuf;
use std::sync::Arc;

use peri_acp_types::session_resources::{AccessMode, SessionResources};
use peri_acp_types::session_store::SessionStoreDeployment;
use peri_acp_types::thread::ThreadMeta;
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

use super::session_open::open_resources;
use crate::domain::{SubagentRecord, SwarmError};

/// Lazily opened read-only session-store handle for subagent reads.
pub(crate) struct SubagentReader {
    session_store: SessionStoreDeployment,
    machine_id: Option<Uuid>,
    workspace_root: Option<PathBuf>,
    resources: AsyncMutex<Option<Arc<dyn SessionResources>>>,
}

impl SubagentReader {
    pub(crate) fn new(
        session_store: SessionStoreDeployment,
        machine_id: Option<Uuid>,
        workspace_root: Option<PathBuf>,
    ) -> Self {
        Self {
            session_store,
            machine_id,
            workspace_root,
            resources: AsyncMutex::new(None),
        }
    }

    /// Whole root tree as neutral records, root excluded.
    ///
    /// A failed open or read is an upstream failure the caller maps to
    /// `502 upstream_unavailable`; an empty tree is a successful empty list.
    pub(crate) async fn records(
        &self,
        root_thread_id: &str,
    ) -> Result<Vec<SubagentRecord>, SwarmError> {
        let mut cached = self.resources.lock().await;
        if let Some(resources) = cached.as_ref() {
            return records_from(resources.as_ref(), root_thread_id).await;
        }
        let resources = self.open_read_only().await?;
        let records = records_from(resources.as_ref(), root_thread_id).await?;
        *cached = Some(resources);
        Ok(records)
    }

    async fn open_read_only(&self) -> Result<Arc<dyn SessionResources>, SwarmError> {
        let deployment = self.session_store.clone().with_access(AccessMode::ReadOnly);
        open_resources(&deployment, self.machine_id, self.workspace_root.clone())
            .await
            .map(|resources| resources.into_session_resources())
            .map_err(|error| {
                tracing::warn!(error = %error, "subagent reader could not open the session store");
                SwarmError::upstream_unavailable("subagent_sessions")
            })
    }
}

/// Project `ThreadMeta` rows into neutral records, excluding the root thread.
pub(crate) async fn records_from(
    resources: &dyn SessionResources,
    root_thread_id: &str,
) -> Result<Vec<SubagentRecord>, SwarmError> {
    let tree = resources
        .list_session_tree(&root_thread_id.to_owned())
        .await
        .map_err(|error| {
            tracing::warn!(error = %error, "subagent session tree read failed");
            SwarmError::upstream_unavailable("subagent_sessions")
        })?;
    Ok(tree
        .into_iter()
        .filter(|meta| meta.id != root_thread_id)
        .map(record_from_meta)
        .collect())
}

fn record_from_meta(meta: ThreadMeta) -> SubagentRecord {
    let active = meta.agent_status.is_active();
    SubagentRecord {
        id: meta.id,
        parent_thread_id: meta.parent_thread_id,
        name: meta.title,
        opened_at: meta.created_at.timestamp(),
        closed_at: if active {
            None
        } else {
            Some(meta.updated_at.timestamp())
        },
        active,
    }
}
