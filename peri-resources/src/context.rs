//! Resources context — 外部系统访问通道的唯一实例化入口。
//!
//! 本迁移点先落 context 形状 + 唯一实例化入口（TUI 启动处）；
//! Controller/Runtime 建成后消费方随 L2/L3/L5 跟进接入（属预期过渡态，
//! 接口按目标态设计，避免二次返工）。

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use peri_acp_types::session_resources::{
    SessionResourceError, SessionResourceErrorKind, SessionResourceResult, SessionResources,
    SessionStoreShutdownPort,
};
use peri_acp_types::session_store::SessionStoreDeployment;

#[cfg(not(target_os = "emscripten"))]
#[path = "context/sqlite.rs"]
mod sqlite;

use crate::sessions::{
    AccessIntent, CredentialError, LocatorError, ResolvedLocator, SessionStoreOpenRequest,
};

/// 外部系统资源门面
///
/// 存储的构造与关闭由门面内部完成，执行所有权由 peri-sdk 维护。
/// 本结构同时持有两类所有权，且**不可克隆**：业务句柄
/// （[`Resources::session_resources`]）可以克隆任意份交给 Agent/Controller/middleware，
/// 但部署关闭权（[`SessionStoreShutdownOwner`]）只有一份，只能经
/// [`Resources::into_parts`] 连所有权一起交出去——业务侧拿不到它，因此没有任何业务
/// 路径能关闭全局存储。
pub struct Resources {
    session_resources: Arc<dyn SessionResources>,
    shutdown: SessionStoreShutdownOwner,
}

/// 部署生命周期关闭权（non-Clone）：关闭整个会话存储的唯一载体。
///
/// 与业务句柄共享同一个门面实例，但只有本类型能调用关闭；装配点把业务句柄交给
/// Agent/Controller/middleware，把本类型留在部署侧，在**任务排空之后**调用
/// [`SessionStoreShutdownPort::shutdown`]。
pub struct SessionStoreShutdownOwner {
    facade: Arc<crate::sessions::SessionResourcesImpl>,
}

impl SessionStoreShutdownOwner {
    /// 从具体门面实例取关闭权（只给 Resources 层自己的装配路径用）。
    ///
    /// 可见性是关闭权的一部分：crate 外拿不到本方法，因此业务侧即使按具体类型打开了一个
    /// 实例（`SessionResourcesImpl::open*` 只服务 I/O，不含关闭），也无法把它变成部署
    /// 关闭权。crate 外交出关闭权的唯一路径是部署装配入口
    /// （[`Resources::open_deployment`] / [`Resources::open_with`]）加
    /// [`Resources::into_parts`]，由部署在任务排空之后消费一次。构造与关闭之间没有隐含
    /// 状态，重复取用不会产生第二份「关闭进度」。
    pub(crate) fn take(facade: Arc<crate::sessions::SessionResourcesImpl>) -> Self {
        Self { facade }
    }
}

#[async_trait::async_trait]
impl SessionStoreShutdownPort for SessionStoreShutdownOwner {
    async fn shutdown(&self) -> SessionResourceResult<()> {
        self.facade.close().await
    }
}

impl Resources {
    /// Open a remote Session store with a deployment supplied virtual workspace
    /// identity. Local locators are rejected before any I/O.
    #[cfg(not(target_os = "emscripten"))]
    pub async fn open_deployment_in_remote_environment(
        deployment: &SessionStoreDeployment,
        environment: crate::sessions::RemoteWorkspaceEnvironment,
    ) -> Result<Self> {
        let request = SessionStoreOpenRequest::from_deployment(deployment)?;
        let ResolvedLocator::Remote(endpoint) = request.resolve_locator()? else {
            anyhow::bail!("remote workspace environment requires a remote session store");
        };
        let Some(source) = request.credential_source() else {
            return Err(LocatorError::MissingCredentialSource.into());
        };
        let credential = source.resolve()?;
        crate::sessions::open_remote_in_environment(
            &endpoint,
            &credential,
            request.access(),
            environment,
        )
        .await
        .map(Self::from_facade)
        .map_err(|error| error.context("无法打开远程会话存储"))
    }

