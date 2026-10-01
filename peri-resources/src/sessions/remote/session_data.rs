//! 远程会话数据 adapter：把 [`SessionDataPort`] 的会话行为落到远端 schema 上。
//!
//! ## 本阶段落地范围（C-03 第一、二批）
//!
//! | 行为 | 状态 |
//! | --- | --- |
//! | `load_snapshot` / `load_meta` / `load_binding` / `load_session_history` | 已实现（一致读取；flags 由一致快照一并返回） |
//! | `list_sessions`（scoped 分页）/ `list_children` / `list_session_tree` | 已实现 |
//! | `save_new_session` | 已实现（meta + 绑定 + frozen 一次落库） |
//! | `save_fork` | 已实现（映射后的 payload + flags，source 不变） |
//! | `save_child` | 已实现（父子/根归属 + 继承区 + root frozen 原文） |
//! | `update_meta`（title/status/cancel_policy/config 定向更新） | 已实现 |
//! | `append_history` / `apply_message_projections` / `apply_compaction` | 已实现（[`super::session_history`]：批内守卫，整批生效或整批不生效） |
//! | `rewind_history`（显式两边界）/ `remove_history_entries` | 已实现（同上；未知截止点保持无变更语义） |
//! | `delete_tree` / `revoke_unpublished_session` / `adopt_legacy_session` | 已实现（[`super::session_lifecycle`]；远端无墓碑/执行行，删除是刻意删除数据事实） |
//! | `load_child_resume_record` / `store_child_resume_record` | 已实现（`agent_status` + 由状态派生的认领标记） |
//! | `drain` | 已实现为「无队列可排空，但未结清不算已排空」（见方法文档） |
//! | `close` | 已实现（真正关闭连接，之后写入明确失败） |
//! | `recover_persistence` | 已实现：本机已无可求证的 durable 记录，会话数据可读即 `Recovered`（跨进程未决判定随 v10 撤销） |
//!
//! 写入路径：**每次调用唯一操作 id + 资格先于效果**（远端账本 `peri_op_ledger` 的资格写入
//! 与业务效果在同一个托管批里同生共死）。操作 id 不由内容派生，因此同内容的第二次、第三次
//! 领域调用都是新操作（状态 A→B→A、标题 x→y→x 不再被当成重放丢弃）；输入摘要只用于一致性
//! 校验。v10 撤销本机操作日志后，不再有「发送前本机落盘、确定终态才结清」这一步，未结清只
//! 在活跃租约上表达（见 `recover_persistence` 的方法文档）。等价的公开行为仍只有门面暴露的
//! 那 33 条——adapter 不另立一套平行行为。
//!
//! 本机执行事实不在本模块：workspace 证据与进程内运行句柄由 `LocalExecution` 持有
//! （见 `sessions::local_port`）。adapter 只回答 canonical 数据事实。
//!
//! 打开的两种访问模式：
//!
//! - 可写打开：读回身份（未初始化则先建身份），再执行一次幂等 DDL 补齐会话表形状；
//! - 只读打开：只读回身份，不写任何东西；若 store 尚无本任务 schema（身份或会话表缺失），
//!   直接 `Unsupported`——没有 schema 就没有会话事实可读，也不越权建表。
//!
//! ## 边界（远程不做本机的事）
//!
//! - 不持有本机执行事实：运行句柄及未结清标记只在当前实例内，远端不持久化这些事实；
//! - 不解析本机目录、不发执行资格、不判定 legacy：`LegacyConfirmed` 与执行准入由门面与
//!   执行面按本机证据判定（这也是 `load_binding` 只回答绑定事实的原因）；

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use peri_acp_types::messages::MessageId;
use peri_acp_types::session_resources::{
    BindingState, ChildSnapshot, ForkSnapshot, FrozenSnapshotBytes, NewSession,
    PersistenceRecovery, RewindBoundary, SessionMetaPatch, SessionResourceError,
    SessionResourceErrorKind, SessionResourceResult, SessionSnapshot,
};
use peri_acp_types::store::{CompactionChange, MessageFlags, PersistedPayload};
use peri_acp_types::thread::{ThreadId, ThreadMeta};
use peri_acp_types::workspace::{
    ResolvedWorkspace, ScopedThreadPage, ScopedThreadQuery, SessionBinding,
};
use tokio::sync::{RwLock, RwLockReadGuard};

use crate::sessions::data::{ChildResumeRecord, SessionDataPort};

use super::credentials::SessionStoreCredential;
use super::endpoint::RemoteEndpoint;
use super::generation::{ConnectionFactory, ConnectionGate, RemoteConnectionFactory};
use super::mutation::{incomplete_reply, RemoteStore, StoreAccess};
use super::schema::{self, StoreId, StoreIdentityOutcome, StoreIdentityRead};
use super::schema_upgrade;
use super::session_schema;
use super::sql::{int_at, StatementSpec};
use turso_serverless::Value;

