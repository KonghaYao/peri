//! `RequestLedger`：`peri/mcp/apps/*` 请求的**去重键与序号权威**（设计 §1.3、§8）。
//!
//! 职责边界：
//! - **分配权威**是 facade（namespace 由 router 分配 / Host namespace 惰性分配），
//!   **推进权威**是本模块：每 namespace 持 `last_accepted`，只有
//!   `seq == last_accepted + 1` 才登记，其余按下表判决；
//! - 同 key（namespace + seq）只登记一次，指纹相同 ⇒ 加入同一等待或返回同一结果；
//!   指纹不同 ⇒ `Conflict`；结果已回收 ⇒ `ResultExpired`（**绝不**当作新执行）；
//! - 销毁的 namespace **不得以同 ID 重建**：`destroy` 之后同 ID 请求一律
//!   `UnknownNamespace`（A16 的同进程部分）。
//!
//! 全部结构有界：容量数字的唯一来源是
//! [`peri_acp_types::mcp_apps_facade`] 的 §4.3 常量（本文件不复写任何字面量上限），
//! 测试经 [`LedgerBudget::for_tests`] 用小预算触发边界。
//!
//! 事实边界（裁决 D7）：本模块只为「**从未成为 invocation**」的请求产出
//! `NotExecuted`（准入前放弃）；已 dispatch 的调用保持 service 的 outcome，
//! 由调用方（facade）按 service 返回值记账，本模块不改写。

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::{Mutex, RwLock};
use peri_acp_types::mcp_apps_facade::{
    AppsRequestOutcome, CancelVerdict, RequestNamespaceId, RequestSeq, MAX_NAMESPACES_PER_SESSION,
    MAX_PENDING_PER_NAMESPACE, MAX_RESULT_BYTES, MAX_RETAINED_BYTES, MAX_RETAINED_RESULTS,
    MCP_APPS_FACADE_VERSION,
};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// 销毁痕迹表容量（有界 tombstone）：与 namespace 上限同阶。
///
/// 超出窗口的旧 ID 在结构上无法被重建——新 namespace 一律经
/// [`RequestNamespaceId::new`] 铸造（UUID v7，永不复用），重挂接复用 router 表中
/// 的既有 namespace，`destroy` 是本模块唯一的删除入口。
const DESTROYED_TOMBSTONES: usize = MAX_NAMESPACES_PER_SESSION as usize;

/// namespace 类别（只用于诊断与准入面：Attachment 与 Host 共享同一套序号规则）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NamespaceKind {
    /// 一个 View 附件的请求域（`apps/attach` 分配）。
    Attachment,
    /// 一条可信连接的 Host 操作域（`apps/list` / `apps/attach` 下发）。
    Host,
}

/// 打开 / 复用 namespace 的失败（调用方必须显式拒绝，不得回落到新 namespace）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NamespaceOpenError {
    /// 已销毁：同 ID **不得**重建（A16）。
    Destroyed,
    /// 该 ID 已存在但类别不同（身份复用）：显式拒绝。
    KindMismatch,
    /// namespace 数已达上限（`MAX_NAMESPACES_PER_SESSION`）。
    CapacityExceeded,
}

/// 预算：全部取自 §4.3 常量；测试用 [`Self::for_tests`] 的小预算触发边界。
#[derive(Debug, Clone, Copy)]
pub(crate) struct LedgerBudget {
    pub(crate) max_namespaces: usize,
    pub(crate) max_pending_per_namespace: usize,
    pub(crate) max_retained_results: usize,
    pub(crate) max_result_bytes: usize,
    pub(crate) max_retained_bytes: usize,
}

impl LedgerBudget {
    /// 生产预算（4a 保守值：**4b 依实测调参**，调参只改契约常量）。
    ///
    /// 字节预算允许 0：`for_tests` 用它构造「单结果即超限」的边界。
    pub(crate) fn conservative() -> Self {
        Self {
            max_namespaces: MAX_NAMESPACES_PER_SESSION as usize,
            max_pending_per_namespace: MAX_PENDING_PER_NAMESPACE as usize,
            max_retained_results: MAX_RETAINED_RESULTS as usize,
            max_result_bytes: MAX_RESULT_BYTES as usize,
            max_retained_bytes: MAX_RETAINED_BYTES as usize,
        }
    }

