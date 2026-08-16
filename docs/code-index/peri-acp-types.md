# peri-acp-types 代码索引

> 速查表：把「我想做什么」映射到文件。细节以代码为准。更新：2026-08-16
> 依据：peri-acp-types/src/lib.rs、docs/standards/architecture-contracts.md、源码（本 crate 无 CLAUDE.md）

## 架构速览

- 定位：契约类型层（type contract layer between layers）——被 peri-agent、peri-acp、peri-middlewares、peri-runtime、peri-tui、peri-workflow、peri-lsp、peri-controller、peri-resources 共同依赖（各 Cargo.toml 均声明 `peri-acp-types`）；只定义类型/枚举/trait，不含执行逻辑（cancel 判定函数除外，是纯函数）
- 事实源矩阵（本层定义、他层 re-export 或消费）：`identity`（AgentId/EventEnvelope/CancelRequest）、`event_v2`（三层事件 + `*_event_to_executor`）、`compact`（CompactConfig/CompactOutcome）、`tools`（BaseTool）、`session`（TurnId/MessageQueue/AgentRuntime）、`messages`（BaseMessage/MessageContent）
- 消费方式：peri-agent 大量 re-export（`src/agent/events_v2.rs:9`、`src/tools/mod.rs:8`、`src/agent/compact_v2/config.rs:8`、`src/session/turn.rs:17`、`src/messages/mod.rs:7`、`src/error.rs:6`）；peri-acp 消费事件映射与 cancel（`src/event/mod.rs:31`、`src/host/prompt_handle.rs:20`）；controller/runtime 直连 identity（`peri-controller/src/controller.rs:27`、`peri-runtime/src/runtime.rs:17`）
- 稳定不变量：身份三元组 + epoch/attempt 不可复用（防迟到消息命中新实例）；`SessionSeq` 单调且**不实现 Default**（缺失必须显式 `Option`）；v2 事件强制携带 `turn_id` + `agent_id`；v1 `ExecutorEvent` 仅作协议序列化面载体，发射统一 v2（ARC-EVENT-001）