/// 本次打开对远端 store 身份做了什么：首次登记资格的唯一证据。
///
/// 「远端此前为空」只由**本次打开建立身份**证明；读到既有身份说明这个 store 已经有数据，
/// 无论本机是否第一次见到它，都不构成自动认领的理由。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StoreInitialization {
    /// 远端此前已有本任务 schema（本次只读回身份）。
    Existing,
    /// 远端此前明确为空，本次打开建立了 store 身份。
    CreatedByThisOpen,
}

/// 只读身份读取之后的下一步（纯函数结论）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum OpenStep {
    /// 已有本构建认识的身份：本次打开没有建立任何东西。
    Existing(StoreId),
    Upgrade(schema::StoreSnapshot),
    /// 明确为空：写打开在这里才继续初始化；只读打开到这一步就拒绝。
    NeedsInitialization,
}

/// 身份读取 + 访问意图 → 打开的下一步（纯函数，真引擎 seam 与生产共用）。
///
/// 三条拒绝路径都不猜：版本/契约不认识、元数据形状不可解释、只读打开遇上尚未初始化的
/// store（没有 schema 就没有会话事实可读，也不越权建表）。
pub(super) fn open_step(
    read: StoreIdentityRead,
    access: StoreAccess,
) -> SessionResourceResult<OpenStep> {
    match read {
        StoreIdentityRead::Present(snapshot) if snapshot.matches_build() => {
            Ok(OpenStep::Existing(snapshot.store_id))
        }
        StoreIdentityRead::Present(snapshot) if snapshot.readable() => match access {
            StoreAccess::ReadOnly => Ok(OpenStep::Existing(snapshot.store_id)),
            StoreAccess::ReadWrite => Ok(OpenStep::Upgrade(snapshot)),
        },
        StoreIdentityRead::Present(_) => {
            Err(unsupported_behavior("unrecognized remote store schema"))
        }
        StoreIdentityRead::Malformed => Err(super::session_codec::corrupt(
            "remote session store metadata is not interpretable",
        )),
        StoreIdentityRead::Uninitialized => match access {
            StoreAccess::ReadOnly => Err(unsupported_behavior(
                "read-only open of an uninitialized remote store",
            )),
            StoreAccess::ReadWrite => Ok(OpenStep::NeedsInitialization),
        },
    }
}

/// 身份竞争的结论 → 权威身份 + 本次打开的初始化事实（首次登记资格的唯一映射）。
///
/// `CreatedByThisOpen` 只能由 `Created` 产生：竞败方读回的胜者身份也是 `Existing`，
/// 所以败方没有首次登记资格，但它用的仍是同一个权威身份。
pub(super) fn open_verdict(outcome: StoreIdentityOutcome) -> (StoreId, StoreInitialization) {
    match outcome {
        StoreIdentityOutcome::Created(store_id) => {
            (store_id, StoreInitialization::CreatedByThisOpen)
        }
        StoreIdentityOutcome::Existing(store_id) => (store_id, StoreInitialization::Existing),
    }
}

/// 旧形状探测（只读、单条 SELECT）：写打开遇上未初始化的库时，先问一次「这里有没有
/// 统一之前的远端会话表」。
///
/// 命中即拒绝（`Unsupported`）：这不是空库，而是一个本构建不认识的旧形状库。
/// **不自动迁移**——迁移要重命名表并搬运每一行，而本段的运行前提是新库没有历史数据；
/// **也不覆盖**——覆盖等于替使用者丢掉他看不见的数据。只发一条只读语句，不建表、不写行。
async fn refuse_legacy_shape(store: &RemoteStore) -> SessionResourceResult<()> {
    let row = store
        .fetch_row(&StatementSpec::new(
            schema::COUNT_LEGACY_TABLES_SQL,
            schema::LEGACY_SHAPE_TABLES
                .iter()
                .map(|table| Value::Text((*table).to_owned()))
                .collect(),
        ))
        .await?;
    // 读不到计数不是「没有旧表」：形状不完整的读取按未决上报，不放行初始化。
    let count = row
        .as_ref()
        .and_then(|values| int_at(values, 0))
        .ok_or_else(|| incomplete_reply("legacy shape probe returned no count"))?;
    if count > 0 {
        return Err(unsupported_behavior(
            "remote store has the pre-unification session tables; it is not migrated automatically",
        ));
    }
    Ok(())
}

