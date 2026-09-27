//! 远程组合装配：把远端数据 adapter 与本机执行面装进同一个门面。
//!
//! 组合层是唯一知道「数据在哪、本机执行事实在哪」的地方，门面只看到两个端口：
//!
//! ```text
//! SessionResourcesImpl
//!   ├── data     : RemoteSessionData  （canonical 会话数据，远端）
//!   └── local    : LocalExecution     （本机 workspace 证据、代际、OS 锁）
//! ```
//!
//! 顺序固定，每一步都不能省：
//!
//! 1. **凭证解析**：只按显式来源取环境变量；解析失败在建立任何 I/O 之前返回（不猜、不回落）。
//! 2. **本机执行面**：写意图打开（必要时升级本机 schema）；只读意图只读打开，库不存在时
//!    如实失败（`DatabaseNotFound`，见 [`open_local_execution`]）——本机执行事实只在这个
//!    库里，没有它就没有可用的执行面，而只读意图不许把它建出来。这与本机库的只读打开是
//!    同一个判定：两种存储模式下「只读打开一个不存在的库」都按 `NotFound` 拒绝。
//! 3. **远端打开**：读回 store 身份（只读打开遇上未初始化的 store 直接拒绝）。
//! 4. **门面装配**：数据端口是远端 adapter，执行面是本机执行面。
//!
//! 远端与本地不是两套公开行为：门面（`SessionResourcesImpl`）只有一套，组合只决定注入。
//! 配置即用：配了哪个 store 就直接用，没有「本机未登记 → 拒绝执行」这一步（v10 撤销）。

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use peri_acp_types::session_resources::AccessMode;

use crate::sessions::data::SessionDataPort;
use crate::sessions::local_port::LocalExecutionPort;
use crate::sessions::resources::SessionDataHome;
use crate::sessions::sqlite_store::LocalExecution;
use crate::sessions::SessionResourcesImpl;

use super::credentials::SessionStoreCredential;
use super::endpoint::RemoteEndpoint;
use super::mutation::StoreAccess;
use super::session_data::RemoteSessionData;

/// 打开远程会话存储并装配门面。
///
/// 凭证是**值**而不是来源：解析发生在 D 边界（读取进程环境的唯一位置），组合层只消费
/// 已解析的值，因此云实验可以把进程内解析出的凭证直接注入，不必把它写进进程环境。
///
/// `registry_path` 是本机执行事实所在（workspace 登记、执行代际、sidecar 锁）；它**不是**
/// canonical 数据的位置，因此远程组合不会把它当作会话库来读写。
pub(crate) async fn open_remote(
    endpoint: &RemoteEndpoint,
    credential: &SessionStoreCredential,
    access: AccessMode,
    registry_path: PathBuf,
) -> Result<Arc<SessionResourcesImpl>> {
    // 本机执行面先于远端打开：缺库的只读意图在这里就如实失败，不会先把远端连接建起来。
    let local = open_local_execution(access, &registry_path).await?;
    let (data, _initialization) =
        RemoteSessionData::open(endpoint, credential, StoreAccess::of(access)).await?;
    let data_port: Arc<dyn SessionDataPort> = Arc::new(data);
    let local_port: Arc<dyn LocalExecutionPort> = Arc::new(local);
    Ok(Arc::new(SessionResourcesImpl::from_ports(
        data_port,
        local_port,
        SessionDataHome::RemoteStore,
    )))
}

/// 本机执行面：写意图可以创建/升级，只读意图只读打开、不创建任何东西。
///
/// 本机库不存在时**如实失败**（`DatabaseNotFound`）：workspace 证据、执行代际与 sidecar
/// 锁都是本机库持有的事实，没有它就没有可用的执行面。只读打开不创建文件是硬约束
/// （不能用「补一个空库」把它变成可写打开），因此这里不回退。
async fn open_local_execution(
    access: AccessMode,
    registry_path: &PathBuf,
) -> Result<LocalExecution> {
    match access {
        AccessMode::ReadWrite => LocalExecution::open(registry_path).await,
        AccessMode::ReadOnly => Ok(LocalExecution::open_existing_read_only(registry_path).await?),
    }
}