    /// 打开全部资源（当前为会话存储）。
    ///
    /// 原生目标的默认路径 `~/.peri/threads/threads.db` 写打开失败时会降级为只读打开，
    /// 见 [`Resources::open_with`]。Emscripten 不提供本机库。
    pub async fn open() -> Result<Self> {
        Self::open_request(SessionStoreOpenRequest::local(
            None,
            AccessIntent::ReadWrite,
        ))
        .await
    }

    /// 既有 `--db-path` 兼容入口：归一成一个本机 locator，不在这里解释后端。
    ///
    /// `Some(path)` 使用指定路径；`None` 使用默认路径
    /// `~/.peri/threads/threads.db`。两条路径都先尝试写打开，写打开不可用时再尝试
    /// 只读打开；只有两者都失败才返回包含路径的错误——不静默 fallback 到共享临时
    /// 数据库。
    pub async fn open_with(db_path: Option<PathBuf>) -> Result<Self> {
        Self::open_request(SessionStoreOpenRequest::local(
            db_path,
            AccessIntent::ReadWrite,
        ))
        .await
    }

    /// 按部署参数打开（D §3.1 的最小配置面；D-04 各部署入口的唯一装配点）。
    ///
    /// [`SessionStoreDeployment`] 携带中性定位描述——已确认本机路径（`--db-path`）、
    /// 待解析 locator 原文（`--session-store`：本机路径、远程 locator 或 `env:<变量名>`）
    /// 或默认本机库——另加可选的显式引擎名、凭证**来源**（环境变量名，不接受 token
    /// 字面量）与访问意图。**只设置 `TURSO_URL` 不会切换后端**。
    ///
    /// 部署参数在这里**一次性**转成 typed open request：locator 形态、引擎名与凭证
    /// 来源的冲突全部在进入任何 I/O（含建目录、开库、连接）之前失败，错误保持类型化
    /// （见 [`classify_open_failure`]）。访问意图不是权限：能不能写由打开结果回答；
    /// 只读意图绝不做写探测，也不退回写打开再降级。
    ///
    /// 后端选择只发生在这里：原生本机 locator 走 SQLite 装配，远程 locator 走
    /// Turso adapter。Emscripten 不提供本机库；其可写远端部署须显式提供稳定的
    /// 虚拟工作区身份，见 `Resources::open_turso_writable`。
    ///
    /// 环境变量只在两处被读取：`--session-store env:` 形式的 locator，以及远程 adapter
    /// 打开时按凭证来源取凭证值。**仅仅存在某个云 URL/token 变量不会切换后端**，
    /// 也不影响默认本机库的选择；`--db-path` 的路径既不解 `env:` 也不受这些变量影响。
    pub async fn open_deployment(deployment: &SessionStoreDeployment) -> Result<Self> {
        Self::open_request(SessionStoreOpenRequest::from_deployment(deployment)?).await
    }

    /// Open a writable Turso store for a WASM host with a virtual workspace.
    /// The host must persist this machine UUID and root across module restarts so
    /// that existing sessions can regain execution eligibility.
    #[cfg(target_os = "emscripten")]
    pub async fn open_turso_writable(
        locator: &str,
        token: String,
        workspace_root: PathBuf,
        machine_id: &str,
    ) -> Result<Self> {
        let environment = crate::sessions::RemoteWorkspaceEnvironment::virtual_workspace(
            machine_id,
            workspace_root,
        )?;
        crate::sessions::set_explicit_machine_id(machine_id)?;
        let endpoint = crate::sessions::RemoteEndpoint::parse(
            locator,
            Some(crate::sessions::remote_engine_turso()),
        )?;
        let credential = crate::sessions::credential_from_value(token)?;
        crate::sessions::open_remote_in_environment(
            &endpoint,
            &credential,
            peri_acp_types::session_resources::AccessMode::ReadWrite,
            environment,
        )
        .await
        .map(Self::from_facade)
    }

