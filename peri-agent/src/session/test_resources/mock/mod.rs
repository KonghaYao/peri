//! `#[cfg(test)]` 内存门面替身：只实现测试真正观察的行为，其余行为直接 panic。
//!
//! 用它而不是造假：`unimplemented` 的条目一旦被某个测试用到就会立刻失败，不会把
//! 「没覆盖」伪装成「通过」。需要真实绑定/事务语义的测试请用
//! [`TestSession`](super::test_resources::TestSession)（真实 `SessionResourcesImpl`）。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use peri_acp_types::messages::{BaseMessage, MessageId};
use peri_acp_types::session_resources::{
    AccessMode, BindingRecheck, BindingState, ChildResumeClaim, ChildSnapshot, DataCapabilities,
    ForkSnapshot, FrozenSnapshotBytes, FrozenState, NewSession, NewSessionMeta,
    PersistenceRecovery, RewindBoundary, RewindOutcome, SessionAvailability, SessionMetaPatch,
    SessionResourceError, SessionResourceErrorKind, SessionResourceResult, SessionResources,
    SessionSnapshot, TerminalSettlementOutcome,
};
use peri_acp_types::store::{CompactionChange, InheritedContext, MessageFlags, PersistedPayload};
use peri_acp_types::thread::{AgentStatus, ThreadId, ThreadMeta, WorkState};
use peri_acp_types::workspace::{
    ResolvedWorkspace, ScopedThreadPage, ScopedThreadQuery, SessionBinding, WorkspaceError,
    SESSION_BINDING_VERSION,
};

/// 阶段门：在 store 行为内部暂停，供「取消/丢弃发生在调用中途」的用例观察时序。
///
/// `entered` 在进入临界点后立刻发信号；`release` 由用例放行；等待期间 future 被丢弃时
/// `dropped` 置位——用例据此区分「调用方未来被取消」与「调用已结清」。
pub(crate) struct ResumeLoadGate {
    pub(crate) entered: tokio::sync::oneshot::Sender<()>,
    pub(crate) release: tokio::sync::oneshot::Receiver<()>,
    pub(crate) dropped: Arc<AtomicBool>,
}

