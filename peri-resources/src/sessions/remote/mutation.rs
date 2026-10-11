//! 远程可变连接：请求预算、访问模式拒绝、结果确定性与终态封闭（C §5.1/§7）。
//!
//! 本层是本 crate 里唯一发 mutation 的地方，形状由四条规矩定死：
//!
//! - **只读打开不写**：`StoreAccess::ReadOnly` 下任何写入在发请求前就拒绝。
//! - **一次 mutation = 一个托管事务批**：资格写入是第一条语句，效果随后，全部在
//!   同一 HTTP 请求的 `BEGIN IMMEDIATE`/`COMMIT` 里；连接已有打开事务时拒绝执行
//!   （SDK 会让批静默加入该事务，all-or-nothing 就不由我们掌控）。
//! - **确定性只有三种**：已生效、确定未生效、无法证明。回滚失败、超时、网络失败、
//!   写忙一律是「无法证明」，绝不折叠成「确定未生效」；未决由终态封闭收敛。
//! - **预算只约束本机等待**：超时或取消 future 都不代表远端没执行（C §5.1）。

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use peri_acp_types::session_resources::{
    AccessMode, SessionResourceError, SessionResourceErrorKind, SessionResourceResult,
};
use peri_acp_types::thread::ThreadId;
use turso_serverless::{Error as SdkError, Value};

use super::connection::{
    connect_sdk, within_budget, Budgeted, RemoteTransport, SdkTransport, REQUEST_BUDGET,
};
use super::credentials::SessionStoreCredential;
use super::endpoint::RemoteEndpoint;
use super::failure::{self, RemoteFailureClass};
use super::generation::ConnectionGate;
use super::ledger::{self, LedgerRow, OperationId, OperationIdentity, Receipt};
use super::schema::{self, StoreId, StoreIdentityOutcome, StoreIdentityRead};
use super::sql::StatementSpec;

/// 单次 mutation 的时间预算（与只读调用同一上界；精确值由 C-01/F 测量确定）。
pub(super) const MUTATION_BUDGET: Duration = REQUEST_BUDGET;

/// 打开意图：只读打开时本层不发任何写入。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StoreAccess {
    ReadWrite,
    ReadOnly,
}

impl StoreAccess {
    /// 本次打开的访问意图对应的存储访问模式（两处枚举含义相同，转换是纯函数）。
    pub(super) fn of(access: AccessMode) -> Self {
        match access {
            AccessMode::ReadWrite => Self::ReadWrite,
            AccessMode::ReadOnly => Self::ReadOnly,
        }
    }

    pub(super) fn ensure_writable(self) -> SessionResourceResult<()> {
        match self {
            Self::ReadWrite => Ok(()),
            Self::ReadOnly => Err(SessionResourceError::new(
                SessionResourceErrorKind::ReadOnlyStore,
            )),
        }
    }
}

/// 一次「资格先于效果」的 mutation 输入：身份 + 效果语句（效果可为空）。
pub(super) struct QualifiedMutation {
    pub(super) identity: OperationIdentity,
    pub(super) effects: Vec<StatementSpec>,
}

/// 一次 mutation 的结果：只表达确定性，不表达机制。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum MutationOutcome {
    /// 本次已提交（`replayed` 为真表示幂等命中已生效的原操作，返回其原收据）。
    Applied { receipt: Receipt, replayed: bool },
    /// 原操作已被封闭：确定从未生效且不可能再生效。
    ClosedNeverApplied,
    /// 确定未生效：远端在提交前拒绝，且托管事务批已回滚。
    NotApplied {
        class: RemoteFailureClass,
        /// 被拒绝的语句序号（0 是资格写）；仅诊断用，不含语句内容。
        rejected_statement: Option<usize>,
    },
    /// 无法证明终态：按未决处理，由终态封闭收敛。
    Unknown { class: RemoteFailureClass },
}

impl MutationOutcome {
    /// 领域失败映射；`Applied` 返回 `None`。未决一律映射为 `PersistenceUncertain`，
    /// 不允许降级成「确定未生效」。
    pub(super) fn failure_error(&self, thread: Option<&ThreadId>) -> Option<SessionResourceError> {
        match self {
            Self::Applied { .. } => None,
            Self::ClosedNeverApplied => Some(SessionResourceError::new(
                SessionResourceErrorKind::InvalidInput {
                    detail:
                        "remote operation was closed before it applied; it will not take effect"
                            .to_owned(),
                },
            )),
            Self::NotApplied { class, .. } => Some(class.into_session_resource_error()),
            Self::Unknown { .. } => {
                Some(SessionResourceError::persistence_uncertain(thread.cloned()))
            }
        }
    }
}