    /// 唯一的后端选择点（typed 请求版本，供 crate 内装配调用）。
    ///
    /// 原生本机只读意图走独立只读 seam：不创建目录、库、锁或迁移 schema；
    /// 原生本机写打开失败且历史仍可读时降级只读。
    pub(crate) async fn open_request(request: SessionStoreOpenRequest) -> Result<Self> {
        #[cfg(not(target_os = "emscripten"))]
        let read_only = request.intent().is_read_only();
        match request.resolve_locator()? {
            #[cfg(not(target_os = "emscripten"))]
            ResolvedLocator::Default if read_only => Self::open_local_read_only(None).await,
            #[cfg(not(target_os = "emscripten"))]
            ResolvedLocator::Default => Self::open_local(None).await,
            #[cfg(not(target_os = "emscripten"))]
            ResolvedLocator::Local(path) if read_only => {
                Self::open_local_read_only(Some(path)).await
            }
            #[cfg(not(target_os = "emscripten"))]
            ResolvedLocator::Local(path) => Self::open_local(Some(path)).await,
            #[cfg(target_os = "emscripten")]
            ResolvedLocator::Default | ResolvedLocator::Local(_) => {
                Err(LocatorError::LocalStoreUnsupported.into())
            }
            #[cfg(target_os = "emscripten")]
            ResolvedLocator::Remote(_)
                if request.access() != peri_acp_types::session_resources::AccessMode::ReadOnly =>
            {
                Err(LocatorError::ExecutionUnsupported.into())
            }
            ResolvedLocator::Remote(endpoint) => Self::open_remote(&request, &endpoint).await,
        }
    }

    /// Turso locator: all persistent Session facts use the remote store.
    /// Local SQLite is never resolved or opened on this branch.
    async fn open_remote(
        request: &SessionStoreOpenRequest,
        endpoint: &crate::sessions::RemoteEndpoint,
    ) -> Result<Self> {
        // 凭证解析在任何 I/O 之前：来源缺失、变量未设置、空值都在这里失败。
        let Some(source) = request.credential_source() else {
            return Err(LocatorError::MissingCredentialSource.into());
        };
        let credential = source.resolve()?;
        crate::sessions::open_remote(endpoint, &credential, request.access())
            .await
            .map(Self::from_facade)
            .map_err(|error| error.context("无法打开远程会话存储"))
    }

    /// 同一个具体实例的两个所有权面：业务句柄 + 部署关闭权。
    fn from_facade(facade: Arc<crate::sessions::SessionResourcesImpl>) -> Self {
        let session_resources: Arc<dyn SessionResources> = facade.clone();
        Self {
            session_resources,
            shutdown: SessionStoreShutdownOwner::take(facade),
        }
    }

    /// 拆出**业务句柄**与**部署关闭权**。
    ///
    /// 工厂只在这里交出所有权：业务句柄（可克隆）进入 Agent/Controller/middleware，
    /// 关闭权（non-Clone）留在部署侧，由部署在任务排空之后消费一次。
    pub fn into_parts(self) -> (Arc<dyn SessionResources>, SessionStoreShutdownOwner) {
        (self.session_resources, self.shutdown)
    }

    /// 会话资源门面句柄（已迁移消费侧的唯一入口）。
    pub fn session_resources(&self) -> Arc<dyn SessionResources> {
        self.session_resources.clone()
    }

    /// 只取业务句柄，随所有权一起放弃部署关闭权。
    ///
    /// 供没有部署生命周期的调用点（只读命令、测试夹具）使用：它们既不排空、也不
    /// 负责关闭全局存储，连接随句柄释放；有部署生命周期的入口一律走
    /// [`Resources::into_parts`]，不能靠本方法把关闭权丢掉。
    pub fn into_session_resources(self) -> Arc<dyn SessionResources> {
        self.session_resources
    }

