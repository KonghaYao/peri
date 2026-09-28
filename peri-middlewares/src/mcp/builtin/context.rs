//! builtin 实例上下文（IF-P3-04 / A33，owner H-02）：宿主装配注入的实例状态载体。
//!
//! 边界：
//! - **唯一可见面**是 `peri_middlewares::assembly` 的再导出；`crate::mcp::builtin` 仍是
//!   `pub(crate)`，宿主（`peri-acp`）不得 import 本模块路径（A33）。
//! - 字段承载的是**实例构造所需状态**（cron scheduler / LSP pool / cwd / 关闭集 /
//!   workspace 的 session 级输入），**不是**工具执行上下文：7 个 workspace 工具当前都不读
//!   `ToolContext::cwd`（`invoke` 一律 `_ctx`），宿主 cwd / session 上下文到 builtin 工具
//!   执行的贯通是 wave 3 的缺口（F1）。本类型不声称该缺口已贯通，也不把 `cwd` 当作工具
//!   执行上下文的替代品。
//! - 字段的**生命周期粒度不同**：`cwd` / `cron` / `lsp` / `closed` 是 host 级，而
//!   [`BuiltinInstanceContext::workspace`] 承载的是 **session 级**输入（per-session 的
//!   `TaskManager` 与 bg 完成回调，AW3-11 / 主 plan §2.1）；两者的来源与 `None` 语义见
//!   各自字段文档。
//! - 同一个状态对象只注入一次（A33）：注入状态与一次性语义在
//!   `McpClientPool::set_builtin_instance_context`（同一短锁保护上下文与
//!   「initialize 已开始」标志）；本模块只提供类型与便利构造，自身不持有注入状态。
//! - cron scheduler 与 LSP pool 由**组合根**构造并以 `Arc` 注入同一份（A1）：本类型不新建
//!   第二份 scheduler / pool（`Arc::ptr_eq` 可观察）。

use std::{collections::BTreeSet, sync::Arc};

use parking_lot::Mutex;
use peri_acp_types::builtin_mcp::find;
use peri_acp_types::tasks::TaskManager;
use peri_resources::lsp::pool::LspServerPool;
use thiserror::Error;

use crate::assembly::OnBgCompleteFn;
use crate::cron::CronScheduler;

/// 实例上下文注入的 typed 语义错误（A33）。
///
/// 两种拒绝都是**「首个生效」**语义：既有上下文继续生效，调用方记录并拒绝本次注入。
/// 错误文本是固定短语：不含路径、env、凭据或任何实例外部事实。
#[derive(Debug, Error, PartialEq, Eq)]
pub enum BuiltinContextError {
    /// 重复注入（含传入**同一** `Arc` 的再注入）。
    #[error("builtin 实例上下文已注入")]
    AlreadyInjected,
    /// `initialize` 已开始后的首次注入（晚注入）。
    #[error("builtin 实例上下文注入晚于初始化开始")]
    InitializationStarted,
}

/// `cron` 实例的上下文输入。
///
/// `scheduler` 是组合根持有的**同一份** scheduler（`Arc::ptr_eq` 可观察，本 crate 内不得
/// 另建一份）；`tick_enabled` 是宿主 `drive_cron_tick` 的投影——只有它为真时 pool 的
/// 唯一 spawn 点才为该代 transport 挂 tick（A32），print / stdio 路径保持无 tick 差异。
pub struct CronInstanceInput {
    /// 组合根构造的 scheduler（与 `CronSchedulerPort` 装配侧同一份 `Arc`）。
    pub scheduler: Arc<Mutex<CronScheduler>>,
    /// tick 驱动开关（宿主投影；与 scheduler 同源，不由本类型推导）。
    pub tick_enabled: bool,
}

/// `lsp` 实例的上下文输入。
///
/// `pool` 是 host 级**唯一** pool：无 LSP 配置时仍注入空配置 pool（`has_servers()` 为假
/// ⇒ handler 工具面为空表），不得用「不注入」表达「无配置」——那会让实例退化成
/// 「上下文缺失」而不是「可见但空」。
pub struct LspInstanceInput {
    /// host 级 LSP pool（经 `peri_resources::lsp` 门面构造）。
    pub pool: Arc<LspServerPool>,
}