/// 远端会话数据 adapter：一个已初始化（或已读回身份）的远程 store 上的会话行为。
///
/// 连接在关闭**确认**之前一直活着：服务期间留在槽里；关闭一开始就离开业务路径、移进关闭
/// 句柄（[`ClosingConnection`]），由它保留真实资源并允许重试，`close` 成功返回才算确认。
/// 一次已经发出的在途请求被丢弃（超时、取消）之后，用到它的那一代连接被记为失效，
/// **下一次访问按同一份打开事实重建**（见 [`Self::store`]）。关闭开始之后（无论上一次成功
/// 与否）再没有重连：槽里没有可服务的连接时如实返回关闭错误，绝不用一次重连把关闭事实盖掉。
///
/// 本机**不再**持有远端操作的日志（v10 删除了 `session_remote_operations`）：远端账本
/// （`peri_op_ledger`）仍按「资格先于效果」写，但它是**远端**事实，本机不复制。跨进程重启
/// 后没有「按原 id 求证终态」这条路径，未结清只在本进程的租约上表达。
pub(super) struct RemoteSessionData {
    /// 连接的生命周期槽位：服务中，或关闭中（含已确认关闭）。
    slot: RwLock<ConnectionSlot>,
    /// 建立连接的地方（本 crate 唯一持有凭证处）；重建不改变打开事实。
    factory: Arc<dyn ConnectionFactory>,
    /// 连接代际门禁：哪一代已经不可信。
    gate: Arc<ConnectionGate>,
    store_id: StoreId,
    /// thread → root 解析缓存：父关系创建后不变（本 adapter 不提供改父行为），因此
    /// 同一条会话只需一次远端上溯；解析失败不缓存，避免把网络失败固化成事实。
    roots: RwLock<HashMap<ThreadId, ThreadId>>,
}

impl RemoteSessionData {
    /// 打开远程会话数据：读回 store 身份；未初始化时只在可写打开下创建本任务 schema。
    ///
    /// 返回值里的 [`StoreInitialization`] 是「首次登记」的唯一证据，且只有一个来源：
    /// [`open_verdict`] 对身份竞争结论（[`StoreIdentityOutcome`]）的映射——只有本事务确切
    /// 插入元数据行并提交（`Created`）才是 `CreatedByThisOpen`。初始化结果未知（丢响应、
    /// 超时）会让本次打开直接失败：既不发身份，也不发创建事实。
    ///
    /// 三条拒绝路径都不猜：版本/契约不认识、元数据形状不可解释、只读打开遇上尚未初始化的
    /// store（没有 schema 就没有会话事实可读，也不越权建表）。
    pub(super) async fn open(
        endpoint: &RemoteEndpoint,
        credential: &SessionStoreCredential,
        access: StoreAccess,
    ) -> SessionResourceResult<(Self, StoreInitialization)> {
        let gate = Arc::new(ConnectionGate::default());
        let factory: Arc<dyn ConnectionFactory> = Arc::new(RemoteConnectionFactory::new(
            endpoint.clone(),
            credential.duplicate(),
            access,
            Arc::clone(&gate),
        ));
        let store = factory.connect().await?;
        let (store_id, initialization) = match open_step(store.read_identity().await?, access)? {
            OpenStep::Existing(store_id) => (store_id, StoreInitialization::Existing),
            OpenStep::Upgrade(snapshot) => {
                schema_upgrade::upgrade(&store, &snapshot).await?;
                (snapshot.store_id, StoreInitialization::Existing)
            }
            // 空库：写打开在这里才参与身份竞争，结论由本事务的结果给出（见 [`open_verdict`]），
            // 不由「刚才读到空库」推定创建。
            OpenStep::NeedsInitialization => {
                // 空库与「旧形状的库」在身份读取上都读作未初始化：先问一次后者，命中即拒绝。
                refuse_legacy_shape(&store).await?;
                open_verdict(store.initialize_store().await?)
            }
        };
        // 可写打开时补齐本构建的会话表形状：DDL 全部 `IF NOT EXISTS`，既有对象不改写、
        // 不覆盖，已初始化的 store 上是一次幂等的空操作。这一步不能只在「身份刚建立」时
        // 跑——身份早于会话表建立的 store（例如只做过机制实测的库）同样需要补齐。
        if access == StoreAccess::ReadWrite {
            crate::sessions::machine::initialize().await.map_err(|_| {
                crate::sessions::sqlite_store::unavailable("machine identity initialization failed")
            })?;
            // 父行检查先归位：canonical 形状里的外键在远端没有可满足的父行（见方法文档）。
            store.force_parent_checks_off().await?;
            store
                .apply_schema(session_schema::initialization_plan())
                .await?;
            store
                .apply_schema(vec![StatementSpec::new(
                    crate::sessions::canonical::BACKFILL_ENVIRONMENTS_SQL,
                    vec![Value::Text(format!("legacy:{}", store_id.as_str()))],
                )])
                .await?;
        }
        Ok((
            Self {
                slot: RwLock::new(ConnectionSlot::serving(store)),
                factory,
                gate,
                store_id,
                roots: RwLock::new(HashMap::new()),
            },
            initialization,
        ))
    }

