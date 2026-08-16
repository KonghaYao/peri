//! Slash 命令契约（L5：命令执行体迁入 Agent 层的边界端口）。
//!
//! 自 `peri-acp/src/session/command/mod.rs` 与 `peri-acp/src/host/exec/`
//! 命令执行体（`bg` / `compact_pipeline`）迁入：命令执行模型
//! （[`CommandHandler`]）、
//! 执行上下文（[`CommandContext`]）、/bg fork 请求（[`BgForkRequest`] +
//! [`BgForkSpawner`]）与命令/执行终态（[`CommandResult`] / [`PromptStopReason`]）
//! 归本层，Agent 层命令实现经本契约执行，ACP 保留协议化薄壳与装配面
//! （命令注册表 / spawner 实现 / EventSink 实现）。
//!
//! 依赖反转说明：
//! - `peri_config`（ACP provider 配置）不进入本契约——`CommandContext` 以
//!   `compact_config`（compact 管线输入）投影，/bg fork 的 LLM 构造由
//!   [`BgForkSpawner`] 实现方（ACP 装配面）自持配置；
//! - 事件发射经 [`crate::event::EventSink`] 端口（ACP 实现，协议序列化面）。

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::compact::CompactConfig;
use crate::event::{EventSink, ExecutorEvent};
use crate::messages::BaseMessage;
use crate::store::ThreadStore;
use crate::tasks::TaskManager;

// ─── 命令契约子模块（Phase 1 拆出，经本模块导出，lib.rs 挂载点不变）────────
//
// 命令系统契约按职责拆分：
// - `command_name`    — 全名词法契约（CommandName / CommandLevel / CommandNameError）
// - `command_args`    — 参数 schema serde 模型（ArgsSchema / ArgSpec / ArgKind / FlagSpec）
// - `command_handler` — 目标执行模型（CommandHandler trait / CommandOutcome 三态）
// - `command_registry`— 运行时注册表（CommandRegistry / RegisterError / ResolvedCommand）
// - `command_route`   — 路由表条目契约（RouteEntry / CommandSource / provenance / UiCommandSpec）
// `#[path]` 声明沿用 mcp_skills.rs 测试内联先例（文件落 crate 根，经 path 挂载）。
#[path = "command_args.rs"]
pub mod command_args;
#[path = "command_handler.rs"]
pub mod command_handler;
#[path = "command_name.rs"]
pub mod command_name;
#[path = "command_registry.rs"]
pub mod command_registry;
#[path = "command_route.rs"]
pub mod command_route;

// 词法契约平铺 re-export：消费路径保持 `command::CommandName`（计划隐含形态，
// Phase 2 注册表接入按此 import）。
pub use command_name::{CommandLevel, CommandName, CommandNameError};
// args schema 模型经本模块顶层 re-export（计划步骤 3/6 均以
// `crate::command::ArgsSchema` 形态引用，Phase 3 协议层沿用；
// `ParsedArgs` 为解析结果形态，拦截层 / RPC 路径统一解析后经
// `CommandContext::parsed_args` 传入 handler）。
pub use command_args::{ArgKind, ArgSpec, ArgsSchema, FlagSpec, ParsedArgs};
// 注册表契约平铺 re-export（Phase 2 Step 4 换型：peri-agent 拦截层 /
// peri-acp 组合根经 `command::CommandRegistry` 等路径引用；顶层
// `command_registry::` 路径同步可用，session/mod.rs 先例）。
pub use command_registry::{CommandRegistry, RegisterError, ResolvedCommand};
// 执行模型契约平铺 re-export（Phase 2 Step 4 换型：拦截层 / RPC 路径匹配
// `CommandOutcome`；handler trait 供组合根适配器实现）。
pub use command_handler::{CommandHandler, CommandOutcome};

/// 命令执行停止原因（`executor::PromptStopReason` 契约化，ACP re-export）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptStopReason {
    /// 正常完成——agent 完成本轮。
    EndTurn,
    /// 用户经 `session/cancel` 取消。
    Cancelled,
    /// agent 达到最大迭代次数。
    MaxTurnRequests,
}