/// 终态封闭的结论（C §5.1 私有接口，不外泄）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum OperationResolution {
    /// 原操作已生效：返回原收据，不执行第二次。
    Applied { receipt: Receipt },
    /// 封闭胜出：原操作不可能再生效。
    ClosedNeverApplied,
    /// 封闭也未确认：保持阻塞，不清理、不放行。
    StillUnknown { class: RemoteFailureClass },
}

/// 托管事务批失败的分类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BatchFailure {
    /// 第一条（资格）语句主键冲突：该 operation 已被占用，需读回原终态。
    QualificationConflict,
    /// 确定未生效：SDK 托管批已确认整批回滚。
    NotApplied {
        class: RemoteFailureClass,
        index: Option<usize>,
    },
    /// 无法证明：回滚失败、超时、网络、写忙或未知变体。
    Unknown { class: RemoteFailureClass },
}

/// 批失败分类。**不**沿用只读路径的折叠规则：`BatchRollbackFailed` 内层即使是约束
/// 冲突，也说明回滚本身失败，只能判未决。
pub(super) fn classify_batch_failure(error: &SdkError) -> BatchFailure {
    match error {
        // 托管事务批：SDK 保证失败即回滚，除非被 BatchRollbackFailed 包住（下面按未决处理）。
        SdkError::BatchStatementFailed { index, error, .. } => {
            let class = failure::classify(error);
            if *index == 0 && class == RemoteFailureClass::Constraint {
                BatchFailure::QualificationConflict
            } else {
                BatchFailure::NotApplied {
                    class,
                    index: Some(*index),
                }
            }
        }
        // 回滚失败：连「零部分结果」都不成立，类别本身也无从确定。
        SdkError::BatchRollbackFailed { .. } => BatchFailure::Unknown {
            class: RemoteFailureClass::Unknown,
        },
        _ => {
            let class = failure::classify(error);
            if rejected_before_commit(error) {
                BatchFailure::NotApplied { class, index: None }
            } else {
                BatchFailure::Unknown { class }
            }
        }
    }
}

/// 初始化批的结果读法：本次初始化对身份做了什么。
///
/// 三种读法都由**本事务的结果**决定，与「打开前看到过什么」无关；真引擎 seam 与生产
/// 初始化共用这一处判定（见 `initialization_test.rs`）。`Failed` 不携带身份：结果未知时
/// 既没有可发的身份，也没有可发的事实。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum InitializationEvidence {
    /// 批已提交且元数据 INSERT 确切插入一行：本次建立身份（胜者）。
    Created,
    /// 本次没有建立身份：唯一键冲突（竞败／库已初始化），或批成功却没有插入证据。
    /// 结论到此为止，既有身份必须**读回**，不能由本次的铸造值冒充。
    Existing,
    /// 确定未生效或无法证明（含丢响应、超时、回滚失败）：不发创建事实，也不发身份。
    Failed(RemoteFailureClass),
}

/// 初始化批的结果 → [`InitializationEvidence`]（纯函数）。
pub(super) fn initialization_evidence(
    result: &Result<Vec<u64>, BatchFailure>,
) -> InitializationEvidence {
    match result {
        Ok(counts) if schema::inserted_meta_row(counts) => InitializationEvidence::Created,
        // 批成功却没有插入证据：不认领创建，按既有身份读回判定（读不回来即失败）。
        Ok(_) => InitializationEvidence::Existing,
        // 身份已被占用：元数据行主键冲突（建表在前，所以下标不是 0），读回胜者。
        Err(BatchFailure::QualificationConflict)
        | Err(BatchFailure::NotApplied {
            class: RemoteFailureClass::Constraint,
            index: Some(schema::META_INSERT_INDEX),
        }) => InitializationEvidence::Existing,
        Err(BatchFailure::NotApplied { class, .. }) | Err(BatchFailure::Unknown { class }) => {
            InitializationEvidence::Failed(*class)
        }
    }
}