    /// 操作所属 root：已有会话沿父链上溯（带缓存），解析失败退回自身。
    ///
    /// 退回自身只影响未决阻塞的范围（这一条会话而不是整棵树），不会把未知写当成已知：
    /// 该记录仍然是未结清状态，门禁照常阻塞该会话。失败结果**不**进缓存。
    pub(super) async fn root_for(&self, id: &ThreadId) -> ThreadId {
        if let Some(root) = self.roots.read().await.get(id) {
            return root.clone();
        }
        match self.root_of(id).await {
            Ok(root) => {
                self.roots.write().await.insert(id.clone(), root.clone());
                root
            }
            Err(_) => id.clone(),
        }
    }

    /// 取当前连接（已就绪的借用）。
    ///
    /// 只有「槽里还有一条**服务中**的连接、但这一代已被记为失效」才重建，而且**只重建一次**：
    /// 失败如实上报，下一次调用再试。槽里没有可服务的连接（关闭已经开始、关闭已确认，或
    /// 关闭态装配）一律返回关闭错误——一次重连绝不能把「已经开始的关闭」变回来。
    ///
    /// 重建判定与取守卫之间还有一个空窗：另一条在途调用被放弃（超时、取消）会把**当前**这一代
    /// 记为失效。拿到读锁之后再复核一次，就不会把已知失效的连接交出去——本调用什么都没发，
    /// 只如实失败（[`Unavailable`](SessionResourceErrorKind::Unavailable)），下一次访问按它重建。
    /// 不做自动重试：重建是下一次访问的事，本次绝不把请求发到一条已经不可证明的连接上。
    pub(super) async fn store(&self) -> SessionResourceResult<RwLockReadGuard<'_, RemoteStore>> {
        self.reconnect_if_retired().await?;
        let slot = self.slot.read().await;
        let store = RwLockReadGuard::try_map(slot, ConnectionSlot::serving_store)
            .map_err(|_| connection_closed())?;
        if self.gate.is_invalid(store.generation()) {
            return Err(retired_connection());
        }
        Ok(store)
    }

    /// 失效代际的重建：建立新连接、重核实身份、替换槽位、关闭退场连接。
    async fn reconnect_if_retired(&self) -> SessionResourceResult<()> {
        let Some(retired) = self.retired_generation().await else {
            return Ok(());
        };
        let replacement = self.factory.connect().await?;
        if let Err(error) = self.verify_reconnect(&replacement).await {
            // 没能证明「还是这个库」的新连接不装回槽里：关闭它，按原错误上报。
            retire(replacement).await;
            return Err(error);
        }
        let replaced = {
            let mut slot = self.slot.write().await;
            match slot.serving_store().map(RemoteStore::generation) {
                // 槽在重建期间失去服务中的连接（关闭开始把连接移进关闭句柄）：不复活，不装回去。
                None => {
                    drop(slot);
                    retire(replacement).await;
                    return Err(connection_closed());
                }
                // 别的调用已经重建好了：自己这条一次请求都没发过，丢弃即可。
                Some(current) if current != retired => {
                    drop(slot);
                    retire(replacement).await;
                    return Ok(());
                }
                Some(_) => slot.replace_serving(replacement),
            }
        };
        // 退场连接的关闭在锁外做，而且不阻塞其它访问：重建路径不能因为一次关闭把别人堵住。
        if let Some(replaced) = replaced {
            retire(replaced).await;
        }
        Ok(())
    }

    /// 当前**服务中**的连接是否已被记为失效；返回那一代的代际号。
    ///
    /// 关闭中的槽没有服务中的连接，也就没有「重建」这回事（返回 `None`：业务读如实失败，
    /// 不重连）。
    async fn retired_generation(&self) -> Option<u64> {
        let slot = self.slot.read().await;
        let store = slot.serving_store()?;
        self.gate
            .is_invalid(store.generation())
            .then(|| store.generation())
    }

    /// 重建后的重核实：新连接必须就是**同一个** store，且本构建认识它。
    ///
    /// 只读检查——不建表、不初始化、不登记：重连不改变任何事实，只证明「还是这个库」。
    /// 只读打开因此与可写打开走同一条重建路径，且不产生任何写入。
    async fn verify_reconnect(&self, store: &RemoteStore) -> SessionResourceResult<()> {
        match store.read_identity().await? {
            StoreIdentityRead::Present(snapshot)
                if !(snapshot.matches_build()
                    || (store.access() == StoreAccess::ReadOnly && snapshot.readable())) =>
            {
                Err(unsupported_behavior(
                    "unrecognized remote store schema after reconnect",
                ))
            }
            StoreIdentityRead::Present(snapshot)
                if snapshot.store_id.as_str() != self.store_id.as_str() =>
            {
                Err(unsupported_behavior(
                    "reconnect reached a different remote store",
                ))
            }
            StoreIdentityRead::Present(_) => Ok(()),
            StoreIdentityRead::Malformed => Err(super::session_codec::corrupt(
                "remote session store metadata is not interpretable after reconnect",
            )),
            StoreIdentityRead::Uninitialized => Err(unsupported_behavior(
                "reconnected remote store has no session schema",
            )),
        }
    }

    /// 装载故障计划（仅测试构建）：把「响应丢失」「发出前丢弃」变成可控观察点，
    /// 走的是同一套真实批、真实账本与真实恢复路径。
    #[cfg(test)]
    pub(super) async fn inject_faults(&self, plan: super::mutation::FaultPlan) {
        if let Some(store) = self.slot.read().await.serving_store() {
            store.inject_faults(plan);
        }
    }

    /// 测试装配：连接已关闭的 adapter（不连网，与 `close` 之后的状态同一个形状）。
    ///
    /// 这种装配下任何一次 store 访问都只会失败，所以「输入不自洽时仍然拿到 `InvalidInput`」
    /// 就证明判定发生在取连接之前、也没有写下任何本机记录。
    ///
    /// 注意这与「关闭过一次但没成功」**不同**：那种情况下连接仍被保留在关闭句柄里，
    /// 关闭可以重试（见 [`RemoteSessionData::close`]）；这里从来没有过连接可关。
    #[cfg(test)]
    pub(super) fn closed_for_test(store_id: StoreId) -> Self {
        Self {
            slot: RwLock::new(ConnectionSlot::default()),
            factory: Arc::new(NoConnectionFactory),
            gate: Arc::new(ConnectionGate::default()),
            store_id,
            roots: RwLock::new(HashMap::new()),
        }
    }

    /// 测试装配：连接由调用方给定的工厂与首条连接构成（故障可控，不连网）。
    ///
    /// 首条连接与工厂共用同一份代际门禁：失效、重建与「迟到任务不碰新连接」的判定与生产
    /// 完全一致，测试只是把传输面换成能确定复现故障的实现。
    #[cfg(test)]
    pub(super) fn with_connection_for_test(
        store_id: StoreId,
        connection: RemoteStore,
        factory: Arc<dyn ConnectionFactory>,
        gate: Arc<ConnectionGate>,
    ) -> Self {
        Self {
            slot: RwLock::new(ConnectionSlot::serving(connection)),
            factory,
            gate,
            store_id,
            roots: RwLock::new(HashMap::new()),
        }
    }
}

