# peri-acp-types 代码索引

> 速查表：把「我想做什么」映射到文件。细节以代码为准。更新：2026-09-11（模块职责拆分与 compact/历史恢复修复合并）
> 依据：peri-acp-types/src/lib.rs、docs/standards/architecture-contracts.md、源码（本 crate 无 CLAUDE.md）

## 架构速览

- 定位：契约类型层（type contract layer between layers）——被 peri-agent、peri-acp、peri-middlewares、peri-runtime、peri-tui、peri-workflow、peri-lsp、peri-controller、peri-resources 共同依赖（各 Cargo.toml 均声明 `peri-acp-types`）；定义共享类型/枚举/trait；session 契约还包含共享队列、inbox 唤醒、cron task owner 与 cancel 判定，执行编排由 Agent 层负责
- 事实源矩阵（本层定义、他层 re-export 或消费）：`identity`（AgentId/EventEnvelope/CancelRequest）、`event_v2`（三层事件 + `*_event_to_executor`）、`compact`（CompactConfig/CompactOutcome）、`tools`（BaseTool）、`session`（TurnId/MessageQueue/AgentRuntime）、`messages`（BaseMessage/MessageContent）
- 消费方式：peri-agent 大量 re-export（`src/agent/events_v2.rs:9`、`src/tools/mod.rs:8`、`src/agent/compact_v2/config.rs:8`、`src/session/turn.rs:17`、`src/messages/mod.rs:7`、`src/error.rs:6`）；peri-acp 消费事件映射与 cancel（`src/event/mod.rs:31`、`src/host/prompt_handle.rs:20`）；controller/runtime 直连 identity（`peri-controller/src/controller.rs:27`、`peri-runtime/src/runtime.rs:17`）
- 稳定不变量：身份三元组 + epoch/attempt 不可复用（防迟到消息命中新实例）；`SessionSeq` 单调且**不实现 Default**（缺失必须显式 `Option`）；v2 事件强制携带 `turn_id` + `agent_id`；v1 `ExecutorEvent` 仅作协议序列化面载体，发射统一 v2（ARC-EVENT-001）