/// 只读读回的结果 → 既有身份（纯函数）：认识不了、形状不可解释或仍为空都拒绝，绝不覆盖。
pub(super) fn existing_identity_from_read(
    read: StoreIdentityRead,
) -> SessionResourceResult<StoreId> {
    match read {
        StoreIdentityRead::Present(snapshot) if snapshot.matches_build() => Ok(snapshot.store_id),
        StoreIdentityRead::Present(_) => Err(SessionResourceError::new(
            SessionResourceErrorKind::Unsupported,
        )),
        StoreIdentityRead::Malformed | StoreIdentityRead::Uninitialized => {
            Err(unreadable_metadata())
        }
    }
}

/// 没有托管包装时，只有「提交前被拒绝」的类别才可判确定未生效。
fn rejected_before_commit(error: &SdkError) -> bool {
    matches!(
        error,
        SdkError::ToSqlConversionFailure(_)
            | SdkError::ConversionFailure(_)
            | SdkError::Misuse(_)
            | SdkError::Constraint(_)
            | SdkError::Readonly(_)
            | SdkError::QueryReturnedNoRows
    )
}

/// 故障注入（仅测试构建）：把「网络把结果吞掉」这类不可控故障变成可控的观察点。
///
/// 生产构建里没有这个类型，也没有它的字段——故障面只在测试里存在。
#[cfg(test)]
#[derive(Clone, Debug, Default)]
pub(super) struct FaultPlan {
    /// 命中 `kind` 的下一次批**照常提交**，但把结果按未决上报：服务端已提交、本机没收到
    /// 响应。恢复必须把它判成「已生效」。
    pub(super) drop_reply: Option<String>,
    /// 命中 `kind` 的下一次批**发出前**被丢弃：本机看到未决，远端什么也没发生。恢复必须
    /// 用同一唯一键把它封闭成「从未生效」。
    pub(super) drop_before_send: Option<String>,
}

/// 一条已确认的远程可变连接；不暴露底层 SDK 类型。
///
/// 一次在途调用被丢弃（超时、取消）之后，这条连接的状态无法证明：本类型把它记在
/// [`ConnectionGate`] 的**代际**上，由会话数据 adapter 在下次访问时重建（见
/// `RemoteSessionData::store`）。每一次传输调用都带一个代际守卫，守卫随被丢弃的 future
/// 同步落下失效事实——不依赖异步收尾。
pub(super) struct RemoteStore {
    transport: Arc<dyn RemoteTransport>,
    access: StoreAccess,
    /// 本连接的代际号（由 [`ConnectionGate`] 铸造）；失效与重建都按它记账。
    generation: u64,
    gate: Arc<ConnectionGate>,
    #[cfg(test)]
    faults: std::sync::Mutex<FaultPlan>,
}

impl RemoteStore {
    pub(super) fn access(&self) -> StoreAccess {
        self.access
    }

    /// 下发一段迁移批（`schema_upgrade` 的唯一出口）并**确认版本推进落地**。
    ///
    /// 收尾不变量：批次的末条语句是 `peri_store_meta` 的版本与契约推进，它必须影响 **1** 行。
    /// 影响 0 行说明批内守卫挡住了推进（库其实还是旧代数，例如快照读完之后身份被改写），
    /// 或者推进的 `WHERE` 没匹配上——两种都不能按成功上报，只能报不确定。索引重放一类
    /// `IF NOT EXISTS` 语句因此排在推进**之前**（它们的行数是 0，放末尾会把这条判定变成
    /// 「永远不成立」）。
    pub(super) async fn apply_schema_upgrade(
        &self,
        statements: Vec<StatementSpec>,
    ) -> SessionResourceResult<()> {
        self.access.ensure_writable()?;
        self.ensure_autocommit()?;
        let expected = statements.len();
        match self.run_managed_batch(statements).await {
            Ok(counts) if counts.len() == expected && counts.last() == Some(&1) => Ok(()),
            Ok(_) | Err(BatchFailure::Unknown { .. }) => Err(SessionResourceError::new(
                SessionResourceErrorKind::PersistenceUncertain { thread_id: None },
            )),
            Err(BatchFailure::QualificationConflict) => {
                Err(RemoteFailureClass::Constraint.into_session_resource_error())
            }
            Err(BatchFailure::NotApplied { class, .. }) => Err(class.into_session_resource_error()),
        }
    }

    /// 装配一条连接：传输面、访问意图、代际号与门禁都是既有事实。
    pub(super) fn new(
        transport: Arc<dyn RemoteTransport>,
        access: StoreAccess,
        generation: u64,
        gate: Arc<ConnectionGate>,
    ) -> Self {
        Self {
            transport,
            access,
            generation,
            gate,
            #[cfg(test)]
            faults: std::sync::Mutex::new(FaultPlan::default()),
        }
    }

