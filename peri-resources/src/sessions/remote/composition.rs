//! Turso composition: canonical data and immutable execution evidence live in
//! the remote store. Only filesystem observations and live leases stay in process.
//! No local SQLite connection is opened in either access mode.

use std::sync::Arc;

use anyhow::Result;
use peri_acp_types::session_resources::AccessMode;

use crate::sessions::data::SessionDataPort;
use crate::sessions::local_port::LocalExecutionPort;
use crate::sessions::resources::SessionDataHome;
use crate::sessions::SessionResourcesImpl;

use super::credentials::SessionStoreCredential;
use super::endpoint::RemoteEndpoint;
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
    let (data, _initialization) =
        RemoteSessionData::open(endpoint, credential, StoreAccess::of(access)).await?;
    let data = Arc::new(data);
    let local_port: Arc<dyn LocalExecutionPort> = Arc::new(RemoteExecution::new(
        Arc::clone(&data),
        access == AccessMode::ReadOnly,
    ));
    let data_port: Arc<dyn SessionDataPort> = data;
    Ok(Arc::new(SessionResourcesImpl::from_ports(
        data_port,
        local_port,
        SessionDataHome::RemoteStore,
    )))
}