/// 命令执行上下文（L5 契约化：原 ACP `session::command::CommandContext`）。
///
/// `peri_config`（ACP provider 配置）不进入本结构：
/// - compact 管线使用 [`CommandContext::compact_config`]（ACP 装配点按
///   `load_compact_config` 语义预填，env overrides 每轮重新应用）；
/// - /bg fork 的 LLM 构造由 [`BgForkSpawner`] 实现方自持配置。
///
/// 拆层（设计 §74 / 不变式 5）：core 5 字段常驻（session_id / history /
/// cwd / event_sink / cancel_token），扩展依赖收进私有字段
/// [`CommandContext::deps`]（[`DependencyBag`]），经
/// [`CommandContext::dep`] 按接口获取——新增依赖不动本结构体。
///
/// 本结构为「两步走」过渡态：core 之外的旧字段（compact_config /
/// auxiliary_model / args / thread_store / frozen_* / bg_* / task_manager
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
    /// 持久化存储，用于 rewind 等需要删除消息的命令。
    pub thread_store: Option<Arc<dyn ThreadStore>>,
    /// 当前会话的 thread ID，配合 thread_store 使用。
    pub thread_id: Option<String>,
    /// 后台任务事件的发送通道（BgCommand 等 Immediate 命令依赖）。
    pub bg_event_sender: Option<tokio::sync::mpsc::UnboundedSender<ExecutorEvent>>,
    /// 后台任务管理器（BgCommand 等 Immediate 命令依赖）。
    pub task_manager: Option<Arc<dyn TaskManager>>,
    /// Frozen CLAUDE.md main content（会话级捕获，BgCommand 透传到 fork agent）。
    pub frozen_claude_md: Option<Arc<String>>,
    /// Frozen CLAUDE.local.md content
    pub frozen_claude_local_md: Option<Arc<String>>,
    /// Frozen skills summary
    pub frozen_skill_summary: Option<Arc<String>>,
    /// Frozen system prompt（fork 路径复用以避免重建）。
    pub frozen_system_prompt: Option<Arc<String>>,
    /// `/bg` fork agent 启动器（legacy：Phase 2 拆层后注入面已迁
    /// `ctx.dep::<Arc<dyn BgForkSpawner>>()`，本字段无生产消费方，列入退役
    /// 清单；当前「字段一个未删」策略下保留，`new()` 恒 None）。
    pub bg_spawner: Option<Arc<dyn BgForkSpawner>>,
    /// 扩展依赖接口注册表（设计 §74 / 不变式 5）：core 之外的一切按接口
    /// 注入，新增依赖不动本结构体（注入/取用形态见 [`DependencyBag`] 与
    /// [`CommandContext::dep`]）。
    deps: DependencyBag,
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
            thread_store: None,
            thread_id: None,
            bg_event_sender: None,
            task_manager: None,
            frozen_claude_md: None,
            frozen_claude_local_md: None,
            frozen_skill_summary: None,
            frozen_system_prompt: None,
            bg_spawner: None,
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

/// `/bg` fork agent 启动请求（纯数据，跨层透传）。
///
/// 命令定义（Agent 层 `session::exec::bg::BgCommand`）只构造本请求并交给
/// 注入的 [`BgForkSpawner`]；深绑 ACP/Agent 层类型（LLM 构造 / 工具集 /
/// SubAgent 发起）的实现在装配面（`peri-agent::session::exec::executor_helpers`
/// 的 `DefaultBgForkSpawner`，经 `BgForkSpawner` 端口注入），命令层不引用
/// 业务面实现。
pub struct BgForkRequest {
    /// 后台任务描述。
    pub prompt: String,
    /// 父会话消息历史（fork 上下文）。
    pub parent_messages: Vec<BaseMessage>,
    /// 父会话 thread id。
    pub parent_thread_id: Option<String>,
    /// 工作目录。
    pub cwd: String,
    /// 冻结 CLAUDE.md main content。
    pub frozen_claude_md: Option<String>,
    /// 冻结 CLAUDE.local.md content。
    pub frozen_claude_local_md: Option<String>,
    /// 冻结 skills summary。
    pub frozen_skill_summary: Option<String>,
    /// 冻结 system prompt（fork 路径复用，避免重建）。
    pub frozen_system_prompt: Option<String>,
    /// 后台任务事件通道（子 agent 事件经此到达事件泵）。
    pub bg_event_sender: tokio::sync::mpsc::UnboundedSender<ExecutorEvent>,
    /// 持久化存储。
    pub thread_store: Arc<dyn ThreadStore>,
}