    /// 直接开一条独立连接（机制实测用：自带门禁，不挂在会话数据 adapter 上）。
    ///
    /// 引擎/URL 已由 [`RemoteEndpoint`] 定死，凭证只在此处进入 SDK 调用。
    pub(super) async fn connect(
        endpoint: &RemoteEndpoint,
        credential: &SessionStoreCredential,
        access: StoreAccess,
    ) -> Result<Self, SessionResourceError> {
        let connection = connect_sdk(endpoint, credential).await?;
        let gate = Arc::new(ConnectionGate::default());
        Ok(Self::new(
            Arc::new(SdkTransport::new(connection)),
            access,
            gate.mint(),
            gate,
        ))
    }

    /// 本连接的代际号。
    pub(super) fn generation(&self) -> u64 {
        self.generation
    }

    /// 有预算、有代际守卫的一次传输调用。
    ///
    /// 守卫随 future 一起被丢弃时（调用方超时、外层任务取消）**同步**把本代际记为失效；
    /// 传输类失败同样如此——那时远端流的状态无法证明。远端明确的拒绝（约束、只读、缺行…）
    /// 是确定回答，本代际保留。
    async fn guarded<T>(
        &self,
        call: impl Future<Output = turso_serverless::Result<T>>,
    ) -> Budgeted<T> {
        let lease = self.gate.lease(self.generation);
        within_budget(
            async move {
                let mut lease = lease;
                let outcome = call.await;
                match &outcome {
                    Ok(_) => lease.release(),
                    Err(error) if !failure::invalidates_connection(error) => lease.release(),
                    // 连接层失败：这一代从此不可再用。
                    Err(_) => lease.invalidate(),
                }
                outcome
            },
            MUTATION_BUDGET,
        )
        .await
    }

    /// 不受代际守卫约束的传输调用（退场路径：连接本来就不再用）。
    async fn budgeted<T>(
        &self,
        call: impl Future<Output = turso_serverless::Result<T>>,
    ) -> Budgeted<T> {
        within_budget(call, MUTATION_BUDGET).await
    }

    /// 装载故障计划（仅测试构建）。
    #[cfg(test)]
    pub(super) fn inject_faults(&self, plan: FaultPlan) {
        if let Ok(mut faults) = self.faults.lock() {
            *faults = plan;
        }
    }

    /// 取出命中 `kind` 的一次性故障（仅测试构建）。
    #[cfg(test)]
    fn take_drop_reply(&self, kind: &str) -> bool {
        let Ok(mut faults) = self.faults.lock() else {
            return false;
        };
        if faults.drop_reply.as_deref() == Some(kind) {
            faults.drop_reply = None;
            true
        } else {
            false
        }
    }

    /// 同上，作用于「发出前丢弃」这一档（仅测试构建）。
    #[cfg(test)]
    fn take_drop_before_send(&self, kind: &str) -> bool {
        let Ok(mut faults) = self.faults.lock() else {
            return false;
        };
        if faults.drop_before_send.as_deref() == Some(kind) {
            faults.drop_before_send = None;
            true
        } else {
            false
        }
    }

    /// 只读身份读取：不建表、不写任何行。
    pub(super) async fn read_identity(&self) -> SessionResourceResult<StoreIdentityRead> {
        let plan = schema::identity_read_plan();
        let table_exists = self.fetch_row(&plan[0]).await?.is_some();
        let row = if table_exists {
            self.fetch_row(&plan[1]).await?
        } else {
            None
        };
        Ok(schema::interpret_identity_read(
            table_exists,
            row.as_deref(),
        ))
    }

    /// 显式初始化本任务 schema 并参与身份竞争，返回**竞争结论**而不是一个身份值。
    ///
    /// 竞争结论是结构化事实（[`StoreIdentityOutcome`]）：只有本事务确切插入元数据行并提交
    /// 才是 `Created`——批成功但没有插入证据、唯一键冲突后读回胜者、结果未知（超时、丢响应）
    /// 都不是创建、也都不发创建事实。已存在且形状/版本可解释时读回既有身份（不覆盖、不重造）；
    /// 不可解释时拒绝。调用方不得从「打开前读到空库」自行推定创建。
    pub(super) async fn initialize_store(&self) -> SessionResourceResult<StoreIdentityOutcome> {
        self.access.ensure_writable()?;
        self.ensure_autocommit()?;
        let store_id = StoreId::mint();
        let result = self
            .run_managed_batch(schema::initialization_plan(&store_id, &now_stamp()))
            .await;
        match initialization_evidence(&result) {
            InitializationEvidence::Created => Ok(StoreIdentityOutcome::Created(store_id)),
            // 没有建立身份（竞败／库已初始化／批成功却无插入证据）：既有身份必须读回。
            InitializationEvidence::Existing => Ok(StoreIdentityOutcome::Existing(
                self.read_existing_identity().await?,
            )),
            // 确定未生效或无法证明：不发身份，也不发创建事实。
            InitializationEvidence::Failed(class) => Err(class.into_session_resource_error()),
        }
    }