## 速查表

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 改旧 Compact 上下文的传输兼容 | `src/compact_reminder.rs` + `src/system_reminder.rs` | `legacy_compact_reminders`；`encode_legacy_system_reminder` | 精确识别 plain-text Human 的文件/Skill 回注及摘要格式，仅在模型投影和 ACP replay 出口生成 Legacy reminder；保留数据库原文，分块并转义正文，不提升可信来源；kind 区分 `compact_file` / `compact_skill` / `compact_summary`，供客户端显示简短类型标题 |
| 改 compact 继承与失败契约 | `src/store.rs` + `src/session/execution.rs` + `src/error.rs` | `InheritedContext`；`ThreadStore::{store_inherited_context,load_inherited_context}`；`PromptResult::default`；`AgentError::CompactBudgetUnrecovered` | 版本化 payload/flags 快照校验版本与 ID 完整性；缺失 PromptResult 默认不可恢复热历史；Full 后持续高压给出安全错误文案；ARC-COMPACT-001 |
| 改 CompactConfig 阈值 | `src/compact.rs`（`CompactConfig` 事实源，struct :210；`peri-agent/src/agent/compact_v2/config.rs:8` 仅 re-export；加载方 `peri-acp/src/host/compact_config.rs:14`；`peri-acp/src/provider/config.rs:194` 可挂配置） | 字段：`auto_compact_threshold`（默认 0.95）、`micro_compact_threshold`（默认 0.75）、`micro_compact_stale_steps`（默认 3）、`smart_compact_enabled`（deprecated、默认 false，但运行时仍尊重 true，:246）；`apply_env_overrides`（:325）；`has_valid_micro_field_limits`（:316） | serde 反序列化仅对 `auto_compact_threshold` 经 `deserialize_threshold_range`（:185）clamp 到 [0.0,1.0] 并 warn；`micro_compact_threshold` 当前不走该 helper；`DISABLE_COMPACT` → 禁用 + micro 阈值=1.0，`DISABLE_AUTO_COMPACT` → 仅禁 auto，`COMPACT_THRESHOLD` 校验后仅覆盖 auto 阈值（:326-339） |
| 改 System Reminder 契约/codec/筛选 | `src/system_reminder.rs` + `src/session/{queue,inbox}.rs` + `src/store.rs` + `src/event.rs` | `SystemReminder` / `TrustedSystemReminderFactory`；`encode_system_reminder` / legacy parser；`QueuedPayload::SystemReminder`、`InboxHandle::push_system_reminder`；`PersistedPayload::SystemReminder`；`ExecutorEvent::SystemReminder` | producer 只构造 trusted canonical DTO；`MessageKind` 继续独立决定 wake；模型 wire 编码仅在 transcript 投影边界；持久化与 ACP event 保留结构化字段，未知/损坏版本 fail closed |
| 改 BaseTool trait / is_direct 默认值 | `src/tools.rs`（trait 事实源；`peri-agent/src/tools/mod.rs:8` re-export；实现方在 peri-middlewares 各工具） | `BaseTool`（:146）；`is_direct`（:199，默认 **false** = deferred）；`context_retention`（:193，默认 `Preserve`）；`timeout`（:170，默认 120s）；`definition`（:152 组合 name/desc/params）；`derive_title_from_name`（:70） | 默认值即行为契约：新工具不覆写 `is_direct` 即为 deferred（经 SearchExtraTools 发现）；`context_retention` 默认 Preserve = 不被压缩；`ToolContext`（:129）只读借用 state，工具不可绕过 dispatch 统一写入 |
| 改 CancelRequest 三元组 | `src/identity.rs`（`CancelRequest` 事实源 :262；`CancelPolicy` 事实源 `src/thread/types.rs:17`） | `CancelRequest::new(identity, policy)`（:273，clear_queue 默认 **false**）；`with_clear_queue`（:282）；`AttemptIdentity`（:140，四元组） | 定位四元组 (session_id, session_epoch, turn_id, attempt_id)，**幂等判定取三元组** (session_id, turn_id, attempt_id)；epoch 不可复用（`SessionEpoch::next` :70 只增）；cancel ≠ 清除待办；消费方仅传递不解释语义：controller `cancel`（controller.rs:336）、runtime `cancel`（runtime.rs:169）、`RuntimePort::cancel`（`src/runtime.rs:85`）、prompt_handle（peri-acp:20 / peri-agent:23） |
| 改 v2 事件枚举与身份提取 | `src/event_v2/types.rs`（`src/event_v2.rs` 保留公共 re-export） | `RenderEvent` / `StateEvent` / `ObserveEvent` / `Event` / `TurnErrorReason`；`turn_id` / `agent_id` | 三层事件强制身份字段；TurnCompleted 保持 Render FIFO，ProtocolEvent 保留系统提醒的协议载荷；类型定义不持发送端或执行状态 |
| 改事件通道与容量 | `src/event_v2/bus.rs` | `EventBus::new` / `emit_render` / `emit_state` / `emit_observe`；`EventHandles` | render/state 是有界 mpsc，try_send 满时立即丢弃；observe 是有界 broadcast，慢消费者 lag；drop_timeout 兼容保留但不参与重试 |
| 改 v2 → Executor 协议转换 | `src/event_v2/executor_mapping.rs` | `render_event_to_executor` / `state_event_to_executor` / `observe_event_to_executor` | 穷尽匹配，None 分支有明确过滤理由；chunk 透传消息级 message_id，工具由 turn_id 派生；共享转换的 source_agent_id 为 None，子 Agent 转发器注入 child 来源；SubagentStart/Stop 透传 child_agent_id |
| 改 MessageContent 判空（is_empty） | `src/messages/content.rs`（`MessageContent` :330；`peri-agent/src/messages/mod.rs:7` re-export） | `is_empty`（:399）；`text_content`（:356）；`content_blocks`（:378）；`has_tool_use`（:408）；`strip_system_reminders`（:469） | 判空按变体：`Text(s) => s.is_empty()`（**不 trim**——纯空白字符串不算空）、`Blocks/Raw` 判 vec 空；消费方（如 peri-agent `is_keepgoing`）须用本函数判空，禁止 trim 替代 |
| 改 AgentId/TurnId 身份类型 | `src/identity.rs` + `src/session.rs` | `AgentId`（identity.rs:18，UUID v7：`new` :22、`from_uuid` :27、`TryFrom<String>` :42）；`TurnId`（session.rs:35，`new` :38、`as_uuid` :42）；`SessionEpoch`（:60，initial=1）；`AttemptId`（:90）；`SessionSeq`（:173）；`EventEnvelope`（:215） | 全部基于 uuid v7（时间有序）；身份构造必须经显式构造器（`SessionSeq` 不实现 `Default`，缺失用 `Option`）；`EventEnvelope` 身份字段（turn_id/agent_id）由事件源填充、session_id 由 Runtime 聚合补打（:216 注释），mapper 不得临时补齐 |
| 加 v2 事件变体（全链路） | `src/event_v2/{types,executor_mapping}.rs` → Agent emit → `peri-acp/src/event/forwarder.rs` → TUI | 枚举变体、身份提取与 `*_event_to_executor` 分支；`event_v2/{types,bus,executor_mapping}_test.rs` | 新变体显式映射或说明过滤原因，并覆盖 ACP 转发及客户端消费；Agent 公共路径保留同一类型 identity（ARC-EVENT-001） |
| 改 cancel 判定 / AgentRuntime 注册表 | `src/session/runtime.rs`（`src/session.rs` 保留 public re-export） | `AgentRuntime`（:12）；`cancel_cascade_agents`（:33）；`cancel_all_agents`（:42）；`cancel_cascade_in`（:49）/`cancel_all_in`（:58） | 注册条目持有 thread_id/token/policy/status；Independent 子 agent 不随父取消，仅随 session 根取消；无新增注册表或 token owner |
| 改消息队列 / inbox 语义 | `src/session/queue.rs` + `src/session/inbox.rs`（根 session 保留 public re-export） | `MessageQueue::{push,drain_all,has_wake_up,has_pending_defer,needs_mq_continuation}`；`SessionInbox::await_wake`；`InboxHandle::{push,push_batch,push_system_reminder}` | queue 仍是共享 Arc 队列 + Notify；inbox 共享 queue 并独立持有 wake Notify，保留唤醒前后 has_wake_up 检查；kind 独立决定唤醒，source 定位 pending defer；行为回归经 `peri-agent/src/session/queue_test.rs` 挂载为 `session::queue::tests` |
| 改执行失败 / PromptResult 契约 | `src/session/execution.rs`（根 session 保留 public re-export） | `ExecutionFailure` / `ExecutionFailureKind`；`sanitize_public_error`；`PromptResult`；`TurnTelemetryOutcome::from_result` | fatal 失败 DTO 不派生 serde；公开错误保留原脱敏、限长与 fallback 路径；cancel/max iterations 与 fatal 结果区分，测试在 `src/session_test.rs` |
| 改 slash 命令契约 | `src/command.rs` + `src/command_handler.rs` | `PromptStopReason`（command.rs:69）；`CommandContext`（:95）；`CommandResult`（:256）；`BgForkRequest`（:272）；`CommandHandler`（command_handler.rs:30，`CommandOutcome` :15） | 命令契约与 handler trait 分离：注册表 `command_registry` 经 lib.rs:38 顶层 re-export（挂载本体在 command.rs 子模块区，避免双份模块实例） |