/// `/bg` fork agent 启动接口（装配注入）。
///
/// 实现方为 ACP executor 装配面（深绑 Agent 层 `SessionFactory`）；命令定义
/// 只经本接口发起，不直接引用 Agent 层类型。`peri_config`（LLM 构造输入）
/// 由实现方自持，不进入请求契约。
#[async_trait]
pub trait BgForkSpawner: Send + Sync {
    /// 启动后台 fork agent。返回 `Err(用户可见错误信息)`。
    async fn spawn_fork(&self, req: BgForkRequest) -> Result<(), String>;
}

#[cfg(test)]
mod feedback_tests {
    use super::*;
    use serde_json::json;

    /// 完整序列化：字段名 camelCase，变体 UiOnly 序列化为 "uiOnly"。
    #[test]
    fn feedback_serializes_camel_case() {
        let fb = CommandFeedback {
            level: FeedbackLevel::Warning,
            message: "测试反馈".to_string(),
            channel: FeedbackChannel::UiOnly,
        };
        let v = serde_json::to_value(&fb).unwrap();
        assert_eq!(
            v,
            json!({"level": "warning", "message": "测试反馈", "channel": "uiOnly"})
        );
    }

    /// 缺省 channel：反序列化未提供时回落 UiOnly（#[serde(default)]，设计 §79）。
    #[test]
    fn feedback_missing_channel_defaults_to_ui_only() {
        let fb: CommandFeedback =
            serde_json::from_value(json!({"level": "error", "message": "boom"})).unwrap();
        assert_eq!(fb.channel, FeedbackChannel::UiOnly);
        assert_eq!(fb.level, FeedbackLevel::Error);
    }

    /// 反序列化显式 Session 通道往返一致。
    #[test]
    fn feedback_session_channel_roundtrip() {
        let fb = CommandFeedback {
            level: FeedbackLevel::Info,
            message: "已应用 skill X".to_string(),
            channel: FeedbackChannel::Session,
        };
        let v = serde_json::to_value(&fb).unwrap();
        let back: CommandFeedback = serde_json::from_value(v).unwrap();
        assert_eq!(back, fb);
    }

    /// FeedbackChannel 默认值 = UiOnly（#[default] 与设计 §79「UiOnly 默认」一致）。
    #[test]
    fn feedback_channel_default_is_ui_only() {
        assert_eq!(FeedbackChannel::default(), FeedbackChannel::UiOnly);
    }
}

#[cfg(test)]
mod context_deps_tests {
    use super::*;

    /// 测试用事件出口（EventSink 必需方法的最小实现）。
    struct NoopSink;
    #[async_trait]
    impl EventSink for NoopSink {
        async fn push_event(
            &self,
            _session_id: &str,
            _event: &ExecutorEvent,
            _context_window: u32,
        ) {
        }
        async fn push_done(
            &self,
            _session_id: &str,
            _stop_reason: &str,
            _request_id: Option<&str>,
        ) {
        }
    }