    /// 读回既有身份；不认识或读不出来时拒绝，绝不覆盖。
    async fn read_existing_identity(&self) -> SessionResourceResult<StoreId> {
        existing_identity_from_read(self.read_identity().await?)
    }

    /// 资格先于效果的一次原子 mutation。
    pub(super) async fn apply_qualified(
        &self,
        mutation: &QualifiedMutation,
    ) -> SessionResourceResult<MutationOutcome> {
        Ok(self.apply_qualified_reporting(mutation).await?.0)
    }

    /// 同上，并读回语句级证据：每条效果语句的受影响行数，与传入顺序对齐。
    ///
    /// 证据只在「本次确认提交」时存在——未提交的批没有可报告的写入数，因此失败分类下
    /// 一律给空证据，不拿上一次的结果或 0 冒充。
    pub(super) async fn apply_qualified_reporting(
        &self,
        mutation: &QualifiedMutation,
    ) -> SessionResourceResult<(MutationOutcome, Vec<u64>)> {
        self.access.ensure_writable()?;
        self.ensure_autocommit()?;
        #[cfg(test)]
        if self.take_drop_before_send(&mutation.identity.kind) {
            // 等价物：请求在发出之前消失（取消、发送前崩溃）。远端什么都没发生，
            // 但调用方无法据此断言——这正是「未知」。
            return Ok((
                MutationOutcome::Unknown {
                    class: RemoteFailureClass::Transport,
                },
                Vec::new(),
            ));
        }
        let now = now_stamp();
        let mut statements = Vec::with_capacity(mutation.effects.len() + 2);
        statements.push(ledger::qualify_statement(&mutation.identity, &now));
        statements.extend(mutation.effects.iter().cloned());
        Ok(match self.run_managed_batch(statements).await {
            // 第一条是资格写入，不进效果证据。
            Ok(counts) => {
                #[cfg(test)]
                if self.take_drop_reply(&mutation.identity.kind) {
                    // 等价物：批真的提交了，响应在回程丢失。本机只能看见「未知」，
                    // 效果与账本行都是真的——恢复必须判成「已生效」。
                    return Ok((
                        MutationOutcome::Unknown {
                            class: RemoteFailureClass::Transport,
                        },
                        Vec::new(),
                    ));
                }
                (
                    MutationOutcome::Applied {
                        receipt: mutation.identity.receipt.clone(),
                        replayed: false,
                    },
                    counts.into_iter().skip(1).collect(),
                )
            }
            Err(BatchFailure::QualificationConflict) => (
                self.resolve_after_conflict(&mutation.identity).await,
                Vec::new(),
            ),
            Err(BatchFailure::NotApplied { class, index }) => (
                MutationOutcome::NotApplied {
                    class,
                    rejected_statement: index,
                },
                Vec::new(),
            ),
            Err(BatchFailure::Unknown { class }) => {
                (MutationOutcome::Unknown { class }, Vec::new())
            }
        })
    }

    /// 终态封闭：证明原操作从未生效（或返回其原收据）。
    ///
    /// 封闭失败（被拒绝、超时、写忙、回滚失败）都**不**构成「确定未生效」：原操作的
    /// 命运仍然未知，只能保持阻塞。
    pub(super) async fn close_operation(
        &self,
        identity: &OperationIdentity,
    ) -> SessionResourceResult<OperationResolution> {
        self.access.ensure_writable()?;
        self.ensure_autocommit()?;
        let closure = ledger::closure_statement(identity, &now_stamp());
        Ok(match self.run_managed_batch(vec![closure]).await {
            Ok(_) => OperationResolution::ClosedNeverApplied,
            Err(BatchFailure::QualificationConflict) => {
                match self.read_ledger_row(&identity.operation_id).await {
                    Ok(LedgerRow::Applied { receipt, .. }) => {
                        OperationResolution::Applied { receipt }
                    }
                    Ok(LedgerRow::Closed) => OperationResolution::ClosedNeverApplied,
                    Ok(LedgerRow::Absent) | Ok(LedgerRow::Malformed) | Err(_) => {
                        OperationResolution::StillUnknown {
                            class: RemoteFailureClass::Unknown,
                        }
                    }
                }
            }
            Err(BatchFailure::NotApplied { class, .. }) | Err(BatchFailure::Unknown { class }) => {
                OperationResolution::StillUnknown { class }
            }
        })
    }

