//! Implementation of the APIs re-exported by the parent module.

use super::{
    Any, Arc, BaseMessage, CancellationToken, CompactConfig, Deserialize, EventSink, HashMap,
    ParsedArgs, Serialize, TaskManager, TypeId,
};

/// 命令执行停止原因（`executor::PromptStopReason` 契约化，ACP re-export）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptStopReason {
    /// 正常完成——agent 完成本轮。
    EndTurn,
    /// 用户经 `session/cancel` 取消。
    Cancelled,
    /// agent 达到最大迭代次数。
    MaxTurnRequests,
    /// 模型连续输出截断，自动续跑预算已耗尽。
    MaxTokens,
}

/// 命令执行上下文（L5 契约化：原 ACP `session::command::CommandContext`）。
///
/// 拆层（设计 §74 / 不变式 5）：core 5 字段常驻（session_id / history /
/// cwd / event_sink / cancel_token），扩展依赖收进私有字段
/// `CommandContext::deps`（[`DependencyBag`]），经
/// [`CommandContext::dep`] 按接口获取——新增依赖不动本结构体。
///
/// 本结构为「两步走」过渡态：core 之外的旧字段（compact_config /
/// auxiliary_model / args / thread_store / frozen_* / task_manager
/// 等）**Phase 2 适配完成后将随消费方迁移逐步退役**，迁移前由消费方
/// 构造点经 [`CommandContext::new`] + 旧字段显式赋值全量预填
/// （`executor_helpers.rs:340` 先例），行为等价零漂移。
pub struct CommandContext {
    pub session_id: String,
    pub history: Vec<BaseMessage>,
    pub cwd: String,
    /// compact 管线配置（ACP 装配点按 `load_compact_config` 语义预填：
    /// unwrap_or_default + env overrides，每次 CommandContext 构造 = 每轮）。
    pub compact_config: CompactConfig,
    /// 辅助 LLM（v2 stages/compact.rs 摘要 + Goal 工具验证共用）。
    pub auxiliary_model: Option<Arc<dyn peri_model::Model>>,
    pub event_sink: Arc<dyn EventSink>,
    /// 用户消息原文（拦截层整段透传，含 `/` 前缀与 args；RPC 路径仅命令名
    /// 文本、无 `/` 前缀保证，且 Inject 在 RPC 路径显式报错、值不被消费）。
    /// `AgentPassthrough` 等需要把原文交还 agent 管线的 handler 消费——
    /// 命令名与 args 均已切分，原文不可重建，故随上下文携带。
    pub raw_text: String,
    /// 当前上下文是否存在可接收 Inject 的 agent 管线（跨层契约，决策 A2/D）。
    ///
    /// 拦截层（`executor_helpers.rs`）置 `true`——`CommandOutcome::Inject`
    /// 原文会替换用户消息进 agent 管线，由 SkillPreload 等后续处理；RPC 路径
    /// （`execute_command.rs`）恒为默认 `false`——无管线可注入，
    /// `McpSkillReleaser` 依此降级为直接返回 skill 全文（决策 D）。
    pub supports_inject: bool,
    /// 命令参数（命令名之后的文本）。
    pub args: String,
    /// 命令参数统一解析结果（P1-1：拦截层 / execute-command RPC 路径在
    /// 构造本结构前经 `args_schema.parse` 权威校验，成功结果传此字段；
    /// `args_schema: None` 的条目为 None）。handler 消费本字段，不再
    /// 自研解析（验收标准第 2 条：旧的自研解析代码删除）。
    pub parsed_args: Option<ParsedArgs>,
    /// 取消令牌，用于 Ctrl+C 打断长时间运行的命令（如 compact 的 LLM 调用）。
    pub cancel_token: CancellationToken,
    /// 会话资源门面：rewind/compact 等需要读写会话历史的命令经此访问。
    ///
    /// `None` = 该上下文没有持久化后端（打印/测试路径）；命令按「无持久化」处理，
    /// 不另建存储旁路。
    pub session_resources: Option<Arc<dyn crate::session_resources::SessionResources>>,
    /// 当前会话的 thread ID，配合 `session_resources` 使用。
    pub thread_id: Option<String>,
    /// 后台任务管理器（Immediate 命令依赖，如 rewind 的异步执行）。
    pub task_manager: Option<Arc<dyn TaskManager>>,
    /// Frozen CLAUDE.md main content（会话级捕获）。
    pub frozen_claude_md: Option<Arc<String>>,
    /// Frozen CLAUDE.local.md content
    pub frozen_claude_local_md: Option<Arc<String>>,
    /// Frozen skills summary
    pub frozen_skill_summary: Option<Arc<String>>,
    /// Frozen system prompt（fork 路径复用以避免重建）。
    pub frozen_system_prompt: Option<Arc<String>>,
    /// 扩展依赖接口注册表（设计 §74 / 不变式 5）：core 之外的一切按接口
    /// 注入，新增依赖不动本结构体（注入/取用形态见 [`DependencyBag`] 与
    /// [`CommandContext::dep`]）。
    pub(super) deps: DependencyBag,
}