    /// 测试用注入接口：注入契约形态 2 要求 `Send + Sync`（trait object 以
    /// `Arc<dyn Trait>` 具体类型形态注入与取用）。
    trait DepGreeter: Send + Sync {
        fn greet(&self) -> &'static str;
    }
    struct EnGreeter;
    impl DepGreeter for EnGreeter {
        fn greet(&self) -> &'static str {
            "hello"
        }
    }
    struct FrGreeter;
    impl DepGreeter for FrGreeter {
        fn greet(&self) -> &'static str {
            "bonjour"
        }
    }

    fn ctx_with(deps: DependencyBag) -> CommandContext {
        let sink: Arc<dyn EventSink> = Arc::new(NoopSink);
        CommandContext::new(
            "s1".to_string(),
            vec![],
            "/tmp".to_string(),
            Arc::clone(&sink),
            CancellationToken::new(),
            deps,
        )
    }

    /// 注入契约形态 1：具体类型注入 + 具体类型查询（type_id 精确匹配）。
    #[test]
    fn dep_concrete_roundtrip() {
        let mut deps = DependencyBag::new();
        deps.insert(
            TypeId::of::<EnGreeter>(),
            Arc::new(EnGreeter) as Arc<dyn Any + Send + Sync>,
        );
        let ctx = ctx_with(deps);
        assert_eq!(ctx.dep::<EnGreeter>().unwrap().greet(), "hello");
        assert!(ctx.dep::<FrGreeter>().is_none());
    }

    /// 注入契约形态 2：trait object 以 `Arc<dyn Trait>` 具体类型形态注入 +
    /// 接口查询（`dep::<Arc<dyn Trait>>()`，std `Arc::downcast` 还原）。
    #[test]
    fn dep_dyn_trait_roundtrip() {
        let mut deps = DependencyBag::new();
        let fr: Arc<dyn DepGreeter> = Arc::new(FrGreeter);
        deps.insert(
            TypeId::of::<Arc<dyn DepGreeter>>(),
            Arc::new(fr) as Arc<dyn Any + Send + Sync>,
        );
        let ctx = ctx_with(deps);
        // 两次 dep 各 clone 句柄：refcount 须正确递增（downcast 所有权转移，
        // 回归保护：位模式复制会破坏引用计数导致双重释放）。
        let g1 = ctx.dep::<Arc<dyn DepGreeter>>().expect("按接口取依赖");
        let g2 = ctx.dep::<Arc<dyn DepGreeter>>().expect("按接口取依赖");
        assert_eq!(Arc::strong_count(&g1), 3, "表 + g1 + g2");
        assert_eq!(g1.greet(), "bonjour");
        assert_eq!(g2.greet(), "bonjour");
        drop(g1);
        drop(g2);
        // key 隔离：具体类型 key 与 trait object 包装形态 key 互不冲突；
        // 裸 `dyn Trait` 形态因 `dep` 要求 `T: Sized` 无法编译（注入契约
        // 强制 `Arc<dyn Trait>` 具体类型形态）。
        assert!(ctx.dep::<FrGreeter>().is_none());
    }

    /// 缺失依赖 → None（消费约定：handler 返回 feedback(Error)，不 panic）。
    #[test]
    fn dep_missing_returns_none() {
        let ctx = ctx_with(DependencyBag::new());
        assert!(ctx.dep::<EnGreeter>().is_none());
        assert!(ctx.dep::<Arc<dyn DepGreeter>>().is_none());
        // 注入形态违反（类型不匹配）→ downcast 失败回落 None，不 panic。
        let mut deps = DependencyBag::new();
        deps.insert(
            TypeId::of::<Arc<dyn DepGreeter>>(),
            Arc::new(EnGreeter) as Arc<dyn Any + Send + Sync>,
        );
        let ctx = ctx_with(deps);
        assert!(ctx.dep::<Arc<dyn DepGreeter>>().is_none());
    }

    /// new() 构造：core 5 字段就位，旧字段取默认/空值（两步走过渡态）。
    #[test]
    fn new_sets_core_fields_and_legacy_defaults() {
        let sink: Arc<dyn EventSink> = Arc::new(NoopSink);
        let cancel = CancellationToken::new();
        let ctx = CommandContext::new(
            "s1".to_string(),
            vec![],
            "/tmp".to_string(),
            Arc::clone(&sink),
            cancel.clone(),
            DependencyBag::new(),
        );
        assert_eq!(ctx.session_id, "s1");
        assert!(ctx.history.is_empty());
        assert_eq!(ctx.cwd, "/tmp");
        assert!(Arc::ptr_eq(&ctx.event_sink, &sink));
        assert!(ctx.cancel_token.is_cancelled() == cancel.is_cancelled());
        // 旧字段默认值（消费方迁移前经 `new()` + 逐字段赋值全量预填）。
        assert_eq!(ctx.args, "");
        assert!(ctx.auxiliary_model.is_none());
        assert!(ctx.thread_store.is_none());
        assert!(ctx.thread_id.is_none());
        assert!(ctx.bg_event_sender.is_none());
        assert!(ctx.task_manager.is_none());
        assert!(ctx.frozen_claude_md.is_none());
        assert!(ctx.frozen_claude_local_md.is_none());
        assert!(ctx.frozen_skill_summary.is_none());
        assert!(ctx.frozen_system_prompt.is_none());
        assert!(ctx.bg_spawner.is_none());
    }
}