    /// 把这条连接的**父行检查**归位（`PRAGMA foreign_keys = OFF`）。
    ///
    /// 为什么远端要显式归位：canonical DDL 带 `REFERENCES`，而远端的数据面是在**没有强制**的
    /// 前提下自己保证引用顺序的——远端确实会写 `machines` / `workspaces` 行（升级批次与新建
    /// 会话都写），但不写 `projects` 行：那是本机 workspace 证据（目录与 Git 对象身份），远端
    /// 没有来源，归属行上的 `project_id` 只是随行搬来的历史证据值。父行检查一旦被打开，
    /// `workspaces.project_id` 这类指向缺席父行的写入会以约束失败收场；`DROP TABLE` 还会变成
    /// **隐式删除 + 级联**——重建 `threads` 时 `messages.thread_id`（`ON DELETE CASCADE`）
    /// 会连带清掉全部历史。
    ///
    /// 服务端的 `PRAGMA foreign_keys` 是**跨连接共享的可变状态**（实测默认为 0，但别的连接
    /// 可以打开它），所以每次写打开都归位一次，而不是假设它一直是 0。
    ///
    /// 归位之后的引用完整性由数据面保证：父行先写、删除时显式先清子行
    /// （[`crate::sessions::canonical::THREAD_CHILD_DELETES`]）；远端迁移另有一条结果守卫
    /// 核对重建之后历史行数没有变（`schema_upgrade::GUARD_MESSAGES_COUNT_SQL`），归位失效
    /// 不会变成一次静默丢历史。
    ///
    /// 只发一条**不在事务里**的语句（PRAGMA 在事务内不生效）；失败按原分类上报。
    pub(super) async fn force_parent_checks_off(&self) -> SessionResourceResult<()> {
        self.transport
            .sql_values(&StatementSpec::bare("PRAGMA foreign_keys = OFF"))
            .await
            .map_err(|error| failure::from_sdk_error(&error))?;
        Ok(())
    }

    /// 只读终态解析：不尝试写入，不推断。
    pub(super) async fn resolve_operation(
        &self,
        operation_id: &OperationId,
    ) -> SessionResourceResult<LedgerRow> {
        self.read_ledger_row(operation_id).await
    }

    /// 关闭连接：失败按分类上报，不静默吞掉。
    ///
    /// **借用**而不是消费：调用方（adapter 的关闭句柄）在确认关闭成功之前必须一直保留这条
    /// 连接本身，因此关闭失败或被取消之后可以在**同一条连接**上重试。不走代际守卫：
    /// 这条连接无论关闭成功与否都不再服务业务读写。
    pub(super) async fn close(&self) -> SessionResourceResult<()> {
        match self.budgeted(self.transport.close()).await {
            Budgeted::Done(()) => Ok(()),
            Budgeted::Failed(error) => Err(failure::from_sdk_error(&error)),
            Budgeted::Exceeded => Err(RemoteFailureClass::Timeout.into_session_resource_error()),
        }
    }

    /// 资格被占用后读回原终态；读不回来就是未决，不猜。
    ///
    /// 摘要必须与本次身份一致：同一个 id 却挂着不同的输入摘要，说明有人在复用 id 换了
    /// 内容，这不是重放。此时不得返回原收据（那等于把别人的效果认成自己的），也不得
    /// 声明未决（我们的批确实被拒绝了）：本次提交确定未生效，按约束拒绝上报。
    async fn resolve_after_conflict(&self, identity: &OperationIdentity) -> MutationOutcome {
        match self.read_ledger_row(&identity.operation_id).await {
            Ok(LedgerRow::Applied { receipt, digest }) if digest == identity.digest => {
                MutationOutcome::Applied {
                    receipt,
                    replayed: true,
                }
            }
            Ok(LedgerRow::Applied { .. }) => MutationOutcome::NotApplied {
                class: RemoteFailureClass::Constraint,
                rejected_statement: Some(0),
            },
            Ok(LedgerRow::Closed) => MutationOutcome::ClosedNeverApplied,
            Ok(LedgerRow::Absent) | Ok(LedgerRow::Malformed) | Err(_) => MutationOutcome::Unknown {
                class: RemoteFailureClass::Unknown,
            },
        }
    }