## 速查表

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 改 CompactConfig 阈值 | `src/compact.rs`（`CompactConfig` 事实源，struct :210；`peri-agent/src/agent/compact_v2/config.rs:8` 仅 re-export；加载方 `peri-acp/src/host/compact_config.rs:14`；`peri-acp/src/provider/config.rs:194` 可挂配置） | 字段：`auto_compact_threshold`（默认 0.95）、`micro_compact_threshold`（默认 0.75）、`micro_compact_stale_steps`（默认 3）、`smart_compact_enabled`（废弃恒 false，:246）；`apply_env_overrides`（:325）；`has_valid_micro_field_limits`（:316） | serde 反序列化经 `deserialize_threshold_range`（:185）把阈值 clamp 到 [0.0,1.0] 并 warn，防 Full 升级路径被静默绕过；`DISABLE_COMPACT` → 禁用 + micro 阈值=1.0，`DISABLE_AUTO_COMPACT` → 仅禁 auto，`COMPACT_THRESHOLD` 校验后仅覆盖 auto 阈值（:326-339） |
| 改 BaseTool trait / is_direct 默认值 | `src/tools.rs`（trait 事实源；`peri-agent/src/tools/mod.rs:8` re-export；实现方在 peri-middlewares 各工具） | `BaseTool`（:146）；`is_direct`（:199，默认 **false** = deferred）；`context_retention`（:193，默认 `Preserve`）；`timeout`（:170，默认 120s）；`definition`（:152 组合 name/desc/params）；`derive_title_from_name`（:70） | 默认值即行为契约：新工具不覆写 `is_direct` 即为 deferred（经 SearchExtraTools 发现）；`context_retention` 默认 Preserve = 不被压缩；`ToolContext`（:129）只读借用 state，工具不可绕过 dispatch 统一写入 |
| 改 CancelRequest 三元组 | `src/identity.rs`（`CancelRequest` 事实源 :262；`CancelPolicy` 事实源 `src/thread/types.rs:17`） | `CancelRequest::new(identity, policy)`（:273，clear_queue 默认 **false**）；`with_clear_queue`（:282）；`AttemptIdentity`（:140，四元组） | 定位四元组 (session_id, session_epoch, turn_id, attempt_id)，**幂等判定取三元组** (session_id, turn_id, attempt_id)；epoch 不可复用（`SessionEpoch::next` :70 只增）；cancel ≠ 清除待办；消费方仅传递不解释语义：controller `cancel`（controller.rs:336）、runtime `cancel`（runtime.rs:169）、`RuntimePort::cancel`（`src/runtime.rs:85`）、prompt_handle（peri-acp:20 / peri-agent:23） |
| 改 v2 事件枚举 / `*_event_to_executor` 映射 | `src/event_v2.rs`（三层事件唯一事实源）：`RenderEvent` :87、`StateEvent` :204、`ObserveEvent` :278、`Event` :492、`EventBus` :526、`EventHandles` :617 | 发射：`emit_render`（:584，try_send 满时丢弃）、`emit_state`（:592）、`emit_observe`（:601，broadcast 慢消费者 lagging）；映射：`render_event_to_executor` :666、`state_event_to_executor` :745、`observe_event_to_executor` :776 | 三层通道契约：render/state = 有界 mpsc critical，observe = 无界 broadcast；映射为**穷尽匹配**返回 `Option<ExecutorEvent>`，无 v1 等价物显式返回 None（如 HitlPending，走独立审批通道）；TextChunk/ThinkingChunk 用消息级 `message_id`（:677），ToolStart/ToolEnd 用 turn_id 派生（:698/:712）；禁止 wildcard 兜底 |
| 改 MessageContent 判空（is_empty） | `src/messages/content.rs`（`MessageContent` :330；`peri-agent/src/messages/mod.rs:7` re-export） | `is_empty`（:399）；`text_content`（:356）；`content_blocks`（:378）；`has_tool_use`（:408）；`strip_system_reminders`（:469） | 判空按变体：`Text(s) => s.is_empty()`（**不 trim**——纯空白字符串不算空）、`Blocks/Raw` 判 vec 空；消费方（如 peri-agent `is_keepgoing`）须用本函数判空，禁止 trim 替代 |
| 改 AgentId/TurnId 身份类型 | `src/identity.rs` + `src/session.rs` | `AgentId`（identity.rs:18，UUID v7：`new` :22、`from_uuid` :27、`TryFrom<String>` :42）；`TurnId`（session.rs:58，`new` :61、`as_uuid` :65）；`SessionEpoch`（:60，initial=1）；`AttemptId`（:90）；`SessionSeq`（:173）；`EventEnvelope`（:215） | 全部基于 uuid v7（时间有序）；身份构造必须经显式构造器（`SessionSeq` 不实现 `Default`，缺失用 `Option`）；`EventEnvelope` 身份字段（turn_id/agent_id）由事件源填充、session_id 由 Runtime 聚合补打（:216 注释），mapper 不得临时补齐 |
| 加 v2 事件变体（全链路） | `src/event_v2.rs`（定义 + 映射）→ `peri-agent`（EventBus emit）→ `peri-acp/src/event/`（forwarder :106/118/142 消费）→ TUI | 枚举变体 + 对应 `*_event_to_executor` 分支 + `turn_id()`/`agent_id()` 提取 impl（如 StateEvent :251、ObserveEvent :446） | 新变体必须：强制携带 turn_id+agent_id、显式映射结果或过滤理由（穷尽匹配编译期强制）、覆盖 ACP 转发与客户端消费；终止事件必须使客户端离开 loading（ARC-EVENT-001） |
| 改 cancel 判定 / AgentRuntime 注册表 | `src/session.rs` | `AgentRuntime`（:552，thread_id + cancel_token + policy + status）；`cancel_cascade_agents`（:573，仅 Cascade 取消）；`cancel_all_agents`（:582，全部取消）；`cancel_cascade_in`/`cancel_all_in`（:589/:598，HashMap 注册表版） | 判定是纯函数、无层依赖；Independent（bg）子 agent 不随父取消，仅随 session 根取消；`AgentStatus` 事实源 thread/types.rs:56 |
| 改消息队列 / inbox 语义 | `src/session.rs` | `MessageQueue`（:182，`push` :203、`drain_all` :226、`has_wake_up` :235、`has_pending_prompt` :241）；`SessionInbox` :286、`InboxHandle` :370（push_prompt/push_defer/push_info :380-407）；`MessageKind`（:87，`wakes_up` :98） | 队列是 v2 共享 Arc 队列（ACP `AcpSession.v2_message_queue` 与 Agent 共用）；prompt/defer/info 三类消息，wake_up 判定决定循环是否继续 |
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

