//! One session-store opening rule for the runtime boundary.
//!
//! Both the ACP host assembly and the read-only `sessions.subagents` reader use
//! this module, so a deployment's store is interpreted in exactly one place
//! (`design §1.2`, `design §4.1`).

use std::path::PathBuf;
use std::sync::Arc;

use peri_acp_types::session_resources::SessionResources;
use peri_acp_types::session_store::SessionStoreDeployment;
use peri_resources::{RemoteWorkspaceEnvironment, Resources, SessionStoreShutdownOwner};
use uuid::Uuid;

use super::{ChildConfig, ChildError};

/// Reject a half-configured remote identity before selecting any handle.
pub(super) fn validate_remote_identity(
    machine_id: Option<Uuid>,
    workspace_root: Option<&std::path::Path>,
) -> Result<(), ChildError> {
    match (machine_id, workspace_root) {
        (None, None) | (Some(_), Some(_)) => Ok(()),
        _ => Err(ChildError::Local(
            "remote session store requires both machine_id and workspace_root; \
             refusing to open the local store"
                .to_owned(),
        )),
    }
}

/// Open a deployment's session store.
///
/// - no remote identity ⇒ the existing local `Resources::open_deployment`
///   (S4/S5 default, unchanged);
/// - `machine_id` **and** `workspace_root` ⇒ the remote PostgreSQL store with a
///   virtual workspace identity (`design §4.1`);
/// - exactly one of the two ⇒ an explicit error. Falling back to the local
///   SQLite file here would write Swarm facts to a second database.
pub(crate) async fn open_resources(
    deployment: &SessionStoreDeployment,
    machine_id: Option<Uuid>,
    workspace_root: Option<PathBuf>,
) -> Result<Resources, ChildError> {
    validate_remote_identity(machine_id, workspace_root.as_deref())?;
    match (machine_id, workspace_root.as_ref()) {
        (None, None) => Resources::open_deployment(deployment).await,
        (Some(machine_id), Some(root)) => {
            let environment = RemoteWorkspaceEnvironment::virtual_workspace(
                &machine_id.to_string(),
                root.clone(),
            )
            .map_err(|error| {
                ChildError::Local(format!("invalid remote workspace environment: {error:#}"))
            })?;
            Resources::open_deployment_in_remote_environment(deployment, environment).await
        }
        // `validate_remote_identity` already rejected this shape; keep the
        // same explicit failure instead of panicking if the check ever drifts.
        _ => {
            return Err(ChildError::Local(
                "remote session store requires both machine_id and workspace_root; \
                 refusing to open the local store"
                    .to_owned(),
            ));
        }
    }
    .map_err(|error| ChildError::Local(format!("{error:#}")))
}

/// Open the child's session store for the configured deployment.
pub(super) async fn open_session_resources(config: &ChildConfig) -> Result<Resources, ChildError> {
    open_resources(
        &config.session_store,
        config.machine_id,
        config.workspace_root.clone(),
    )
    .await
}

/// Resolved session store for one child: business handle plus, only when this
/// child opened the store itself, its close right.
pub(super) struct ResolvedSessionResources {
    pub(super) resources: Arc<dyn SessionResources>,
    /// `None` when the deployment injected a shared handle: a child never owns
    /// the process-wide pool's close right.
    pub(super) shutdown: Option<SessionStoreShutdownOwner>,
}

impl std::fmt::Debug for ResolvedSessionResources {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResolvedSessionResources")
            .field("owns_shutdown", &self.shutdown.is_some())
            .finish_non_exhaustive()
    }
}

/// Resolve the child's session store for the configured deployment.
///
/// 1. Validate the remote identity first: exactly one of `machine_id` /
///    `workspace_root` is an explicit error even when a shared handle exists,
///    because a half-configured identity must never be papered over by an
///    injected facade.
/// 2. Injected deployment handle ⇒ clone it and open nothing. The close right
///    stays with the deployment.
/// 3. No injected handle ⇒ the per-child open through the single opening rule.
pub(super) async fn resolve_session_resources(
    config: &ChildConfig,
) -> Result<ResolvedSessionResources, ChildError> {
    validate_remote_identity(config.machine_id, config.workspace_root.as_deref())?;
    if let Some(shared) = config.session_resources.as_ref() {
        return Ok(ResolvedSessionResources {
            resources: Arc::clone(shared),
            shutdown: None,
        });
    }
    let resources = open_session_resources(config).await?;
    let (resources, shutdown) = resources.into_parts();
    Ok(ResolvedSessionResources {
        resources,
        shutdown: Some(shutdown),
    })
}