impl ResumeLoadGate {
    pub(crate) async fn wait(self) {
        struct DropProbe(Arc<AtomicBool>);
        impl Drop for DropProbe {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let _probe = DropProbe(self.dropped);
        let _ = self.entered.send(());
        let _ = self.release.await;
    }
}

/// 故障注入与观察计数（只针对被测试的行为）。
#[derive(Default)]
struct Injection {
    fail_compaction: bool,
    uncertain_compaction: bool,
    append_calls: usize,
    compaction_calls: usize,
    rewind_calls: usize,
}

/// 单条会话的独立数据区。
///
/// 替身也按会话隔离：不同 thread 的 payload/flags/inherited/frozen/binding 互不影响，
/// 「未登记 id」在 trait 方法上按 `NotFound` 回答（真实门面语义），在夹具便利方法上
/// 按「空区」回答（见文件末尾 `便利方法` 一节的分工说明）。
#[derive(Default)]
struct Region {
    /// `None` 表示该 id 只有数据、没有 metadata 记录（夹具写入即登记时补齐）。
    meta: Option<ThreadMeta>,
    payloads: Vec<PersistedPayload>,
    flags: HashMap<MessageId, MessageFlags>,
    inherited: InheritedContext,
    /// `None` = `FrozenState::LegacyAbsent`（legacy 会话没有快照）。
    frozen: Option<String>,
    /// `None` = 无绑定（`BindingState::Missing`，legacy / 外来登记）。
    binding: Option<SessionBinding>,
}

/// 夹具用默认绑定：`create_bound_thread` / `register_bound_session` 之外的 id 一律无绑定。
#[cfg_attr(not(test), allow(dead_code))]
fn fixture_binding(cwd: &str) -> SessionBinding {
    SessionBinding {
        schema_version: SESSION_BINDING_VERSION,
        revision: 1,
        project_id: peri_acp_types::workspace::ProjectId::new(),
        workspace_id: peri_acp_types::workspace::WorkspaceId::new(),
        cwd_relative_to_workspace: std::path::PathBuf::from(cwd),
    }
}

/// 夹具用默认 frozen 快照字节（版本化 envelope 由 ACP 拥有，替身只搬运字节）。
fn fixture_frozen() -> String {
    "{\"version\":1,\"fixture\":true}".to_owned()
}

/// 只有数据、没有 meta 记录的 id 的默认 meta（写入即登记的替身里不会出现「无 meta」）。
fn default_meta_for(id: &ThreadId) -> ThreadMeta {
    let mut meta = ThreadMeta::new_at("/test", peri_time::now_wall());
    meta.id = id.clone();
    meta
}

/// 由 `NewSessionMeta` 构造替身登记的 `ThreadMeta`（隐藏标记保留调用方意图）。
fn child_meta(id: &ThreadId, meta: &NewSessionMeta, hidden: bool) -> ThreadMeta {
    let mut thread = ThreadMeta::new_at(&meta.cwd, peri_time::now_wall());
    thread.id = id.clone();
    thread.title = meta.title.clone();
    thread.parent_thread_id = meta.parent_thread_id.clone();
    thread.snapshot_at_message_id = meta
        .snapshot_at_message_id
        .map(|id| id.as_uuid().to_string());
    thread.hidden = meta.hidden && hidden;
    thread.cancel_policy = meta.cancel_policy;
    thread
}

pub(crate) struct MockSessionResources {
    /// 每个会话 id 的独立数据区：测试替身也按会话隔离（不同 thread 不串扰）。
    ///
    /// `Arc` 共享给认领 handle：`mark_running` 等写入在 trait 方法返回后仍要落回同一份事实。
    regions: Arc<Mutex<HashMap<ThreadId, Region>>>,
    closing: Mutex<HashSet<ThreadId>>,
    /// 登记顺序（`threads()` 断言用；HashMap 无序）。
    order: Mutex<Vec<ThreadId>>,
    injection: Mutex<Injection>,
    /// 每条 payload 是否允许写入：`false` 时 append 真实失败（模拟后端拒绝）。
    writable: bool,
    /// 状态**变更**序列（同值重复写不记录）：断言「是否残留 active」用。
    ///
    /// 只记录变更与真实 `threads.agent_status` 语义一致——恢复成认领前的值本身是
    /// 一次真实变更，重复写同一个值是 no-op。
    statuses: Arc<Mutex<Vec<(ThreadId, AgentStatus)>>>,
    pub(crate) status_changed: Arc<tokio::sync::Notify>,
    pub(crate) load_gate: Mutex<Option<ResumeLoadGate>>,
    pub(crate) append_gate: Mutex<Option<ResumeLoadGate>>,
    pub(crate) inherited_load_gate: Mutex<Option<ResumeLoadGate>>,
    pub(crate) flags_load_gate: Mutex<Option<ResumeLoadGate>>,
    pub(crate) active_write_gate: Mutex<Option<ResumeLoadGate>>,
    pub(crate) working_write_gate: Mutex<Option<ResumeLoadGate>>,
    pub(crate) fail_working_write: AtomicBool,
    /// 交接提醒的持久化门：让用例在「flush 已成功、提醒尚未写入」之间观察。
    pub(crate) reminder_append_gate: Mutex<Option<ResumeLoadGate>>,
    pub(crate) archive_probe_gate: Mutex<Option<ResumeLoadGate>>,
    /// 认领事实：`claim_child_resume` 成功返回后置位，结清时复位。
    ///
    /// 阶段门（[`ResumeLoadGate`]）只在**认领后**的快照读取生效：resume 的第一次读取是
    /// 认领前的绑定分类，认领后才是 history 装载——用例的门语义按后者定义。
    claimed: Arc<AtomicBool>,
    /// 快照读取故障注入：在第 N 次 `load_session_snapshot`（1-based）返回 Err，0 = 关闭。
    ///
    /// resume 路径有两次快照读取（认领前的绑定分类、认领后的 history 装载），用例据此
    /// 选择「失败发生在哪一次」——置 1 表示绑定读取即失败，置 2 表示认领后装载失败。
    pub(crate) fail_snapshot_at: AtomicUsize,
    /// 已发生的快照读取次数（配合 `fail_snapshot_at` 定位第 N 次）。
    snapshot_reads: AtomicUsize,
    /// 只读能力面：置位后 `inspect_availability` 报告 [`DataCapabilities::HistoryReadOnly`]，
    /// 且所有 mutation 在副作用前返回 `Unsupported`（契约要求，不是「静默 no-op」）。
    history_read_only: AtomicBool,
    /// 单次 compaction 成功写入替身数据区后、返回前触发（测试精确取消时序用）。
    compaction_applied_hook: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    /// MQ 行事实：替身也必须提供持久收件箱，Receive 才能像生产路径一样原子移交。
    ///
    /// 提交后的载荷同步写回本区 `payloads`，让 `load_session_history` 与 MQ 看到同一份事实。
    mq: peri_acp_types::session::InMemoryMqStore,
    /// 故障注入：置位后所有 MQ 入队返回内部错误（验证启动路径不得吞掉接纳失败）。
    mq_enqueue_failure: AtomicBool,
    /// 故障注入：置位后历史查询返回内部错误（验证 `Unknown` 不得被当作不存在）。
    mq_history_failure: AtomicBool,
    /// 故障注入：剩余多少次 MQ 查询返回内部错误（验证查询失败不清理运行期状态）。
    mq_query_failures: AtomicUsize,
    /// 故障注入：剩余多少次终态结算在写入前返回错误（验证同身份重试）。
    finalize_failures: AtomicUsize,
    /// 已发生的终态结算次数（含注入失败）。
    finalize_attempts: AtomicUsize,
    /// 每次 `finalize_child_terminal` 收到的结果消息身份（含注入失败的重试）。
    finalize_message_ids: Mutex<Vec<MessageId>>,
    /// 已发生的状态写入次数（含同值去重前的调用），用于断言唯一写入者。
    status_writes: AtomicUsize,
    /// 模拟远端 `active` 守卫：仅 child 仍 active 时允许 finalize。
    finalize_requires_active: AtomicBool,
    /// 故障注入：剩余多少次无结果终态写入在写完工作意图、写状态前失败。
    terminal_status_failures: AtomicUsize,
    /// `enqueued_at` 生成器：同一进程内单调递增，测试不依赖真实时钟。
    mq_clock: Mutex<u64>,
}

impl MockSessionResources {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            regions: Arc::new(Mutex::new(HashMap::new())),
            closing: Mutex::new(HashSet::new()),
            order: Mutex::new(Vec::new()),
            injection: Mutex::new(Injection::default()),
            writable: true,
            statuses: Arc::new(Mutex::new(Vec::new())),
            status_changed: Arc::new(tokio::sync::Notify::new()),
            load_gate: Mutex::new(None),
            append_gate: Mutex::new(None),
            inherited_load_gate: Mutex::new(None),
            flags_load_gate: Mutex::new(None),
            active_write_gate: Mutex::new(None),
            working_write_gate: Mutex::new(None),
            fail_working_write: AtomicBool::new(false),
            reminder_append_gate: Mutex::new(None),
            archive_probe_gate: Mutex::new(None),
            claimed: Arc::new(AtomicBool::new(false)),
            fail_snapshot_at: AtomicUsize::new(0),
            snapshot_reads: AtomicUsize::new(0),
            history_read_only: AtomicBool::new(false),
            compaction_applied_hook: Mutex::new(None),
            mq: peri_acp_types::session::InMemoryMqStore::new(),
            mq_enqueue_failure: AtomicBool::new(false),
            mq_history_failure: AtomicBool::new(false),
            mq_query_failures: AtomicUsize::new(0),
            finalize_failures: AtomicUsize::new(0),
            finalize_attempts: AtomicUsize::new(0),
            finalize_message_ids: Mutex::new(Vec::new()),
            status_writes: AtomicUsize::new(0),
            finalize_requires_active: AtomicBool::new(false),
            terminal_status_failures: AtomicUsize::new(0),
            mq_clock: Mutex::new(0),
        })
    }

    /// 注册一次 compaction 已写入后的回调；回调在成功返回前执行一次。
    pub(crate) fn on_compaction_applied(&self, hook: impl Fn() + Send + Sync + 'static) {
        *self.compaction_applied_hook.lock().unwrap() = Some(Box::new(hook));
    }

    /// 故障注入：让后续 MQ 入队全部失败。
    pub(crate) fn fail_mq_enqueues(&self) {
        self.mq_enqueue_failure.store(true, Ordering::SeqCst);
    }

    /// 故障注入：让后续 MQ 历史查询全部失败。
    pub(crate) fn fail_mq_history_lookup(&self) {
        self.mq_history_failure.store(true, Ordering::SeqCst);
    }

    /// 故障注入：让接下来的 `count` 次 MQ 查询失败。
    pub(crate) fn fail_next_mq_queries(&self, count: usize) {
        self.mq_query_failures.store(count, Ordering::SeqCst);
    }

    /// 故障注入：让接下来的 `count` 次终态结算在写入前失败。
    pub(crate) fn fail_next_finalizes(&self, count: usize) {
        self.finalize_failures.store(count, Ordering::SeqCst);
    }

    /// 故障注入：让接下来的 `count` 次无结果终态写入在写完工作意图、写状态前失败。
    ///
    /// 用来验证「部分写入不产生 Done + working」以及同一轮重试能否收敛。
    pub(crate) fn fail_next_terminal_status_writes(&self, count: usize) {
        self.terminal_status_failures.store(count, Ordering::SeqCst);
    }

    pub(crate) fn finalize_attempts(&self) -> usize {
        self.finalize_attempts.load(Ordering::SeqCst)
    }

    /// 每次 finalize 尝试携带的结果消息身份；重试必须保持同一身份。
    pub(crate) fn finalize_message_ids(&self) -> Vec<MessageId> {
        self.finalize_message_ids.lock().unwrap().clone()
    }

    pub(crate) fn status_writes(&self) -> usize {
        self.status_writes.load(Ordering::SeqCst)
    }

    /// 模拟远端 `active` 守卫：非 active 的 child 不得再被结算。
    pub(crate) fn require_active_for_finalize(&self) {
        self.finalize_requires_active.store(true, Ordering::SeqCst);
    }

    /// 当前 meta 状态序列（`(thread_id, status)`），只含真实发生变更的写入。
    pub(crate) fn statuses(&self) -> Vec<(ThreadId, String)> {
        self.statuses
            .lock()
            .unwrap()
            .iter()
            .map(|(id, status)| (id.clone(), status_name(*status).to_owned()))
            .collect()
    }

    /// 已登记 thread 快照（父子链断言用），按登记顺序。
    pub(crate) fn threads(&self) -> Vec<ThreadMeta> {
        let regions = self.regions.lock().unwrap();
        self.order
            .lock()
            .unwrap()
            .iter()
            .filter_map(|id| regions.get(id).and_then(|region| region.meta.clone()))
            .collect()
    }

    /// 清空全部会话的继承区（用例修复损坏夹具用）。
    pub(crate) fn clear_inherited(&self) {
        for region in self.regions.lock().unwrap().values_mut() {
            region.inherited = InheritedContext::default();
        }
    }

    /// 写继承区（镜像迁移前 `store_inherited_context` 的测试用法）。
    pub(crate) async fn store_inherited_context(
        &self,
        id: &ThreadId,
        context: &InheritedContext,
    ) -> Result<(), anyhow::Error> {
        self.with_region(id, |region| region.inherited = context.clone());
        Ok(())
    }

    /// 写侧数据区入口（写入即登记：替身不区分「先建 thread 再写」与直接写）。
    fn with_region<R>(&self, id: &ThreadId, work: impl FnOnce(&mut Region) -> R) -> R {
        let mut regions = self.regions.lock().unwrap();
        let region = regions.entry(id.clone()).or_default();
        let out = work(region);
        drop(regions);
        let mut order = self.order.lock().unwrap();
        if !order.iter().any(|known| known == id) {
            order.push(id.clone());
        }
        out
    }

    /// 读侧数据区快照；未登记 id 返回 `None`（trait 读取据此回答 `NotFound`）。
    fn region(&self, id: &ThreadId) -> Option<RegionRead> {
        self.regions
            .lock()
            .unwrap()
            .get(id)
            .map(|region| RegionRead {
                meta: region.meta.clone(),
                payloads: region.payloads.clone(),
                flags: region.flags.clone(),
                inherited: region.inherited.clone(),
                frozen: region.frozen.clone(),
                binding: region.binding.clone(),
            })
    }

    /// 夹具：登记一条**已绑定**会话（有 workspace 绑定与 frozen 快照）。
    ///
    /// 生产前置条件是「父会话有绑定与已持久化 frozen」，而 `create_thread` 登记的是无绑定
    /// 会话（legacy 语义）；需要 bound 语义的用例显式调用本方法，不靠替身默认值。
    pub(crate) fn register_bound_session(&self, id: &str, cwd: &str) {
        let mut meta = ThreadMeta::new_at(cwd, peri_time::now_wall());
        meta.id = id.to_owned();
        self.with_region(&id.to_owned(), |region| {
            region.binding = Some(fixture_binding(cwd));
            region.frozen = Some(fixture_frozen());
            region.meta = Some(meta);
        });
    }

    /// 状态写入入口：只在值真的变化时记录并唤醒等待者。
    fn write_status(&self, id: &ThreadId, status: AgentStatus) {
        self.status_writes.fetch_add(1, Ordering::SeqCst);
        write_status_shared(
            &self.regions,
            &self.statuses,
            &self.status_changed,
            id,
            status,
        );
    }

    /// durable 工作意图写入：只改 meta 的那一列，不进入状态序列。
    fn write_work_state(&self, id: &ThreadId, state: WorkState) {
        self.with_region(id, |region| {
            let mut meta = region
                .meta
                .clone()
                .unwrap_or_else(|| ThreadMeta::new_at("/test", peri_time::now_wall()));
            meta.id = id.clone();
            meta.work_state = state;
            region.meta = Some(meta);
        });
    }

    /// 上下文占用快照写入：只改 meta 的那一列。
    fn write_context_usage(&self, id: &ThreadId, tokens: Option<u64>) {
        self.with_region(id, |region| {
            let mut meta = region
                .meta
                .clone()
                .unwrap_or_else(|| ThreadMeta::new_at("/test", peri_time::now_wall()));
            meta.id = id.clone();
            meta.context_usage_tokens = tokens;
            region.meta = Some(meta);
        });
    }

    /// 切换为只读能力面（测试后端能力差异用）。
    pub(crate) fn restrict_to_history_read_only(&self) {
        self.history_read_only.store(true, Ordering::SeqCst);
    }

    /// mutation 的能力面准入：只读能力面下必须在副作用前失败。
    fn ensure_writable(&self) -> SessionResourceResult<()> {
        if self.history_read_only.load(Ordering::SeqCst) {
            return Err(SessionResourceError::new(
                SessionResourceErrorKind::Unsupported,
            ));
        }
        Ok(())
    }

    /// 取走并等待一个阶段门（未设置时立即返回）。
    async fn wait_gate(gate: &Mutex<Option<ResumeLoadGate>>) {
        let gate = gate.lock().unwrap().take();
        if let Some(gate) = gate {
            gate.wait().await;
        }
    }
}