/// 关闭态装配用的连接工厂：任何一次重建都明确失败（关闭的实例不重连）。
#[cfg(test)]
struct NoConnectionFactory;

#[cfg(test)]
#[async_trait]
impl ConnectionFactory for NoConnectionFactory {
    async fn connect(&self) -> SessionResourceResult<RemoteStore> {
        Err(connection_closed())
    }
}

/// 退场连接的收尾：尝试关闭并记下结果。
///
/// 关闭失败**不**掩盖本次重建的结论：这一代本来就已经不可信，重连的意义是让后续访问有路
/// 可走；但也不能悄悄丢掉——失败按诊断记录，不含凭证、URL 或语句内容。
async fn retire(store: RemoteStore) {
    if store.close().await.is_err() {
        tracing::debug!("retired remote connection did not close cleanly");
    }
}

/// 连接的生命周期槽位：**服务中**，或**关闭中**（含已确认关闭）。
///
/// 两个字段互斥，翻转点只有一个（[`Self::begin_close`]）：取走 `serving`、装上 `closing`。
/// 空槽（两者皆空）只属于「从来没有过连接」的装配。
struct ConnectionSlot {
    /// 服务中的连接；关闭开始后为空——业务读不再经过它，也不会因此重连。
    serving: Option<RemoteStore>,
    /// 关闭句柄；关闭开始后一直保留（含已确认关闭），真实资源的关闭进度在它里面。
    closing: Option<Arc<ClosingConnection>>,
}

impl ConnectionSlot {
    /// 装配一条服务中的连接。
    fn serving(store: RemoteStore) -> Self {
        Self {
            serving: Some(store),
            closing: None,
        }
    }

    /// 服务中的连接（关闭中、空槽为 `None`）。
    fn serving_store(&self) -> Option<&RemoteStore> {
        self.serving.as_ref()
    }