/// 扩展依赖接口注册表（设计 §74 / 不变式 5）：core 之外的一切按接口注入，
/// 新增依赖不动 [`CommandContext`] 结构体。
///
/// key = `TypeId::of::<T>()`（具体类型或 trait object 均可）；value 的
/// **动态类型必须是 `Arc<T>` 形态**（注入契约，见 [`CommandContext::dep`]
/// 的 Safety 约定）。
pub type DependencyBag = HashMap<TypeId, Arc<dyn Any + Send + Sync>>;

impl CommandContext {
    /// 按具体类型取扩展依赖（`TypeId::of::<T>()` 查表；可注入 mock）。缺失 →
    /// None，调用方（命令 handler）返回 feedback(Error) 优雅报错，不 panic。
    ///
    /// # 注入契约
    ///
    /// 本方法经 std [`Arc::downcast`] 还原（`T: Sized`——裸 `dyn Trait` 形态
    /// 调用直接编译失败；trait object 依赖以 `Arc<dyn Trait>` **具体类型**
    /// 形态注入与取用）。[`DependencyBag`] 中 key = `TypeId::of::<T>()` 的
    /// 条目，其 value 必须以 `Arc<T>` 形态 upcast 注入：
    ///
    /// - 具体类型：`bag.insert(TypeId::of::<T>(), Arc::new(v) as Arc<dyn Any + Send + Sync>)`；
    /// - trait object：`trait Trait: Send + Sync`，
    ///   `bag.insert(TypeId::of::<Arc<dyn Trait>>(), Arc::new(arc_dyn) as Arc<dyn Any + Send + Sync>)`，
    ///   取用 `ctx.dep::<Arc<dyn Trait>>()`（`TypeId` 键与 `Arc<T>` 动态类型
    ///   均以 `Arc<dyn Trait>` 为维度，`Arc::downcast` 内部再校验，形态违反
    ///   自动回落 None，不构成 UB）。
    ///
    /// 注入形态与 [`crate::ports`] 端口 `downcast_arc` 的 `as_any` 样板同一类
    /// 约定；本步无**生产**注入方（生产表恒空；注入契约由 `context_deps_tests`
    /// 覆盖），Phase 5 命令迁移起按此契约注入。
    pub fn dep<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.deps
            .get(&TypeId::of::<T>())
            .and_then(|dep| Arc::clone(dep).downcast::<T>().ok())
    }

    /// 构造辅助：core 5 字段 + 扩展依赖预填表（[`DependencyBag`]）。
    ///
    /// 旧字段（core 之外的 12 个）取默认/空值：Phase 2 适配完成后旧字段将
    /// 随消费方迁移逐步退役，迁移前消费方构造点以 `new()` + 旧字段逐字段
    /// 赋值全量预填。
    pub fn new(
        session_id: String,
        history: Vec<BaseMessage>,
        cwd: String,
        event_sink: Arc<dyn EventSink>,
        cancel_token: CancellationToken,
        deps: DependencyBag,
    ) -> Self {
        CommandContext {
            session_id,
            history,
            cwd,
            event_sink,
            cancel_token,
            deps,
            compact_config: CompactConfig::default(),
            auxiliary_model: None,
            raw_text: String::new(),
            supports_inject: false,
            args: String::new(),
            parsed_args: None,
            session_resources: None,
            thread_id: None,
            task_manager: None,
            frozen_claude_md: None,
            frozen_claude_local_md: None,
            frozen_skill_summary: None,
            frozen_system_prompt: None,
        }
    }
}

/// 命令反馈（设计 §79/§89：默认 UiOnly，不污染会话；Session 仅命令显式 opt-in）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandFeedback {
    /// 反馈级别。
    pub level: FeedbackLevel,
    /// 反馈内容（用户可见文本）。
    pub message: String,
    /// 反馈通道；缺省（反序列化未提供）回落 UiOnly。
    #[serde(default)]
    pub channel: FeedbackChannel,
}

/// 命令反馈级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FeedbackLevel {
    /// 信息提示。
    Info,
    /// 警告。
    Warning,
    /// 错误。
    Error,
}

/// 命令反馈通道（设计 §79：默认 UI-only，不污染会话；会话是 agent 的上下文，
/// 运维反馈不是 agent 该看的）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FeedbackChannel {
    /// 通知条/状态区渲染，不进会话，agent 永不见（默认，设计 §79）。
    #[default]
    UiOnly,
    /// 命令显式 opt-in 才另写系统消息进会话。
    Session,
}

/// 命令执行结果（设计 §89：增 feedback 字段）。
pub struct CommandResult {
    /// 执行后的消息历史。
    pub messages: Vec<BaseMessage>,
    /// 停止原因。
    pub stop_reason: PromptStopReason,
    /// 命令反馈；None = 无反馈。执行失败 = level: Error 走同一通道（设计 §81）。
    pub feedback: Option<CommandFeedback>,
}