/// 只读的数据区快照（读路径不需要持有锁，替身数据量小）。
struct RegionRead {
    meta: Option<ThreadMeta>,
    payloads: Vec<PersistedPayload>,
    flags: HashMap<MessageId, MessageFlags>,
    inherited: InheritedContext,
    frozen: Option<String>,
    binding: Option<SessionBinding>,
}

/// 共享的状态写入：只在值变化时记录 + 唤醒（认领 handle 与门面用同一份事实）。
fn write_status_shared(
    regions: &Mutex<HashMap<ThreadId, Region>>,
    statuses: &Mutex<Vec<(ThreadId, AgentStatus)>>,
    status_changed: &tokio::sync::Notify,
    id: &ThreadId,
    status: AgentStatus,
) {
    {
        let mut regions = regions.lock().unwrap();
        let region = regions.entry(id.clone()).or_default();
        if region
            .meta
            .as_ref()
            .is_some_and(|meta| meta.agent_status == status)
        {
            return;
        }
        let mut meta = region
            .meta
            .clone()
            .unwrap_or_else(|| ThreadMeta::new_at("/test", peri_time::now_wall()));
        meta.id = id.clone();
        meta.agent_status = status;
        region.meta = Some(meta);
    }
    statuses.lock().unwrap().push((id.clone(), status));
    status_changed.notify_waiters();
}

