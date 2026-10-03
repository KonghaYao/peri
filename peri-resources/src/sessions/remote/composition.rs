//! Turso composition: canonical data and immutable execution evidence live in
//! the remote store. Only filesystem observations and live leases stay in process.
//! No local SQLite connection is opened in either access mode.

#[cfg(target_os = "emscripten")]
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use peri_acp_types::session_resources::AccessMode;

use crate::sessions::data::SessionDataPort;
use crate::sessions::local_port::LocalExecutionPort;
use crate::sessions::resources::SessionDataHome;
use crate::sessions::SessionResourcesImpl;

use super::credentials::SessionStoreCredential;
use super::endpoint::RemoteEndpoint;
#[cfg(not(target_os = "emscripten"))]
use super::execution::RemoteExecution;
use super::mutation::StoreAccess;
use super::session_data::RemoteSessionData;
#[cfg(target_os = "emscripten")]
use crate::sessions::wasm_execution::WasmExecution;

/// 打开远程会话存储并装配门面。
///
/// 凭证是**值**而不是来源：解析发生在 D 边界（读取进程环境的唯一位置），组合层只消费
/// 已解析的值，因此云实验可以把进程内解析出的凭证直接注入，不必把它写进进程环境。
///
pub(crate) async fn open_remote(
    endpoint: &RemoteEndpoint,
    credential: &SessionStoreCredential,
    access: AccessMode,
    #[cfg(target_os = "emscripten")] workspace: Option<(PathBuf, String)>,
) -> Result<Arc<SessionResourcesImpl>> {
    #[cfg(target_os = "emscripten")]
    if let Some((_, machine_id)) = workspace.as_ref() {
        crate::sessions::machine::set_explicit(machine_id)?;
    }
    let (data, _initialization) =
        RemoteSessionData::open(endpoint, credential, StoreAccess::of(access)).await?;
    let data = Arc::new(data);
    #[cfg(not(target_os = "emscripten"))]
    let local_port: Arc<dyn LocalExecutionPort> = Arc::new(RemoteExecution::new(
        Arc::clone(&data),
        access == AccessMode::ReadOnly,
    ));
    #[cfg(target_os = "emscripten")]
    let local_port: Arc<dyn LocalExecutionPort> = match workspace {
        Some((root, machine_id)) => {
            anyhow::ensure!(
                access == AccessMode::ReadWrite,
                "workspace execution requires writable store"
            );
            let _ = machine_id;
            Arc::new(WasmExecution::new(
                root,
                crate::sessions::machine::current()?,
            )?)
        }
        None => {
            anyhow::ensure!(
                access == AccessMode::ReadOnly,
                "WASM writable store requires workspace authority"
            );
            Arc::new(WasmExecution::read_only())
        }
    };
    let data_port: Arc<dyn SessionDataPort> = data;
    Ok(Arc::new(SessionResourcesImpl::from_ports(
        data_port,
        local_port,
        SessionDataHome::RemoteStore,
    )))
}