/// `workspace` 实例的上下文输入（AW3-11 的 **session 级** seam）。
///
/// 两名成员各恢复 `BashTool` 的一条能力源——它们与 `crate::middleware::terminal` 的
/// `BashTool::with_task_manager` / `BashTool::with_on_bg_complete` 入参**同型**
/// （`OnBgCompleteFn` 见下），因此 handler 只是把接到的值原样转交，不做包装或适配。
///
/// 与 `cron` / `lsp` 的 host 级输入不同，本输入的**来源是会话环境装配**（每 session 一份：
/// per-session `TaskManager` 诞生于 `ensure_session`，见主 plan §3.4.1 Q1/Q2 的「不通达」
/// 结论与 §2 AW3-11 的 seam 冻结），因此装配面必须在 `McpClientPool::run_initialize` **之前**
/// 把它送进 [`BuiltinInstanceContext`]。
///
/// `None`（整个字段为 `None`，或成员为 `None`）的语义是**可见但退化**——handler 照常构造、
/// 7 个工具照常声明，只有 `Bash` 失去「后台任务」那一路（逐条退化见
/// `crate::mcp::builtin::workspace` 的模块头），**不是**「实例不可用」：因此
/// [`BuiltinInstanceContext::instance_input_ready`] 不为 `workspace` 增加 arm（AW3-11 明文）。
#[derive(Clone)]
pub struct WorkspaceInstanceInput {
    /// per-session 的后台任务管理器（`run_in_background` 发起、前台超时提升、`command &`
    /// 进程组登记与外部执行所有权跟踪都由它承载）。
    ///
    /// 类型与 `BashTool::task_manager` 字段逐字同型（`peri_acp_types::tasks::TaskManager`）。
    pub task_manager: Option<Arc<dyn TaskManager>>,
    /// bg shell 完成回调（在 `registry.complete()` 之前调用）。
    ///
    /// 类型即 [`OnBgCompleteFn`]（`crate::assembly` 的再导出）——它与
    /// `peri_acp_types::tasks::OnBgCompleteFn` 是**同一底层类型**
    /// （`Arc<dyn Fn(&BackgroundTaskResult, BgTaskKind) + Send + Sync>`，type alias 透明），
    /// 也与 `BashTool::with_on_bg_complete` 的入参同型；这里取 assembly 的再导出是因为宿主
    /// （A33：不得 import `peri_middlewares::mcp::builtin`）经 `peri_middlewares::assembly`
    /// 构造本输入，同一个名字在两侧指同一类型。
    pub on_bg_complete: Option<OnBgCompleteFn>,
}

/// 宿主装配构造并注入 pool 的 builtin 实例上下文（IF-P3-04）。
///
/// 字段按冻结形状保持 `pub`：宿主可直接构造字面量，也可经 [`Self::new`] 系列便利构造。
pub struct BuiltinInstanceContext {
    /// host 单 cwd：`artifact` 实例的相对路径解析根，也是 `lsp` pool `root_uri` 的来源。
    /// 与 pool 的 `execution_cwd` 同源（同一 host cwd），不支持多 cwd。
    pub cwd: String,
    /// `cron` 实例输入；`None` = 未提供（`cron` 实例不可装配，见 [`Self::instance_input_ready`]）。
    pub cron: Option<CronInstanceInput>,
    /// `lsp` 实例输入；`None` = 未提供（同上）。
    pub lsp: Option<LspInstanceInput>,
    /// `workspace` 实例输入（**session 级**，见 [`WorkspaceInstanceInput`]）。
    ///
    /// `None` = **可见但退化**：`workspace` 实例照常装配（`dispatch` 无条件构造 handler），
    /// 只有 `Bash` 失去后台任务那一路——与 `cron` / `lsp` 的「缺输入 ⇒ 不可装配」不同，
    /// 因此 [`Self::instance_input_ready`] 不为它增加 arm（AW3-11）。
    ///
    /// 来源：会话环境装配（`peri-acp/src/host/workspace.rs` 的 `SessionEnvironment::assemble`）；
    /// 顶层三路径与 1:N 形态（`session_resources = true` 当 server root）不产生 session，
    /// 因此传 `None`（该退化形态进验收记录）。
    pub workspace: Option<WorkspaceInstanceInput>,
    /// A24 关闭集：`policy_key ∈ disabled_middlewares` 的实例名（唯一实现
    /// `mcp::builtin::closed_instances`，宿主经 `peri_middlewares::assembly` 的薄委托派生）。
    ///
    /// 语义是**非物理**关闭（只关本 turn 的工具投影与同步目标），因此本类型只承载它、
    /// 不据此拒绝装配：装配面（`dispatch` / `runtime`）不消费 `closed`。
    pub closed: BTreeSet<String>,
}