    /// 测试预算：小上限触发容量 / 回收边界（不得把「1024 条目」当有界，§9.2）。
    #[cfg(test)]
    pub(crate) fn for_tests(
        max_namespaces: usize,
        max_pending: usize,
        max_results: usize,
        max_result_bytes: usize,
        max_retained_bytes: usize,
    ) -> Self {
        Self {
            max_namespaces,
            max_pending_per_namespace: max_pending,
            max_retained_results: max_results,
            max_result_bytes,
            max_retained_bytes,
        }
    }
}

/// 请求指纹的**输入**：影响执行的参数、目标与允许的 metadata 投影。
///
/// 排除项（不得进入指纹）：transport ID、等待时长、`connectionEpoch`
/// （设计 §8：这些不改变业务身份）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RequestOperation {
    /// App 回调：record 身份 + App 面工具名 + 参数。
    AppToolsCall {
        record_id: String,
        tool_name: String,
        arguments: Map<String, Value>,
    },
    /// Host 调用：server + wire 工具名 + 参数（`apps/rerun` 的目标来自服务端记录）。
    HostToolsCall {
        server_id: String,
        tool_name: String,
        arguments: Map<String, Value>,
    },
    /// 资源读取：record 身份 + 目标 URI。
    ResourcesRead { record_id: String, uri: String },
    /// 资源列表：record 身份 + 游标。
    ResourcesList {
        record_id: String,
        cursor: Option<String>,
    },
}

impl RequestOperation {
    fn kind(&self) -> &'static str {
        match self {
            Self::AppToolsCall { .. } => "app/tools/call",
            Self::HostToolsCall { .. } => "host/tools/call",
            Self::ResourcesRead { .. } => "resources/read",
            Self::ResourcesList { .. } => "resources/list",
        }
    }

    /// 规范化 JSON：对象键排序（嵌套同样排序）、`None` 不落键。
    fn canonical(&self) -> Value {
        let mut object = Map::new();
        object.insert("kind".into(), Value::String(self.kind().into()));
        match self {
            Self::AppToolsCall {
                record_id,
                tool_name,
                arguments,
            } => {
                object.insert("recordId".into(), Value::String(record_id.clone()));
                object.insert("toolName".into(), Value::String(tool_name.clone()));
                object.insert(
                    "arguments".into(),
                    canonicalize(&Value::Object(arguments.clone())),
                );
            }
            Self::HostToolsCall {
                server_id,
                tool_name,
                arguments,
            } => {
                object.insert("serverId".into(), Value::String(server_id.clone()));
                object.insert("toolName".into(), Value::String(tool_name.clone()));
                object.insert(
                    "arguments".into(),
                    canonicalize(&Value::Object(arguments.clone())),
                );
            }
            Self::ResourcesRead { record_id, uri } => {
                object.insert("recordId".into(), Value::String(record_id.clone()));
                object.insert("uri".into(), Value::String(uri.clone()));
            }
            Self::ResourcesList { record_id, cursor } => {
                object.insert("recordId".into(), Value::String(record_id.clone()));
                if let Some(cursor) = cursor {
                    object.insert("cursor".into(), Value::String(cursor.clone()));
                }
            }
        }
        Value::Object(object)
    }
}

/// 键排序的规范化 JSON（`preserve_order` 打开时也成立：插入序即排序序）。
fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut sorted: Vec<(&String, &Value)> = map.iter().collect();
            sorted.sort_by(|left, right| left.0.cmp(right.0));
            let mut object = Map::new();
            for (key, entry) in sorted {
                object.insert(key.clone(), canonicalize(entry));
            }
            Value::Object(object)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonicalize).collect()),
        other => other.clone(),
    }
}

/// 请求指纹（sha2 over 规范化 JSON）。
///
/// 覆盖：facade 版本 + 会话身份 + 操作类别 + 目标身份 + 参数（键排序）。
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct RequestFingerprint([u8; 32]);

impl RequestFingerprint {
    pub(crate) fn new(session_id: &str, operation: &RequestOperation) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(MCP_APPS_FACADE_VERSION.as_bytes());
        hasher.update([0]);
        hasher.update(session_id.as_bytes());
        hasher.update([0]);
        hasher
            .update(serde_json::to_vec(&operation.canonical()).unwrap_or_else(|_| b"{}".to_vec()));
        Self(hasher.finalize().into())
    }
}