    async fn read_ledger_row(
        &self,
        operation_id: &OperationId,
    ) -> SessionResourceResult<LedgerRow> {
        Ok(
            match self
                .fetch_row(&ledger::resolve_statement(operation_id))
                .await?
            {
                Some(values) => ledger::decode_row(&values),
                None => LedgerRow::Absent,
            },
        )
    }

    /// 幂等 schema 批（建表/建索引）：一个托管事务批，失败整批回滚。
    ///
    /// 与业务 mutation 的区别是**没有账本资格写**：DDL 没有「同一操作重试」的语义，
    /// 全部语句都是 `IF NOT EXISTS`，重复执行不改变结果。因此它也不产生收据。
    /// 失败一律按原分类上报：DDL 未提交即未生效，不做未决判定（没有业务效果可证明）。
    pub(super) async fn apply_schema(
        &self,
        statements: Vec<StatementSpec>,
    ) -> SessionResourceResult<()> {
        self.access.ensure_writable()?;
        self.ensure_autocommit()?;
        match self.run_managed_batch(statements).await {
            Ok(_) => Ok(()),
            // 批内没有资格写，主键冲突只可能来自并发建同名对象；如实按约束失败上报。
            Err(BatchFailure::QualificationConflict) => {
                Err(RemoteFailureClass::Constraint.into_session_resource_error())
            }
            Err(BatchFailure::NotApplied { class, .. }) | Err(BatchFailure::Unknown { class }) => {
                Err(class.into_session_resource_error())
            }
        }
    }

    /// 只读一致读：同一请求内的 `BEGIN DEFERRED` … `COMMIT`，多段读取落在同一快照上。
    ///
    /// 只读打开（[`StoreAccess::ReadOnly`]）也可用：批内只有 SELECT，不申请写锁。
    /// 读没有副作用，因此不套用 mutation 的确定性三分类——失败只表示「这次没读到」。
    pub(super) async fn read_batch(
        &self,
        statements: Vec<StatementSpec>,
    ) -> SessionResourceResult<Vec<Vec<Vec<Value>>>> {
        self.ensure_autocommit()?;
        let expected = statements.len();
        match self
            .guarded(self.transport.consistent_read(statements))
            .await
        {
            // 回复少了结果集时，调用方绝不能把它读成「这段查询没有数据」，所以在唯一的
            // 读取出口处校验一次形状，三条语句与四条语句的读取都走这条检查。
            Budgeted::Done(results) => {
                ensure_result_sets(expected, results.len())?;
                Ok(results)
            }
            Budgeted::Failed(error) => Err(read_batch_error(&error)),
            Budgeted::Exceeded => Err(RemoteFailureClass::Timeout.into_session_resource_error()),
        }
    }

    /// 恰好两段结果的一致读取（例如「会话事实行 + 自有 payload 行集」）。
    ///
    /// [`Self::read_batch`] 已经校验过结果集数量；这里仍然显式取两次而不是用默认值兜底：
    /// 拿不到就不返回快照，避免把不完整的回复降级成「没有历史」。
    pub(super) async fn read_pair(
        &self,
        facts: StatementSpec,
        rows: StatementSpec,
    ) -> SessionResourceResult<(Vec<Vec<Value>>, Vec<Vec<Value>>)> {
        let mut batches = self.read_batch(vec![facts, rows]).await?.into_iter();
        let facts = batches
            .next()
            .ok_or_else(|| incomplete_reply("expected 2 result sets, got 1"))?;
        let rows = batches
            .next()
            .ok_or_else(|| incomplete_reply("expected 2 result sets, got 1"))?;
        Ok((facts, rows))
    }

    /// 一个托管事务批：`BEGIN IMMEDIATE` … `COMMIT`，失败整批回滚。
    ///
    /// 返回每条语句的受影响行数（与传入顺序对齐），供上层核对「恰好一行」这类后置条件。
    /// 语句构造失败与远端拒绝走同一分类（都是 `Failed`），不因为失败发生在本地就降级成
    /// 「确定未生效」。
    async fn run_managed_batch(
        &self,
        statements: Vec<StatementSpec>,
    ) -> Result<Vec<u64>, BatchFailure> {
        match self.guarded(self.transport.managed_batch(statements)).await {
            Budgeted::Done(counts) => Ok(counts),
            Budgeted::Failed(error) => Err(classify_batch_failure(&error)),
            Budgeted::Exceeded => Err(BatchFailure::Unknown {
                class: RemoteFailureClass::Timeout,
            }),
        }
    }