impl BuiltinInstanceContext {
    /// 最小构造：给定 cwd，无实例输入、关闭集为空。
    pub fn new(cwd: impl Into<String>) -> Self {
        Self {
            cwd: cwd.into(),
            cron: None,
            lsp: None,
            workspace: None,
            closed: BTreeSet::new(),
        }
    }

    /// 提供 `cron` 实例输入。
    pub fn with_cron(mut self, input: CronInstanceInput) -> Self {
        self.cron = Some(input);
        self
    }

    /// 提供 `lsp` 实例输入。
    pub fn with_lsp(mut self, input: LspInstanceInput) -> Self {
        self.lsp = Some(input);
        self
    }

    /// 提供 `workspace` 实例输入（**session 级**；AW3-11 的 seam 接收端）。
    ///
    /// 未调用时 `workspace` 仍是「可见但退化」——本 builder 只表达「有 session 级输入」，
    /// 与 `with_cron` / `with_lsp` 的「不调用 ⇒ 实例不可装配」不是同一件事。
    pub fn with_workspace(mut self, input: WorkspaceInstanceInput) -> Self {
        self.workspace = Some(input);
        self
    }

    /// 设置 A24 关闭集（`closed_instances(...)` 的产物，原样承载）。
    pub fn with_closed(mut self, closed: BTreeSet<String>) -> Self {
        self.closed = closed;
        self
    }

    /// 该实例所需的上下文输入是否齐备——**唯一**判定，供 `runtime`（拒绝装配）与后续
    /// 实例装配共同使用，避免各自硬编码实例名。
    ///
    /// 判定按注册表名（`peri_acp_types::builtin_mcp::find(instance)?.name`），不另立第二张
    /// 实例名字表：`cron` 需要 [`Self::cron`]、`lsp` 需要 [`Self::lsp`]；`web` / `artifact`
    /// 不需要额外输入（`artifact` 的解析根是 [`Self::cwd`]）。
    ///
    /// `workspace` **不在本判定内**（AW3-11 明文）：它落 `Some(_) => true` 分支——
    /// [`Self::workspace`] 为 `None` 时 handler 照常装配（可见但退化），把「缺 session 级输入」
    /// 判成「不可装配」会把一个可用实例报成 `InstanceInputMissing`。
    ///
    /// 未注册实例（表外名字与保留但未实现的预留名）返回 `false`：调用方
    /// （`runtime::spawn_builtin_transport_with_context`）在此之前已用
    /// `require_known_builtin_instance` 收口 `UnknownInstance`，本返回值不会掩盖它。
    /// 注册表将来新增「不需要额外输入」的实例时默认落 `true` 分支——缺 handler 仍由
    /// `HandlerNotWired` 诚实收口，不伪装成「输入缺失」。
    pub(crate) fn instance_input_ready(&self, instance: &str) -> bool {
        match find(instance).map(|registered| registered.name) {
            Some("cron") => self.cron.is_some(),
            Some("lsp") => self.lsp.is_some(),
            Some(_) => true,
            None => false,
        }
    }
}

#[cfg(test)]
#[path = "context_test.rs"]
mod tests;