impl std::fmt::Debug for RequestFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// 准入前放弃的原因（记在 pending 上，不新增 `NotExecutedCause` 变体，裁决 D7）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RequestCancelReason {
    /// 附件被替换 / 重挂接 / 终态关闭：该请求从未成为 invocation。
    AttachmentReplaced,
    /// 客户端 `apps/cancel`（只命中该次操作）。
    ClientCancelled,
}

/// 一次已登记但尚未终结的请求。
pub(crate) struct PendingRequest {
    seq: RequestSeq,
    fingerprint: RequestFingerprint,
    /// 准入前的放弃信号：接线到 `SessionInvocationRequest.cancel`，
    /// **不得**成为已提交调用的取消源（设计 §1.3）。
    pub(crate) cancel: CancellationToken,
    cancel_reason: Mutex<Option<RequestCancelReason>>,
    /// 同 key 并发请求共用同一等待槽（只执行一次，取同一结果）。
    pub(crate) slot: Arc<OutcomeSlot>,
}

impl std::fmt::Debug for PendingRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingRequest")
            .field("seq", &self.seq)
            .field("fingerprint", &self.fingerprint)
            .field("cancel_reason", &self.cancel_reason())
            .field("cancelled", &self.cancel.is_cancelled())
            .finish()
    }
}

impl PendingRequest {
    fn new(seq: RequestSeq, fingerprint: RequestFingerprint) -> Self {
        Self {
            seq,
            fingerprint,
            cancel: CancellationToken::new(),
            cancel_reason: Mutex::new(None),
            slot: Arc::new(OutcomeSlot::new()),
        }
    }

    /// 本请求被放弃的原因（`None` = 未被取消）。
    pub(crate) fn cancel_reason(&self) -> Option<RequestCancelReason> {
        *self.cancel_reason.lock()
    }

    /// 登记放弃原因并发出准入前放弃信号（幂等；首个原因胜出）。
    pub(crate) fn abandon(&self, reason: RequestCancelReason) {
        let mut slot = self.cancel_reason.lock();
        if slot.is_none() {
            *slot = Some(reason);
            self.cancel.cancel();
        }
    }
}

/// 一次请求的结果槽：终态只写一次；同 key 的并发等待者共享同一槽。
pub(crate) struct OutcomeSlot {
    outcome: Mutex<Option<AppsRequestOutcome>>,
    notify: Notify,
}

impl OutcomeSlot {
    fn new() -> Self {
        Self {
            outcome: Mutex::new(None),
            notify: Notify::new(),
        }
    }

    /// 发布终态（幂等：首个终态胜出，不覆盖）。
    pub(crate) fn complete(&self, outcome: AppsRequestOutcome) {
        {
            let mut slot = self.outcome.lock();
            if slot.is_some() {
                return;
            }
            *slot = Some(outcome);
        }
        self.notify.notify_waiters();
    }

    /// 当前终态（未终结 ⇒ `None`）。
    pub(crate) fn get(&self) -> Option<AppsRequestOutcome> {
        self.outcome.lock().clone()
    }

    /// 有界等待终态：超时返回当前值（可能仍是 `None`，调用方按 `Pending` 呈现）。
    pub(crate) async fn wait(&self, timeout: Duration) -> Option<AppsRequestOutcome> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Some(outcome) = self.get() {
                return Some(outcome);
            }
            let notified = self.notify.notified();
            // 在创建等待 future 之前置值不会丢通知：这里再查一次。
            if let Some(outcome) = self.get() {
                return Some(outcome);
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return self.get();
            }
        }
    }
}

/// 已终结的结果条目（`outcome: None` = 超过单条字节上限，显式不可用但**保留防重事实**）。
struct CompletedEntry {
    seq: RequestSeq,
    fingerprint: RequestFingerprint,
    outcome: Option<Arc<AppsRequestOutcome>>,
    bytes: usize,
}

struct NamespaceState {
    kind: NamespaceKind,
    last_accepted: RequestSeq,
    pending: BTreeMap<RequestSeq, Arc<PendingRequest>>,
    complete: VecDeque<CompletedEntry>,
    complete_bytes: usize,
}

impl NamespaceState {
    fn new(kind: NamespaceKind) -> Self {
        Self {
            kind,
            last_accepted: RequestSeq::new(0),
            pending: BTreeMap::new(),
            complete: VecDeque::new(),
            complete_bytes: 0,
        }
    }
}

