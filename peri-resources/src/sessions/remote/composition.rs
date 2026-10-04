//! Turso composition: canonical data and immutable execution evidence live in
//! the remote store. Workspace observations and live leases stay in process.
//! No local SQLite connection is opened in either access mode.

use std::sync::Arc;

use anyhow::{Context, Result};
use peri_acp_types::session_resources::AccessMode;

use crate::sessions::data::SessionDataPort;
use crate::sessions::local_port::LocalExecutionPort;
use crate::sessions::resources::SessionDataHome;
use crate::sessions::SessionResourcesImpl;

use super::credentials::SessionStoreCredential;
use super::endpoint::RemoteEndpoint;
use super::environment::RemoteWorkspaceEnvironment;
use super::execution::RemoteExecution;
use super::mutation::StoreAccess;
use super::session_data::RemoteSessionData;

/// 打开远程会话存储并装配门面。
///
/// 凭证是**值**而不是来源：解析发生在 D 边界（读取进程环境的唯一位置），组合层只消费
/// 已解析的值，因此云实验可以把进程内解析出的凭证直接注入，不必把它写进进程环境。
///
pub(crate) async fn open_remote(
    endpoint: &RemoteEndpoint,
    credential: &SessionStoreCredential,
    access: AccessMode,
) -> Result<Arc<SessionResourcesImpl>> {
    open_remote_in_environment(
        endpoint,
        credential,
        access,
        RemoteWorkspaceEnvironment::Native,
    )
    .await
}

pub(crate) async fn open_remote_in_environment(
    endpoint: &RemoteEndpoint,
    credential: &SessionStoreCredential,
    access: AccessMode,
    environment: RemoteWorkspaceEnvironment,
) -> Result<Arc<SessionResourcesImpl>> {
    let environment = environment.validated()?;
    let machine_id = match &environment {
        RemoteWorkspaceEnvironment::Native => None,
        _ => Some(environment.machine_id()?),
    };
    open_remote_with(endpoint, credential, access, machine_id, |data, access| {
        Ok(Arc::new(RemoteExecution::new_in_environment(
            data,
            access == AccessMode::ReadOnly,
            environment,
        )))
    })
    .await
}

/// Remote composition chooses the execution port after opening remote data.
/// The factory receives the same data handle held by the facade, so it can
/// check durable workspace ownership without opening a second store. Deployments
/// select the supported observation mode through `RemoteWorkspaceEnvironment`.
pub(super) async fn open_remote_with<F>(
    endpoint: &RemoteEndpoint,
    credential: &SessionStoreCredential,
    access: AccessMode,
    machine_id: Option<String>,
    execution_factory: F,
) -> Result<Arc<SessionResourcesImpl>>
where
    F: FnOnce(Arc<RemoteSessionData>, AccessMode) -> Result<Arc<dyn LocalExecutionPort>>,
{
    let (data, _initialization) = RemoteSessionData::open_with_machine(
        endpoint,
        credential,
        StoreAccess::of(access),
        machine_id,
    )
    .await?;
    let data = Arc::new(data);
    match assemble_remote(Arc::clone(&data), access, execution_factory) {
        Ok(facade) => Ok(facade),
        Err(error) => {
            // No facade or shutdown owner has been handed out yet. Explicitly
            // close the opened data connection before returning the factory
            // failure; a dropped Arc is not a deployment shutdown contract.
            SessionDataPort::close(data.as_ref())
                .await
                .context("failed to close remote data after execution setup failed")?;
            Err(error)
        }
    }
}

fn assemble_remote<F>(
    data: Arc<RemoteSessionData>,
    access: AccessMode,
    execution_factory: F,
) -> Result<Arc<SessionResourcesImpl>>
where
    F: FnOnce(Arc<RemoteSessionData>, AccessMode) -> Result<Arc<dyn LocalExecutionPort>>,
{
    let local_port = execution_factory(Arc::clone(&data), access)?;
    let data_port: Arc<dyn SessionDataPort> = data;
    Ok(Arc::new(SessionResourcesImpl::from_ports(
        data_port,
        local_port,
        SessionDataHome::RemoteStore,
    )))
}

#[cfg(test)]
#[path = "composition_test.rs"]
mod tests;