    /// 只读单行：只取第一行，行内所有列原样取出（调用方决定怎么解释）。
    pub(super) async fn fetch_row(
        &self,
        spec: &StatementSpec,
    ) -> SessionResourceResult<Option<Vec<Value>>> {
        Ok(self.rows(spec).await?.into_iter().next())
    }

    /// 只读多行：按语句的 `ORDER BY` 顺序返回。
    pub(super) async fn fetch_rows(
        &self,
        spec: &StatementSpec,
    ) -> SessionResourceResult<Vec<Vec<Value>>> {
        self.rows(spec).await
    }

    /// 单条语句的全部行（值矩阵在传输边界已经解码）。
    async fn rows(&self, spec: &StatementSpec) -> SessionResourceResult<Vec<Vec<Value>>> {
        match self.guarded(self.transport.sql_values(spec)).await {
            Budgeted::Done(rows) => Ok(rows),
            Budgeted::Failed(error) => Err(failure::from_sdk_error(&error)),
            Budgeted::Exceeded => Err(RemoteFailureClass::Timeout.into_session_resource_error()),
        }
    }

    /// 托管事务批在「连接已有打开事务」时会静默加入该事务（SDK `run_batch` 的 wrap 判定），
    /// 那时 all-or-nothing 不由我们掌控，因此显式拒绝而不是顺手执行。
    fn ensure_autocommit(&self) -> SessionResourceResult<()> {
        match self.transport.is_autocommit() {
            Ok(true) => Ok(()),
            Ok(false) => Err(internal(
                "remote mutation refused: connection has an open transaction",
            )),
            Err(_) => Err(internal(
                "remote mutation refused: transaction state is unreadable",
            )),
        }
    }
}

/// 时间戳只作诊断（记录空间与封闭时间），不作任何判据。
fn now_stamp() -> String {
    peri_time::now_utc_rfc3339()
}

/// 只读批失败 → 领域失败：直接按分类上报（读没有副作用，不存在「不确定生效」）。
fn read_batch_error(error: &SdkError) -> SessionResourceError {
    match classify_batch_failure(error) {
        BatchFailure::NotApplied { class, .. } | BatchFailure::Unknown { class } => {
            class.into_session_resource_error()
        }
        BatchFailure::QualificationConflict => {
            RemoteFailureClass::Constraint.into_session_resource_error()
        }
    }
}

/// 回复形状不符：结果集少了、或单行查询多给了行。
///
/// 分类是 `Internal`，理由是这三个「不是」：不是存储数据损坏（`Corrupt` 说的是记录本身读不
/// 出来）、不是没有这一行（用 `NotFound` 会把不完整回复伪装成缺失、把「读不到」变成
/// 「不存在」）、也不是能力缺失（`Unsupported`）。`detail` 只写条数，不带会话内容或绑定值。
pub(super) fn incomplete_reply(detail: &str) -> SessionResourceError {
    internal(&format!("read reply is incomplete: {detail}"))
}

/// 请求了几条语句就必须拿到几段结果。
pub(super) fn ensure_result_sets(expected: usize, actual: usize) -> SessionResourceResult<()> {
    if expected == actual {
        Ok(())
    } else {
        Err(incomplete_reply(&format!(
            "expected {expected} result set(s), got {actual}"
        )))
    }
}

/// 至多一行（主键查询）的结果：多给一行同样是无法解释的回复，不静默取其中一行。
pub(super) fn sole_row(mut rows: Vec<Vec<Value>>) -> SessionResourceResult<Option<Vec<Value>>> {
    match rows.len() {
        0 => Ok(None),
        1 => Ok(rows.pop()),
        actual => Err(incomplete_reply(&format!(
            "single-row read returned {actual} rows"
        ))),
    }
}

fn internal(detail: &str) -> SessionResourceError {
    SessionResourceError::new(SessionResourceErrorKind::Internal {
        detail: detail.to_owned(),
    })
}

fn unreadable_metadata() -> SessionResourceError {
    SessionResourceError::new(SessionResourceErrorKind::Corrupt {
        detail: "remote session store metadata is not interpretable".to_owned(),
    })
}