#[derive(Default)]
struct LedgerState {
    namespaces: HashMap<RequestNamespaceId, NamespaceState>,
    /// 有界 tombstone（FIFO）：销毁过的 ID 不得重建。
    destroyed: HashSet<RequestNamespaceId>,
    destroyed_order: VecDeque<RequestNamespaceId>,
    retained_bytes: usize,
}

/// 准入判决（设计 §8 的六种情况 + namespace 未知）。
#[derive(Debug)]
pub(crate) enum AdmissionVerdict {
    /// 新请求已原子登记（指纹已存），调用方负责执行并 `complete`。
    Accepted { pending: Arc<PendingRequest> },
    /// 同 key 已在执行：加入同一等待（不重复审批 / 执行）。
    JoinedPending { pending: Arc<PendingRequest> },
    /// 同 key 已有终态结果：返回同一结果。
    Duplicate { outcome: AppsRequestOutcome },
    /// 同 key 但指纹不同：拒绝，不执行。
    Conflict,
    /// 同 key 的结果已被回收：即使无法再比较指纹也**绝不**当作新执行。
    ResultExpired,
    /// 序号跳跃：要求客户端改用 `expected`。
    SequenceGap { expected: RequestSeq },
    /// 容量不足且尚未登记：**不消耗序号**。
    CapacityRejected,
    /// namespace 不存在 / 已销毁 / 类别不符：不得当作新执行，也不得重建。
    UnknownNamespace,
}

/// session 级请求账本（`SessionAppsFacade` 独占）。
pub(crate) struct RequestLedger {
    budget: LedgerBudget,
    state: RwLock<LedgerState>,
}

impl RequestLedger {
    pub(crate) fn new(budget: LedgerBudget) -> Self {
        Self {
            budget,
            state: RwLock::new(LedgerState::default()),
        }
    }

    /// 打开（首次）或复用（重挂接）一个 namespace。
    ///
    /// 幂等：同一 ID 同类别重复调用是复用（重挂接不重建 namespace）；
    /// 销毁过的 ID 一律拒绝（A16），类别不符一律拒绝。
    pub(crate) fn ensure_namespace(
        &self,
        namespace: &RequestNamespaceId,
        kind: NamespaceKind,
    ) -> Result<(), NamespaceOpenError> {
        let mut state = self.state.write();
        if state.destroyed.contains(namespace) {
            return Err(NamespaceOpenError::Destroyed);
        }
        if let Some(existing) = state.namespaces.get(namespace) {
            return if existing.kind == kind {
                Ok(())
            } else {
                Err(NamespaceOpenError::KindMismatch)
            };
        }
        if state.namespaces.len() >= self.budget.max_namespaces {
            return Err(NamespaceOpenError::CapacityExceeded);
        }
        state
            .namespaces
            .insert(namespace.clone(), NamespaceState::new(kind));
        Ok(())
    }

    /// 销毁 namespace：删除全部请求状态并留下有界 tombstone（不得以同 ID 重建）。
    ///
    /// 未决请求被登记为 [RequestCancelReason::AttachmentReplaced] 并发出放弃信号
    /// （幂等；已终结的请求不受影响）。
    pub(crate) fn destroy(&self, namespace: &RequestNamespaceId) {
        let pending = {
            let mut state = self.state.write();
            let Some(existing) = state.namespaces.remove(namespace) else {
                return;
            };
            if !state.destroyed.insert(namespace.clone()) {
                return;
            }
            state.destroyed_order.push_back(namespace.clone());
            while state.destroyed_order.len() > DESTROYED_TOMBSTONES {
                if let Some(evicted) = state.destroyed_order.pop_front() {
                    state.destroyed.remove(&evicted);
                }
            }
            existing
                .pending
                .into_values()
                .collect::<Vec<Arc<PendingRequest>>>()
        };
        for request in pending {
            request.abandon(RequestCancelReason::AttachmentReplaced);
        }
    }

    /// 下一个可用序号（`last_accepted + 1`）。未登记 namespace 返回 1（起点）。
    pub(crate) fn next_seq(&self, namespace: &RequestNamespaceId) -> RequestSeq {
        self.state
            .read()
            .namespaces
            .get(namespace)
            .map(|state| state.last_accepted.next())
            .unwrap_or_else(|| RequestSeq::new(1))
    }