## 子系统

### compact（src/compact.rs）

| 功能 | 入口/关键点 |
| --- | --- |
| 配置契约 | `CompactConfig`（:210）；阈值 serde clamp（`deserialize_threshold_range` :185）；env 覆盖（`apply_env_overrides` :325）；micro 字段截断合法性（`has_valid_micro_field_limits` :316） |
| 执行结果契约 | `CompactOutcome`（:24；`has_applied_change` :49、`is_full_applied` :62）；`FullEscalationReason`（:12） |
| 提取函数 | `extract_file_info`（:68，解析 `[最近读取的文件: ...]` 前缀）；`extract_skill_names`（:87，解析 `[激活的 Skill 指令: ...]` 前缀） |

### tools（src/tools.rs）

| 功能 | 入口/关键点 |
| --- | --- |
| 工具 trait | `BaseTool`（:146）；`is_direct`（:199，默认 false）；`context_retention`（:193，默认 Preserve）；`timeout`（:170，默认 120s）；`aliases`（:176）；`output_char_limit`（:181）；`prefers_persist`（:186）；`title`/`namespace`（:204/:209）；`tool_description` 组装 |
| 描述契约 | `ToolDefinition`（:37，线上 LLM 投影）；`ToolDescription`（:51，title/namespace 仅进程内与提示词层）；`derive_title_from_name`（:70，CamelCase/snake_case 拆词） |
| 压缩保留策略 | `ContextRetention`（:113：Preserve/StateBearing/SideEffectReceipt/Recomputable） |
| 只读上下文 | `ToolContext`（:129，messages + cwd 只读借用）；Todo 契约 `TodoStatus`/`TodoItem`（:15/:24，与 event.rs 同构但独立定义） |