fn status_name(status: AgentStatus) -> &'static str {
    status.as_str()
}

/// `claim_child_resume` 返回的认领：语义与资源实现一致——结清时恢复到认领前的状态。
struct MockResumeClaim {
    claimed: Arc<AtomicBool>,
    regions: Arc<Mutex<HashMap<ThreadId, Region>>>,
    statuses: Arc<Mutex<Vec<(ThreadId, AgentStatus)>>>,
    status_changed: Arc<tokio::sync::Notify>,
    child: ThreadId,
    previous: AgentStatus,
}

#[async_trait]
impl ChildResumeClaim for MockResumeClaim {
    async fn mark_running(&self) -> SessionResourceResult<()> {
        write_status_shared(
            &self.regions,
            &self.statuses,
            &self.status_changed,
            &self.child,
            AgentStatus::Active,
        );
        Ok(())
    }

    async fn hand_off_to_background(&self) -> SessionResourceResult<()> {
        self.mark_running().await
    }

    async fn mark_failed(&self) -> SessionResourceResult<()> {
        self.settle();
        Ok(())
    }

    async fn mark_terminated(&self) -> SessionResourceResult<()> {
        self.settle();
        Ok(())
    }
}

impl MockResumeClaim {
    /// 恢复到认领前的状态：终态写入由调用方在结清之后单独提交。
    fn settle(&self) {
        self.claimed.store(false, Ordering::SeqCst);
        write_status_shared(
            &self.regions,
            &self.statuses,
            &self.status_changed,
            &self.child,
            self.previous,
        );
    }
}

// ── 子模块（按职责拆分；内部细节见各自文件头）────────────────────────────
/// 夹具便利方法：镜像迁移前 `ThreadStore` 的常用测试调用形态。
mod fixtures;
/// `peri_model::Model` 形态的假模型助手（生产 bridge 装配的测试入口）。
pub(crate) mod model;
/// 故障注入与观察入口（只覆盖被测试的行为）。
mod observe;
/// `SessionResources` 门面替身：逐个方法实现契约语义。
mod session_resources;