    /// 遗留 wire 别名的**归属序号**：推进 `last_accepted` 并返回本次序号。
    ///
    /// 旧信封没有可重传的业务身份（无 seq / 无附件身份），因此不进入去重表、不产生
    /// 结果槽——它既不能与服务端记录比较指纹，也没有「同 key」概念。归属值仍由本
    /// 账本推进（单调、不复用），随旧别名一并删除（S8）。
    pub(crate) fn issue_legacy_seq(&self, namespace: &RequestNamespaceId) -> RequestSeq {
        let mut state = self.state.write();
        let Some(namespace_state) = state.namespaces.get_mut(namespace) else {
            return RequestSeq::new(0);
        };
        let seq = namespace_state.last_accepted.next();
        namespace_state.last_accepted = seq;
        seq
    }

    /// 准入判决（设计 §8 表逐条）。
    pub(crate) fn admit(
        &self,
        namespace: &RequestNamespaceId,
        seq: RequestSeq,
        fingerprint: &RequestFingerprint,
    ) -> AdmissionVerdict {
        let mut state = self.state.write();
        let Some(namespace_state) = state.namespaces.get_mut(namespace) else {
            return AdmissionVerdict::UnknownNamespace;
        };
        let expected = namespace_state.last_accepted.next();
        if seq.get() < 1 {
            // 序号从 1 起：0 不是「已用旧序号」也不是新请求。
            return AdmissionVerdict::SequenceGap { expected };
        }
        if seq <= namespace_state.last_accepted {
            if let Some(pending) = namespace_state.pending.get(&seq) {
                return if &pending.fingerprint == fingerprint {
                    AdmissionVerdict::JoinedPending {
                        pending: Arc::clone(pending),
                    }
                } else {
                    AdmissionVerdict::Conflict
                };
            }
            if let Some(entry) = namespace_state
                .complete
                .iter()
                .find(|entry| entry.seq == seq)
            {
                if &entry.fingerprint != fingerprint {
                    return AdmissionVerdict::Conflict;
                }
                return match entry.outcome.as_ref() {
                    Some(outcome) => AdmissionVerdict::Duplicate {
                        outcome: outcome.as_ref().clone(),
                    },
                    // 结果超过单条字节上限或已被回收：显式不可用，绝不重执行。
                    None => AdmissionVerdict::ResultExpired,
                };
            }
            // 已登记过但结果已回收：无法再比较指纹，也绝不当作新执行。
            return AdmissionVerdict::ResultExpired;
        }
        if seq > expected {
            return AdmissionVerdict::SequenceGap { expected };
        }
        // seq == expected：容量判定在登记之前（不足即不消耗序号）。
        if namespace_state.pending.len() >= self.budget.max_pending_per_namespace {
            return AdmissionVerdict::CapacityRejected;
        }
        let pending = Arc::new(PendingRequest::new(seq, fingerprint.clone()));
        namespace_state.pending.insert(seq, Arc::clone(&pending));
        namespace_state.last_accepted = seq;
        AdmissionVerdict::Accepted { pending }
    }

    /// 登记终态：发布到结果槽（同 key 等待者共享）并按条数 / 字节回收。
    ///
    /// 返回 `false` = 该请求不在此账本内（namespace 已销毁 / 未登记 / 已终结）：
    /// 调用方仍需把 outcome 发布到它持有的 pending 槽（等待者据此解除等待）。
    pub(crate) fn complete(
        &self,
        namespace: &RequestNamespaceId,
        seq: RequestSeq,
        outcome: AppsRequestOutcome,
    ) -> bool {
        let mut state = self.state.write();
        let Some(namespace_state) = state.namespaces.get_mut(namespace) else {
            return false;
        };
        let Some(pending) = namespace_state.pending.remove(&seq) else {
            return false;
        };
        let bytes = serde_json::to_vec(&outcome)
            .map(|bytes| bytes.len())
            .unwrap_or(0);
        let retained = (bytes <= self.budget.max_result_bytes).then(|| Arc::new(outcome.clone()));
        let retained_bytes = if retained.is_some() { bytes } else { 0 };
        namespace_state.complete.push_back(CompletedEntry {
            seq,
            fingerprint: pending.fingerprint.clone(),
            outcome: retained,
            bytes: retained_bytes,
        });
        namespace_state.complete_bytes += retained_bytes;
        // 回收：条数 + 字节双预算（只淘汰最旧的终态条目；pending 不淘汰但有上限）。
        let mut reclaimed = 0usize;
        while namespace_state.complete.len() > self.budget.max_retained_results
            || namespace_state.complete_bytes > self.budget.max_retained_bytes
        {
            let Some(evicted) = namespace_state.complete.pop_front() else {
                break;
            };
            namespace_state.complete_bytes =
                namespace_state.complete_bytes.saturating_sub(evicted.bytes);
            reclaimed += evicted.bytes;
        }
        if retained_bytes > 0 {
            state.retained_bytes += retained_bytes;
        }
        state.retained_bytes = state.retained_bytes.saturating_sub(reclaimed);
        pending.slot.complete(outcome);
        true
    }

