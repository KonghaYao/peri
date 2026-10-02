//! builtin 实例上下文（IF-P3-04 / A33，owner H-02）：宿主装配注入的实例状态载体。
//!
//! 边界：
//! - 宿主上下文类型与 cron/LSP 输入经 `peri_middlewares::assembly` 再导出；
//!   `WorkspaceInstanceInput` 由 `peri-mcp-workspace` 直接公开（仅保留旧构造测试）。`crate::mcp::builtin` 仍是
//!   `pub(crate)`，宿主（`peri-acp`）不得 import 本模块路径（A33）。
//! - 字段承载的是**实例构造所需状态**（cron scheduler / LSP pool / cwd / 关闭集 /
//!   workspace 的遗留输入槽），**不是**工具执行上下文：7 个 workspace 工具当前都不读
//!   `ToolContext::cwd`（`invoke` 一律 `_ctx`），宿主 cwd / session 上下文到 builtin 工具
//!   执行的贯通是 wave 3 的缺口（F1）。本类型不声称该缺口已贯通，也不把 `cwd` 当作工具
//!   执行上下文的替代品。
//! - 生产 Workspace 不消费 `workspace` 遗留输入槽，后台 Bash 任务由 Workspace 自持。
//! - 同一个状态对象只注入一次（A33）：注入状态与一次性语义在
//!   `McpClientPool::set_builtin_instance_context`（同一短锁保护上下文与
//!   「initialize 已开始」标志）；本模块只提供类型与便利构造，自身不持有注入状态。
//! - cron scheduler 与 LSP pool 由**组合根**构造并以 `Arc` 注入同一份（A1）：本类型不新建
//!   第二份 scheduler / pool（`Arc::ptr_eq` 可观察）。

use std::{collections::BTreeSet, sync::Arc};

use parking_lot::Mutex;
use peri_acp_types::builtin_mcp::find;
use peri_mcp_lsp::pool::LspServerPool;
use thiserror::Error;

use peri_mcp_cron::CronScheduler;
use peri_mcp_workspace::{WorkspaceInstanceInput, WorkspaceResourcesInput};

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
    /// host 级 LSP pool（经 `peri_mcp_lsp` 门面构造）。
    pub pool: Arc<LspServerPool>,
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
    /// 遗留测试输入槽。生产 dispatcher 不读取它；Workspace 自己持有 Bash 任务。
    pub workspace: Option<WorkspaceInstanceInput>,
    /// `workspace` 实例的**资源面**输入（`WorkspaceMcpServer::with_resources` 的装配输入）。
    ///
    /// `None` = **资源面未接线**（本槽位之前的既有行为）：`workspace` 实例照常装配，
    /// 但 `resources/list` 只有 git ref，`skills/*` 返回 `-32601`、新 scheme 的
    /// `resources/read` 返回 `-32602`——与「空目录集」不同，不得混同。
    ///
    /// **一次性装配输入**：宿主装配（会话环境）在
    /// `McpClientPool::run_initialize` 之前随本上下文一次注入，dispatch 只读不改、
    /// 不读配置、不派生根（AW3-11 模式）。资源根列表（skills / agents / builtin
    /// 关闭位）的事实源是装配期输入（F11 插件 manifest / F12 配置读取已在该层完成）。
    pub workspace_resources: Option<WorkspaceResourcesInput>,
    /// A24 关闭集：`policy_key ∈ disabled_middlewares` 的实例名（唯一实现
    /// `mcp::builtin::closed_instances`，宿主经 `peri_middlewares::assembly` 的薄委托派生）。
    ///
    /// 语义是**非物理**关闭（只关本 turn 的工具投影与同步目标），因此本类型只承载它、
    /// 不据此拒绝装配：装配面（`dispatch` / `runtime`）不消费 `closed`。
    pub closed: BTreeSet<String>,
    /// 宿主技能面关闭位：与 [`Self::closed`] **同源**——同一份 `disabled_middlewares`
    /// 的投影（`"SkillsMiddleware" ∈ disabled`，宿主装配一次派生、两处承载），
    /// 语义 = 宿主技能面（`core:{skill}` 裸名命令投影）关闭。
    ///
    /// **不是**实例关闭：workspace 实例照常装配，7 个工具、资源面与
    /// `{server}:{skill}` MCP 发现面均不受影响（链槽关闭 = `SkillsMiddleware`
    /// 不构造，13_skills 段落与两个技能工具随槽位消失；命令面不得留下幽灵路由）。
    /// 唯一消费点是发现管线的 core 投影（`skill_discovery::project_core_skill_commands`）。
    pub skills_face_closed: bool,
}

impl BuiltinInstanceContext {
    /// 最小构造：给定 cwd，无实例输入、关闭集为空、技能面关闭位为假。
    pub fn new(cwd: impl Into<String>) -> Self {
        Self {
            cwd: cwd.into(),
            cron: None,
            lsp: None,
            workspace: None,
            workspace_resources: None,
            closed: BTreeSet::new(),
            skills_face_closed: false,
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

    /// 仅供旧装配测试；生产 Workspace 不使用此输入。
    pub fn with_workspace(mut self, input: WorkspaceInstanceInput) -> Self {
        self.workspace = Some(input);
        self
    }

    /// 提供 `workspace` 实例的**资源面**输入（资源根 / builtin 关闭位 / 预算）。
    ///
    /// 未调用 = 资源面未接线（[`Self::workspace_resources`] 的诚实口径）；本 builder
    /// 只表达「装配期已产出该输入」，不改变 `instance_input_ready`——资源面缺失是
    /// 「未支持」而不是「实例不可装配」（`workspace` 落 `Some(_) => true` 分支）。
    pub fn with_workspace_resources(mut self, input: WorkspaceResourcesInput) -> Self {
        self.workspace_resources = Some(input);
        self
    }

    /// 设置 A24 关闭集（`closed_instances(...)` 的产物，原样承载）。
    pub fn with_closed(mut self, closed: BTreeSet<String>) -> Self {
        self.closed = closed;
        self
    }

    /// 设置宿主技能面关闭位（宿主装配从**同一份** `disabled_middlewares` 派生，
    /// 见 [`Self::skills_face_closed`]；不在此处推导，避免第二份判定）。
    pub fn with_skills_face_closed(mut self, closed: bool) -> Self {
        self.skills_face_closed = closed;
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