### session（src/session.rs + 私有 session/ 子模块）

所有既有类型与函数仍经 `peri_acp_types::session::*` 导出；Agent 的 re-export 使用同一类型 identity。

| 功能 | 入口/关键点 |
| --- | --- |
| 身份 / 会话访问端口 | `session.rs`：`TurnId`（:35）；`SessionAccessPort`（:68，executor 对 ACP SessionManager 的依赖反转端口） |
| 执行结果与公开错误 | `session/execution.rs`：`ExecutionFailureKind`（:12）；`ExecutionFailure`（:69）；`sanitize_public_error`（:134）；`PromptResult`（:362）；`TurnTelemetryOutcome`（:26） |
| 消息与共享队列 | `session/queue.rs`：`MessageKind`（:13）/`MessageSource`（:34）/`QueuedPayload`（:65）/`QueuedMessage`（:72）；`MessageQueue`（:134），队列状态与 Notify 仍同一 owner |
| inbox 唤醒与投递句柄 | `session/inbox.rs`：`SessionInbox`（:16）/`InboxHandle`（:100）；`await_wake`（:51）；handle 共享 queue/wake 两个 Arc，按消息 kind 决定通知 |
| 子 agent 注册表条目与取消 | `session/runtime.rs`：`AgentRuntime`（:12）；`cancel_cascade_agents`（:33）/`cancel_all_agents`（:42）/`cancel_cascade_in`（:49）/`cancel_all_in`（:58） |
| cron 归属 | `session/cron_owner.rs`：`CronOwner`（:17）；`start`（:44）/`shutdown`（:90）/`Drop` 同模块，保留 task 与 cancel token 所有权、取消优先级 |

### identity（src/identity.rs，§9 身份标识契约）

| 功能 | 入口/关键点 |
| --- | --- |
| 身份类型 | `AgentId`（:18，UUID v7）；`SessionEpoch`（:60，initial=1 / next 只增）；`AttemptId`（:90）；`TurnIdentity`（:121）；`AttemptIdentity`（:140，四元组） |
| 事件身份 | `SessionSeq`（:173，单调、不实现 Default）；`EventDeliveryClass`（:200）；`EventEnvelope`（:215，canonical 事件身份） |
| cancel 契约 | `CancelRequest`（:262，identity + clear_queue + policy） |

### event_v2（src/event_v2.rs 公共入口 + 私有 event_v2/）

| 功能 | 入口/关键点 |
| --- | --- |
| 载荷与身份 | `types.rs`：RenderEvent（含 TurnCompleted）、StateEvent、ObserveEvent、Event；身份提取保持穷尽匹配 |
| 通道 owner | `bus.rs`：EventBus / EventBusConfig / EventHandles；有界 mpsc 与 broadcast 保持原容量、丢弃和 lagging 行为，不新增状态 owner |
| 协议兼容面 | `executor_mapping.rs`：三个 `*_event_to_executor`；只做纯转换，不复制 envelope、取消或消费者生命周期 |
| 契约测试 | `types_test.rs` / `bus_test.rs` / `executor_mapping_test.rs`：身份、serde、FIFO、饱和和映射；Agent 的 `events_v2_test.rs` 只保护公共路径与 prelude 类型 identity，无双套测试 |