    /// 未决请求的结果槽（终态请求返回 `None`：结果经 `admit` 的判决返回）。
    pub(crate) fn slot(
        &self,
        namespace: &RequestNamespaceId,
        seq: RequestSeq,
    ) -> Option<Arc<OutcomeSlot>> {
        self.state
            .read()
            .namespaces
            .get(namespace)
            .and_then(|state| state.pending.get(&seq))
            .map(|pending| Arc::clone(&pending.slot))
    }

    /// 取消一次未决请求（`apps/cancel`：只命中该次操作）。
    pub(crate) fn cancel(
        &self,
        namespace: &RequestNamespaceId,
        seq: RequestSeq,
        reason: RequestCancelReason,
    ) -> CancelVerdict {
        let state = self.state.read();
        let Some(namespace_state) = state.namespaces.get(namespace) else {
            return CancelVerdict::Unknown;
        };
        if let Some(pending) = namespace_state.pending.get(&seq) {
            pending.abandon(reason);
            return CancelVerdict::Accepted;
        }
        if namespace_state
            .complete
            .iter()
            .any(|entry| entry.seq == seq)
        {
            return CancelVerdict::AlreadyFinished;
        }
        CancelVerdict::Unknown
    }

    /// 取消该 namespace 的**全部**未决请求（附件被替换 / 终态关闭 / 连接退休）。
    pub(crate) fn cancel_pending(
        &self,
        namespace: &RequestNamespaceId,
        reason: RequestCancelReason,
    ) -> Vec<Arc<PendingRequest>> {
        let pending: Vec<Arc<PendingRequest>> = self
            .state
            .read()
            .namespaces
            .get(namespace)
            .map(|state| state.pending.values().cloned().collect())
            .unwrap_or_default();
        for request in &pending {
            request.abandon(reason);
        }
        pending
    }

    // ── 观察面（测试断言语义；不参与判决）────────────────────────────────────

    #[cfg(test)]
    pub(crate) fn namespace_len(&self) -> usize {
        self.state.read().namespaces.len()
    }

    #[cfg(test)]
    pub(crate) fn destroyed_len(&self) -> usize {
        self.state.read().destroyed.len()
    }

    #[cfg(test)]
    pub(crate) fn pending_len(&self, namespace: &RequestNamespaceId) -> usize {
        self.state
            .read()
            .namespaces
            .get(namespace)
            .map(|state| state.pending.len())
            .unwrap_or(0)
    }

    #[cfg(test)]
    pub(crate) fn retained_len(&self, namespace: &RequestNamespaceId) -> usize {
        self.state
            .read()
            .namespaces
            .get(namespace)
            .map(|state| state.complete.len())
            .unwrap_or(0)
    }

    #[cfg(test)]
    pub(crate) fn retained_bytes(&self) -> usize {
        self.state.read().retained_bytes
    }

    /// 预算上限（测试断言用；仍取自同一 §4.3 来源）。
    #[cfg(test)]
    pub(crate) fn budget_for_tests(&self) -> LedgerBudget {
        self.budget
    }
}

/// 从未成为 invocation 的请求被放弃时的**唯一**判决（裁决 D7：不新增 `NotExecutedCause`）。
pub(crate) fn not_executed_before_commit() -> peri_acp_types::mcp_apps::SessionInvocationOutcome {
    peri_acp_types::mcp_apps::SessionInvocationOutcome::NotExecuted {
        cause: peri_acp_types::mcp_apps::NotExecutedCause::CancelledBeforeCommit,
    }
}

#[cfg(test)]
#[path = "apps_ledger_test.rs"]
mod tests;