### session（src/session.rs）

| 功能 | 入口/关键点 |
| --- | --- |
| 身份 | `TurnId`（:58，uuid）；`PromptResult`（:26） |
| 消息队列 | `MessageKind`（:87，`wakes_up` :98）/`MessageSource`（:108）/`QueuedMessage`（:139）；`MessageQueue`（:182）；`SessionInbox`（:286）/`InboxHandle`（:370） |
| 子 agent 注册表 | `AgentRuntime`（:552）；`cancel_cascade_agents`（:573）/`cancel_all_agents`（:582）/`cancel_cascade_in`（:589）/`cancel_all_in`（:598） |
| 会话访问端口 | `SessionAccessPort`（:613，L5：executor 对 ACP SessionManager 的依赖反转端口） |
| cron 归属 | `CronOwner`（:446） |

### identity（src/identity.rs，§9 身份标识契约）

| 功能 | 入口/关键点 |
| --- | --- |
| 身份类型 | `AgentId`（:18，UUID v7）；`SessionEpoch`（:60，initial=1 / next 只增）；`AttemptId`（:90）；`TurnIdentity`（:121）；`AttemptIdentity`（:140，四元组） |
| 事件身份 | `SessionSeq`（:173，单调、不实现 Default）；`EventDeliveryClass`（:200）；`EventEnvelope`（:215，canonical 事件身份） |
| cancel 契约 | `CancelRequest`（:262，identity + clear_queue + policy） |

### event_v2（src/event_v2.rs，三层事件流契约）

| 功能 | 入口/关键点 |
| --- | --- |
| 渲染层 | `RenderEvent`（:87：TextChunk/ThinkingChunk/ToolStarted/ToolEnded/BudgetWarning/HitlPending/TurnCompleted）——critical 有界通道，满时丢弃 |
| 状态层 | `StateEvent`（:204：StateSnapshot/SyntheticUserMessage/TurnSuspended）——critical 有界通道；快照只带元数据不带完整 transcript |
| 观测层 | `ObserveEvent`（:278：LlmCallStart/LlmCallEnd/CompactStarted/CompactEnded/MessagesCompacted/TurnError/SubagentStart/SubagentStop...）——broadcast 无界，慢消费者 lagging |
| 总线 | `Event`（:492，统一枚举）；`EventBus`（:526，`EventBus::new` :562；`emit_render` :584 / `emit_state` :592 / `emit_observe` :601）；`EventHandles`（:617，try_render/try_state/try_observe/subscribe_observe） |
| v1 协议序列化面 | `render_event_to_executor`（:666）/`state_event_to_executor`（:745）/`observe_event_to_executor`（:776）——穷尽匹配，无 v1 等价物显式 None |

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

- ARC-TOOLS-001：`BaseTool::is_direct()` 自声明可见性；true 才直接进 LLM tools，false 仅经 SearchExtraTools 发现、ExecuteExtraTool 执行；包装层须透传
- ARC-CANCEL-001：cancel 按 (session_id, turn_id, attempt_id) 三元组定位（`CancelRequest` 事实源 identity.rs:262）；幂等判定与终态归 Agent 层；`clear_queue` 默认 false
- ARC-EVENT-001：事件链路单事实源（Agent emit v2 → `*_event_to_executor` 协议序列化面 → ACP 映射 → TUI）；穷尽匹配、禁止 wildcard 兜底、禁止恢复 v2_tx 双轨直连
- ARC-FROZEN-001：会话创建时冻结日期/项目指引/skills 摘要/system prompt，会话及 SubAgent 复用，禁止中途重读改变 prompt 前缀
