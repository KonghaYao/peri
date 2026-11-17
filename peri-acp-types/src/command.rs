//! Slash 命令契约（L5：命令执行体迁入 Agent 层的边界端口）。
//!
//! 命令执行模型、执行上下文与命令执行终态归本层，Agent 层命令实现经本契约执行。

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::compact::CompactConfig;
use crate::event::EventSink;
use crate::messages::BaseMessage;
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
#[path = "args_schema.rs"]
pub mod args_schema;
pub use args_schema as command_args;
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

pub use command_context::PromptStopReason;

pub use command_context::CommandContext;

pub use command_context::DependencyBag;

pub use command_context::CommandFeedback;

pub use command_context::FeedbackLevel;

pub use command_context::FeedbackChannel;

pub use command_context::CommandResult;

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
    #[async_trait::async_trait]
    impl EventSink for NoopSink {
        async fn push_event(
            &self,
            _session_id: &str,
            _event: &crate::event::ExecutorEvent,
            _context_window: u32,
        ) -> Result<(), crate::event::EventSinkError> {
            Ok(())
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
        assert!(ctx.session_resources.is_none());
        assert!(ctx.thread_id.is_none());
        assert!(ctx.task_manager.is_none());
        assert!(ctx.frozen_claude_md.is_none());
        assert!(ctx.frozen_claude_local_md.is_none());
        assert!(ctx.frozen_skill_summary.is_none());
        assert!(ctx.frozen_system_prompt.is_none());
    }
}

#[path = "command/command_context.rs"]
mod command_context;