    /// 仅测试：crate 内装配点取具体实例（业务侧只有 `Arc<dyn SessionResources>`）。
    #[cfg(test)]
    pub(crate) fn into_concrete_for_test(self) -> Arc<crate::sessions::SessionResourcesImpl> {
        self.shutdown.facade
    }
}

/// 打开会话存储失败的稳定分类。
///
/// 消费侧（`peri meta session` 等只读命令）按这个分类映射退出码与 DTO 文案，不需要
/// downcast 资源层内部类型，也不需要解析错误文本。分类只覆盖**打开阶段**：
/// 打开成功后读取单条会话的失败仍按门面的 [`peri_acp_types::session_resources::SessionResourceError`]
/// 分类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreOpenFailure {
    /// 部署参数/配置错误：locator 形态、引擎名、凭证来源或访问意图相互矛盾。
    NotConfigured,
    /// 目标库不存在（只读打开）。
    NotFound,
    /// 库存在但不可读。
    Unreadable,
    /// schema 与当前构建不兼容。
    SchemaIncompatible,
    /// 已打开的库内容损坏。
    Corrupt,
    /// 存储后端暂时不可用（远程连接/传输/服务端错误、超时、只读拒绝）。
    Unavailable,
    /// 其余内部错误。
    Internal,
}

/// 按类型化 source chain 分类打开失败，不解析错误文本、不回显 locator 或凭证。
pub fn classify_open_failure(error: &anyhow::Error) -> StoreOpenFailure {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error.as_ref());
    while let Some(current) = source {
        if current.downcast_ref::<LocatorError>().is_some() {
            return StoreOpenFailure::NotConfigured;
        }
        // 凭证**来源**的问题（变量名非法、未设置、空值、非 Unicode，以及直接注入的空值）
        // 同样是配置错误，不是「后端不可用」也不是「内部错误」——按类型判定，不解析错误
        // 文本；`CredentialError` 只携带变量名，不带值，因此分类不会回显任何凭证内容。
        if current.downcast_ref::<CredentialError>().is_some() {
            return StoreOpenFailure::NotConfigured;
        }
        // 远程后端的暂时不可用（传输/忙碌/服务端错误/超时）在打开阶段同样是
        // 「后端不可用」而不是「内部错误」：分类按类型，不解析错误文本，也不回显 locator。
        if let Some(remote) = current.downcast_ref::<SessionResourceError>() {
            match remote.kind() {
                SessionResourceErrorKind::Unavailable { .. }
                | SessionResourceErrorKind::Timeout
                | SessionResourceErrorKind::ReadOnlyStore => return StoreOpenFailure::Unavailable,
                SessionResourceErrorKind::Corrupt { .. } => return StoreOpenFailure::Corrupt,
                _ => {}
            }
        }
        #[cfg(not(target_os = "emscripten"))]
        if let Some(read_only) = current.downcast_ref::<crate::sessions::ReadOnlyThreadStoreError>()
        {
            return match read_only {
                crate::sessions::ReadOnlyThreadStoreError::DatabaseNotFound => {
                    StoreOpenFailure::NotFound
                }
                crate::sessions::ReadOnlyThreadStoreError::DatabaseUnreadable => {
                    StoreOpenFailure::Unreadable
                }
                crate::sessions::ReadOnlyThreadStoreError::SchemaIncompatible => {
                    StoreOpenFailure::SchemaIncompatible
                }
                crate::sessions::ReadOnlyThreadStoreError::CorruptSessionData => {
                    StoreOpenFailure::Corrupt
                }
                crate::sessions::ReadOnlyThreadStoreError::SessionNotFound
                | crate::sessions::ReadOnlyThreadStoreError::Internal => StoreOpenFailure::Internal,
            };
        }
        source = current.source();
    }
    StoreOpenFailure::Internal
}

#[cfg(test)]
#[path = "context_test.rs"]
mod tests;