    /// 换掉服务中的连接，返回退场的那一条；只在确认槽里有服务中的连接时调用。
    fn replace_serving(&mut self, store: RemoteStore) -> Option<RemoteStore> {
        self.serving.replace(store)
    }

    /// 开始关闭（幂等）：把服务中的连接移进关闭句柄；已经在关闭中时复用同一个句柄。
    ///
    /// `None` 只出现在空槽（从来没有过连接）：这与「关闭失败」**不同**——失败时句柄已经被
    /// 保留下来，重试关闭的是同一条真实连接。
    fn begin_close(&mut self) -> Option<Arc<ClosingConnection>> {
        if let Some(closing) = &self.closing {
            return Some(Arc::clone(closing));
        }
        let store = self.serving.take()?;
        let closing = Arc::new(ClosingConnection::new(store));
        self.closing = Some(Arc::clone(&closing));
        Some(closing)
    }
}

impl Default for ConnectionSlot {
    /// 空槽：连接从来没有过（关闭态装配）。
    fn default() -> Self {
        Self {
            serving: None,
            closing: None,
        }
    }
}

/// 关闭句柄：连接退出业务路径之后，真实资源的关闭进度被保留在这里。
///
/// 唯一句柄在**句柄里**而不在关闭 future 里：调用方超时、取消或整个 future 被丢弃都不会丢
/// 连接。关闭失败或被取消都不确认（[`CloseProgress::Unconfirmed`]），下一次调用在**同一条
/// 连接**上继续关——不新建连接，也不把一次未确认的失败固化成永久错误。
struct ClosingConnection {
    /// 待关闭的连接：确认关闭成功之前一直保留。
    connection: RemoteStore,
    /// 关闭尝试的串行点：并发关闭共享同一次真实关闭的结论，成功只发生一次。
    progress: tokio::sync::Mutex<CloseProgress>,
}

/// 真实关闭的进度：只有「已确认成功」会让关闭不再重做。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CloseProgress {
    /// 尚未成功（从未尝试，或上一次失败/被取消）：允许重试。
    Unconfirmed,
    /// 已确认成功：不再重复关闭同一条连接，后续关闭幂等成功。
    Confirmed,
}

impl ClosingConnection {
    fn new(connection: RemoteStore) -> Self {
        Self {
            connection,
            progress: tokio::sync::Mutex::new(CloseProgress::Unconfirmed),
        }
    }

    /// 真实关闭；并发调用被串行化，成功只发生一次。
    ///
    /// 失败如实上报且**不**确认：连接仍在句柄里，下一次调用在同一条连接上继续关闭。
    /// 调用 future 被丢弃只放弃这一次尝试——句柄与进度都不在 future 里，因此什么也不会丢。
    async fn shutdown(&self) -> SessionResourceResult<()> {
        let mut progress = self.progress.lock().await;
        if *progress == CloseProgress::Confirmed {
            return Ok(());
        }
        self.connection.close().await?;
        *progress = CloseProgress::Confirmed;
        Ok(())
    }
}

/// 槽里没有可服务的连接（关闭已经开始、关闭已确认，或从来没有过连接）：不是「没有连接就没问题」。
fn connection_closed() -> SessionResourceError {
    SessionResourceError::new(SessionResourceErrorKind::Internal {
        detail: "remote session store connection is closed".to_owned(),
    })
}

/// 这一代在守卫交出去之前就被记为失效：本调用**没有发出任何请求**。
///
/// 保守归类为 `Unavailable`（连接这一刻不可用），而不是「关闭」或「契约」类错误：这不是
/// 会话数据的结论，也不代表远端拒绝；下一次访问会按同一份打开事实重建。
fn retired_connection() -> SessionResourceError {
    SessionResourceError::new(SessionResourceErrorKind::Unavailable {
        detail: "remote session store connection was retired before use".to_owned(),
    })
}

pub(super) fn not_found() -> SessionResourceError {
    SessionResourceError::new(SessionResourceErrorKind::NotFound)
}

pub(super) fn invalid_input(detail: &str) -> SessionResourceError {
    SessionResourceError::new(SessionResourceErrorKind::InvalidInput {
        detail: detail.to_owned(),
    })
}

/// 本阶段尚未落地的行为：明确失败，并留下行为名便于诊断（不含任何会话内容）。
fn unsupported_behavior(behavior: &'static str) -> SessionResourceError {
    tracing::debug!(behavior, "remote session data behavior is not implemented");
    SessionResourceError::new(SessionResourceErrorKind::Unsupported)
}

#[async_trait]
impl SessionDataPort for RemoteSessionData {
    fn oauth_credentials(
        self: Arc<Self>,
    ) -> Option<Arc<dyn peri_acp_types::oauth_credentials::OAuthCredentialPort>> {
        Some(Arc::new(super::oauth_credentials::RemoteOAuthCredentials(
            self,
        )))
    }

