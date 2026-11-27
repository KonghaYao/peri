//! One session-store opening rule for the runtime boundary.
//!
//! Both the ACP host assembly and the read-only `sessions.subagents` reader use
//! this module, so a deployment's store is interpreted in exactly one place
//! (`design §1.2`, `design §4.1`).

use std::path::PathBuf;

use peri_acp_types::session_store::SessionStoreDeployment;
use peri_resources::{RemoteWorkspaceEnvironment, Resources};
use uuid::Uuid;

use super::{ChildConfig, ChildError};

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