### event（src/event.rs，v1 载体）

| 功能 | 入口/关键点 |
| --- | --- |
| v1 协议化载体 | `ExecutorEvent`（:307，仅 ACP 协议序列化面使用，Agent 层禁止构造）；`EventMessage`（:603，envelope + event 包装）；`FnEventHandler`（:635） |
| 载荷 DTO | `CompactFileInfo`（:143）、`TodoEntry`（:150）、`WorkflowProgressPayload`（:171）、`CompactTrigger`（:253）、`CompactThreshold`（:267）等 |

### messages（src/messages/）

| 功能 | 入口/关键点 |
| --- | --- |
| 内容契约 | `MessageContent`（content.rs:330，Text/Blocks/Raw 三变体；`is_empty` :399、`text_content` :356、`content_blocks` :378、`has_tool_use` :408）；`ContentBlock`（content.rs:35）；`strip_system_reminders`（content.rs:469） |
| 消息契约 | `BaseMessage`（message.rs:67）；`MessageId`（:5）；`ToolCallRequest`（:35）；re-export 在 messages/mod.rs:9-12 |

### command（src/command*.rs）

| 功能 | 入口/关键点 |
| --- | --- |
| 命令契约 | `PromptStopReason`（command.rs:69）、`CommandContext`（:95）、`CommandFeedback`（:221）、`CommandResult`（:256）、`BgForkRequest`（:272）、`BgForkSpawner`（:301） |
| handler 契约 | `CommandHandler`（command_handler.rs:30）、`CommandOutcome`（:15）；注册表 `command_registry` 顶层 re-export（lib.rs:38） |

### 其余契约模块（src/）

| 功能 | 入口/关键点 |
| --- | --- |
| 线程/存储 | `thread/types.rs`（`CancelPolicy` :17、`AgentStatus` :56、`ThreadMeta` :126）；`store.rs`（ThreadStore/CompactionLifecycle/MessageFlags） |
| 冻结数据 | `frozen.rs`（`FrozenData` :26、`ThreadPersistence` :39）——会话创建时冻结，SubAgent 复用 |
| 运行端口 | `runtime.rs`（`RuntimePort`，`cancel` :85）；`ports.rs`（McpPoolPort/ToolSearchPort/WorkflowMiddlewarePort/SkillsPort） |
| 其他 | `interaction.rs`（HITL）、`goal.rs`、`tasks.rs`、`cron.rs`、`workflow.rs`、`hooks.rs`、`plugin.rs`、`skills.rs`、`mcp.rs`/`mcp_skills.rs`、`lsp.rs`、`meta_harness.rs`、`peri_caps.rs`（`PeriCaps` re-export lib.rs:57）、`projection.rs`、`permission.rs`、`agents.rs`、`error.rs`（`AgentError`）、`summary.rs`/`event_data.rs`（TUI 消费 DTO） |

## 跨模块契约（指向 architecture-contracts.md，不复制正文）

- ARC-COMPACT-001：继承快照、失败恢复与预算错误契约
- ARC-TOOLS-001：`BaseTool::is_direct()` 自声明可见性；true 才直接进 LLM tools，false 仅经 SearchExtraTools 发现、ExecuteExtraTool 执行；包装层须透传
- ARC-CANCEL-001：cancel 按 (session_id, turn_id, attempt_id) 三元组定位（`CancelRequest` 事实源 identity.rs:262）；幂等判定与终态归 Agent 层；`clear_queue` 默认 false
- ARC-EVENT-001：事件链路单事实源（Agent emit v2 → `*_event_to_executor` 协议序列化面 → ACP 映射 → TUI）；穷尽匹配、禁止 wildcard 兜底、禁止恢复 v2_tx 双轨直连
- ARC-FROZEN-001：会话创建时冻结日期/项目指引/skills 摘要/system prompt，会话及 SubAgent 复用，禁止中途重读改变 prompt 前缀