    async fn machine_id_of(&self, id: &ThreadId) -> SessionResourceResult<Option<String>> {
        let store = self.store().await?;
        if store.fetch_row(&StatementSpec::bare("SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'session_environments'")).await?.is_none() {
            return Ok(None);
        }
        let row = store
            .fetch_row(&StatementSpec::new(
                "SELECT machine_id FROM session_environments WHERE thread_id = ?1",
                vec![Value::Text(id.clone())],
            ))
            .await?;
        Ok(row.and_then(|values| super::sql::text_at(&values, 0).map(str::to_owned)))
    }
    async fn save_new_session(&self, input: &NewSession) -> SessionResourceResult<()> {
        self.write_new_session(input).await
    }

    async fn save_new_session_draft(
        &self,
        draft: &peri_acp_types::session_resources::NewSessionDraft,
    ) -> SessionResourceResult<()> {
        self.write_new_session_draft(draft).await
    }

    async fn commit_frozen(
        &self,
        id: &ThreadId,
        frozen: &FrozenSnapshotBytes,
    ) -> SessionResourceResult<()> {
        self.write_commit_frozen(id, frozen).await
    }

    async fn revoke_unpublished_session(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.revoke_unpublished(id).await
    }

    async fn revoke_unpublished_draft(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.revoke_unpublished_draft(id).await
    }

    async fn adopt_legacy_session(
        &self,
        id: &ThreadId,
        saved_cwd: &str,
        workspace: &ResolvedWorkspace,
        frozen: &FrozenSnapshotBytes,
    ) -> SessionResourceResult<()> {
        self.adopt_legacy(id, saved_cwd, workspace, frozen).await
    }

    async fn load_snapshot(&self, id: &ThreadId) -> SessionResourceResult<SessionSnapshot> {
        self.read_snapshot(id).await
    }

    async fn load_binding(&self, id: &ThreadId) -> SessionResourceResult<BindingState> {
        self.read_binding_state(id).await
    }

