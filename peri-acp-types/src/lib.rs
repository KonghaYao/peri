//! Type contract layer between layers. Data contracts plus layer-boundary
//! event/session/runtime contracts (tokio-backed channel types).
//!
//! Active modules:
//! - `event_data` — unstable-event payload structs consumed by peri-tui
//! - `peri_caps` — capability negotiation flags consumed by both peri-acp and peri-tui
//! - `summary` — migrated event DTOs re-exported via peri-acp::event
//! - `messages` — 消息契约（BaseMessage/MessageContent/...），peri-agent 保留 re-export
//! - `thread` — Thread 元数据契约（ThreadMeta/ThreadId/CancelPolicy/AgentStatus...）
//! - `store` — ThreadStore 持久化契约（trait + CompactionLifecycle + MessageFlags）
//! - `projection` — compact 投影指令纯数据契约
//! - `identity` — §9 身份标识契约（AgentId/EventEnvelope/CancelRequest/...）
//! - `event` / `event_v2` — 事件契约（ExecutorEvent + v2 三层事件 + EventBus + v1 兼容映射）
//! - `session` — session 契约（TurnId/MQ/inbox/cron/AgentRuntime）
//! - `interaction` — HITL/通道交互契约（UserInteractionBroker/ChannelState/...）
//! - `goal` — goal steering 契约（ThreadGoal/GoalStatus/GoalStore/...）
//! - `frozen` — 会话冻结数据契约（FrozenData/ThreadPersistence/...）
//! - `tasks` — 后台任务契约（BgTaskKind/BgRegistryEvent）
//! - `tools` — 工具契约（ToolDefinition）
//! - `compact` — compact 契约（CompactOutcome/FullEscalationReason + 提取函数）
//! - `error` — 层边界错误契约（AgentError）
//! - `permission` — 权限模式契约（PermissionMode/SharedPermissionMode）
//! - `agents` — agent 定义契约（AgentOverrides/AgentCapability）
//! - `command` — slash 命令契约（PromptStopReason/AgentCommand/CommandContext/BgForkRequest）
//! - `skills` — skill 契约（SkillSource/SkillRoot/SkillMetadata）
//! - `lsp` — LSP 服务器配置契约（LspServerConfig/LspConfigSource）
//! - `cron` — cron 契约（CronTrigger + CronSchedulerPort）
//! - `workflow` — workflow 协议契约（AgentRunParams/ProgressEvent/AgentExecutor/...）
//! - `hooks` — hook 契约（HookEvent/HookType/RegisteredHook/...）
//! - `plugin` — 插件契约（PluginManifest/LoadedPlugin/PluginLoadResult/PluginManagerPort）
//! - `ports` — 装配注入端口（McpPoolPort/ToolSearchPort/WorkflowMiddlewarePort/SkillsPort）

pub mod agents;
pub mod command;
pub mod compact;
pub mod cron;
pub mod error;
pub mod event;
pub mod event_data;
pub mod event_v2;
pub mod frozen;
pub mod goal;
pub mod hooks;
pub mod identity;
pub mod interaction;
pub mod lsp;
pub mod messages;
pub mod model;
pub mod peri_caps;
pub use peri_caps::PeriCaps;
pub mod permission;
pub mod plugin;
pub mod ports;
pub mod projection;
pub mod runtime;
pub mod session;
pub mod skills;
pub mod store;
pub mod summary;
pub mod tasks;
pub mod thread;
pub mod tools;
pub mod workflow;