    async fn load_session_history(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<Vec<PersistedPayload>> {
        self.read_history(id).await
    }

    async fn load_meta(&self, id: &ThreadId) -> SessionResourceResult<ThreadMeta> {
        self.read_meta(id).await
    }

    async fn session_exists(&self, id: &ThreadId) -> SessionResourceResult<bool> {
        self.exists(id).await
    }

    async fn binding_of(&self, id: &ThreadId) -> SessionResourceResult<Option<SessionBinding>> {
        self.binding_of(id).await
    }

    async fn session_root(&self, id: &ThreadId) -> SessionResourceResult<ThreadId> {
        // 远端父链的根；上溯失败按「解析不出」退回自身（范围变窄，但不会把未知当成已知）。
        Ok(self.root_for(id).await)
    }

    async fn list_sessions(
        &self,
        query: &ScopedThreadQuery,
    ) -> SessionResourceResult<ScopedThreadPage> {
        self.read_page(query).await
    }

    async fn list_children(&self, parent: &ThreadId) -> SessionResourceResult<Vec<ThreadMeta>> {
        self.read_children(parent).await
    }

    async fn list_session_tree(&self, root: &ThreadId) -> SessionResourceResult<Vec<ThreadMeta>> {
        self.read_tree(root).await
    }

    async fn append_history(
        &self,
        id: &ThreadId,
        payloads: &[PersistedPayload],
    ) -> SessionResourceResult<()> {
        self.write_history_append(id, payloads).await
    }

    async fn save_fork(&self, fork: &ForkSnapshot) -> SessionResourceResult<()> {
        self.write_fork(fork).await
    }

    async fn save_child(&self, child: &ChildSnapshot) -> SessionResourceResult<()> {
        self.write_child(child).await
    }

    async fn load_child_resume_record(
        &self,
        child: &ThreadId,
    ) -> SessionResourceResult<ChildResumeRecord> {
        self.read_child_resume(child).await
    }

    async fn store_child_resume_record(
        &self,
        child: &ThreadId,
        record: &ChildResumeRecord,
    ) -> SessionResourceResult<()> {
        self.write_child_resume(child, record).await
    }

    async fn apply_compaction(
        &self,
        id: &ThreadId,
        change: &CompactionChange,
    ) -> SessionResourceResult<()> {
        self.write_compaction_change(id, change).await
    }

    async fn apply_message_projections(
        &self,
        id: &ThreadId,
        updates: &[(MessageId, MessageFlags)],
    ) -> SessionResourceResult<()> {
        self.write_projections(id, updates).await
    }

    async fn rewind_history(
        &self,
        id: &ThreadId,
        boundary: RewindBoundary,
    ) -> SessionResourceResult<()> {
        self.write_rewind(id, boundary).await
    }

    async fn remove_history_entries(
        &self,
        id: &ThreadId,
        ids: &[MessageId],
    ) -> SessionResourceResult<()> {
        self.write_history_removal(id, ids).await
    }

    async fn update_meta(
        &self,
        id: &ThreadId,
        patch: &SessionMetaPatch,
    ) -> SessionResourceResult<()> {
        self.write_meta(id, patch).await
    }

    async fn delete_tree(&self, id: &ThreadId) -> SessionResourceResult<()> {
        self.write_tree_deletion(id).await
    }

    /// 远端未决收敛：拿本机日志里的操作 id 向远端账本求证终态（C §5.1 第 4 步）。
    ///
    /// 每条未结清记录只有三种结论，且都由**写确认过的证据**给出，不靠空查询或超时推断：
    ///
    /// - 账本行已生效 → 结清 `applied`（原操作生效了，不是丢失）；
    /// - 账本行已封闭 → 结清 `never_applied`；
    /// - 账本行不存在 → 用**同一 id** 做终态封闭竞争：封闭先提交则原请求此后不可能再生效
    ///   （结清 `never_applied`）；竞争发现对方已提交则读回收据（结清 `applied`）；
    ///   封闭本身未确认则保持未结清。
    ///
    /// 账本行存在但不可解释时保持未结清：无法证明就不是收敛。全部结清后才看本机还有没有
    /// 别的未决事实，两者都为空才报告 `Recovered`。
    ///
    /// **封闭与提交互斥**由唯一键给出，不由时序给出：封闭写的是与资格写**同一个主键**，
    /// 本机**没有**远端操作的日志（v10 删除了 `session_remote_operations`，用户裁决不做
    /// 跨安装能力），因此没有「按本机记录里的原操作 id 逐条向远端求证终态」这条路径可走。
    ///
    /// 本方法只回答本机能回答的那部分：本机已没有可证明未结态的 durable 记录，会话数据
    /// 仍可读即可重载。**影响面**：进程崩溃前发出的远端请求若结果未知，本机无法再判定它
    /// 是否生效——这是被撤销的能力，不是遗漏；未结清只在活跃租约上表达，进程退出后
    /// 不由新的运行句柄宣称前次未知请求已结清。
    ///
    /// 若将来重新引入跨进程未决判定，移除条件是：本机重新持有「发送前登记、确定终态才
    /// 结清」的记录，并且远端账本的终态封闭仍按同一唯一键空间竞争。
    async fn recover_persistence(
        &self,
        id: &ThreadId,
    ) -> SessionResourceResult<PersistenceRecovery> {
        // 只读事实：会话数据不存在时不能宣告「已收敛、可重载」。
        self.load_meta(id).await?;
        Ok(PersistenceRecovery::Recovered)
    }

    /// 远端没有异步写入队列，本机也没有未结清记录可供等待，因此没有可排空的东西。
    ///
    /// 在途请求的等待由门面按活跃租约完成（`drain_persistence` 先等 `wait_for_in_flight`
    /// 并检查 `is_uncertain`），adapter 自己不做时序假设。
    async fn drain(&self, _id: &ThreadId) -> SessionResourceResult<()> {
        Ok(())
    }

    /// 关闭连接：把服务中的连接移进关闭句柄，再真正关闭；之后任何调用都明确失败。
    ///
    /// 幂等只适用于**确认关闭**：只有真实关闭成功返回过，后续调用才是幂等成功。关闭一开始
    /// （无论成功与否）连接就离开业务路径且不再重连；失败或取消都不丢句柄——重试关闭的是
    /// **同一条被保留的连接**，不新建连接，也不把「没有连接可用」当成「已经干净关闭」。
    ///
    /// 「确认」的范围是**本机传输面关闭成功**（见 [`RemoteStore::close`] 与 `remote` 模块
    /// 文档的 shutdown 定义）：它不证明服务端连接已释放，也不证明任何未知的远端写没有生效——
    /// v10 撤销本机操作日志后，本机已没有可以向远端账本求证的 durable 锚点，这条判定只剩下
    /// 活跃租约上的未结清标记；重开不构成对前次未知写入效果的证明。
    async fn close(&self) -> SessionResourceResult<()> {
        // 唯一的翻转点：取走服务中的连接（已经在关闭中时复用同一个句柄）。
        let closing = { self.slot.write().await.begin_close() };
        // 从来没有过连接（关闭态装配）：不谎报成功，也不建连接。
        let Some(closing) = closing else {
            return Err(connection_closed());
        };
        // 真实关闭：成功才确认（进度记在句柄里，不在这个 future 里）；失败如实上报，可重试。
        closing.shutdown().await
    }
}
