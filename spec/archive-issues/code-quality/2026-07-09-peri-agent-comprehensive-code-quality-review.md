# peri-agent 全面代码水平审查与改进总纲


> 归档于 2026-07-20，原路径 spec/issues/2026-07-09-peri-agent-comprehensive-code-quality-review.md
**状态**：Fixed
**优先级**：高
**创建日期**：2026-07-09
**类型**：技术债

## 问题描述

基于 2026-07-08 三项专项审查 + 2026-07-09 ultra-batch 三维度并行审查（架构设计 7.5/10 + 实现质量 8.0/10 + 测试卫生 7.5/10），汇总 peri-agent crate 的全部代码水平问题。本 issue 合并已有 3 个 Partial issue（`code-quality` / `architecture` / `maintainability`）中的未完成子项 + 今日审查新发现的问题，作为 peri-agent 质量改进的完整跟踪清单。

**已有 3 个 Partial issue 已完成的工作**：
- ✅ 持久化 writer task panic 检测（P0）
- ✅ `dispatch_concurrent` 工具调用 300s timeout（P0）
- ✅ AgentState ⇄ MessageTranscript 双轨存储统一为 AgentContext（P0）
- ✅ `tool_dispatch` 编排函数集成测试（P0）

---

## 问题清单

### 高优先级

#### H1：`events_v2_mapper.rs` 16 处 `panic!` 致 ReAct 循环崩溃风险

- **文件**：`peri-agent/src/agent/events_v2_mapper.rs`
- **现象**：事件映射层中 16 处 `panic!`，任一处击中即终止整个 ReAct 循环。事件映射为纯数据转换，不应因输入异常而崩溃
- **期望**：全部替换为 `tracing::error!` + 降级返回（`None` 或默认值）
- **估算**：1 天
- **来源**：2026-07-09 ultra-batch 实现质量审查

#### H2：`stages/` 目录核心 ReAct 阶段零测试覆盖

- **文件**：`peri-agent/src/agent/stages/{reason,act,receive,compact,end,middleware_runner,mod}.rs`
- **现象**：~3500 行 ReAct 循环核心逻辑无任何测试。`tool_dispatch.rs` 已有编排函数测试（7/8 完成），但 `act.rs`（工具分发入口）、`end.rs`（循环终止条件）、`receive.rs`（MQ drain）、`compact.rs`（上下文压缩触发）等关键阶段仍然零覆盖
- **期望**：至少为 `act.rs`、`end.rs`、`receive.rs` 添加单元测试
- **估算**：3-5 天
- **来源**：2026-07-09 ultra-batch 测试卫生审查 + 2026-07-08 maintainability #1（已部分完成）

#### H3：`block_to_anthropic` 静默丢弃序列化错误

- **文件**：`peri-agent/src/llm/anthropic/invoke.rs:34`
- **现象**：`serde_json::to_value(source).unwrap_or_default()` — Document 序列化失败时静默吞错，错误数据完全不可恢复且无告警
- **期望**：至少记录 `tracing::warn!`，考虑降级策略（跳过该 content block 而非整个消息失败）
- **估算**：1 天
- **来源**：2026-07-08 code-quality #3

#### H4：工具参数 JSON 解析失败时丢失原始语义

- **文件**：`peri-agent/src/llm/openai/invoke.rs:266-275`
- **现象**：`parse_assistant_message` 中工具参数 JSON 解析失败时降级为 `{_raw_arguments: "..."}`，丢失原始参数语义且无 error_suggest 注入
- **期望**：保留原始字符串在 `_raw_arguments` 之外，同时注入 error_suggest 提示 LLM 修正格式
- **估算**：1 天
- **来源**：2026-07-08 code-quality #5

#### H5：`parse_assistant_message` 临时引用生命周期隐患

- **文件**：`peri-agent/src/llm/openai/invoke.rs:260`
- **现象**：`assistant_msg["tool_calls"].as_array().unwrap_or(&vec![])` 使用临时 `vec![]` 的引用，在 Rust 临时对象生命周期延长规则下有理论 UB 风险
- **期望**：改为 `let empty = vec![]; unwrap_or(&empty)`
- **估算**：30 分钟
- **来源**：2026-07-08 code-quality #4

---

### 中优先级

#### M1：`RetryableLLM` 重试循环不检查 CancellationToken

- **文件**：`peri-agent/src/llm/retry.rs:99-146`
- **现象**：重试循环 `for attempt in 0..max_retries` 期间不检查 cancel，用户 Ctrl+C 后需等待所有重试完成才能退出
- **期望**：在循环内添加 `if cancel.is_cancelled() { return Err(AgentError::Interrupted) }`
- **估算**：30 分钟
- **来源**：2026-07-09 ultra-batch 实现质量审查

#### M2：`AgentContext::from_stage` 每次全量克隆 visible_messages

- **文件**：`peri-agent/src/agent/agent_context.rs:65-82`
- **现象**：每次 middleware hook 调用都从 `ctx.transcript.read()` 克隆全部可见消息。在 500 条消息、19 个 middleware、每轮 3-5 次 hook 的场景下累积 O(n²) 克隆开销
- **期望**：引入 token-based 缓存失效机制——transcript `version`/`len` 未变时复用上次缓存
- **估算**：1 天
- **来源**：2026-07-09 ultra-batch 架构审查

#### M3：`before_tool` / `before_tools_batch` 签名不一致

- **文件**：`peri-agent/src/middleware/trait.rs:72-96`、`chain.rs:58-108`
- **现象**：两个方法语义类似但签名不同——`before_tool` 签名为 `&ToolCall → AgentResult<ToolCall>`，`before_tools_batch` 签名为 `&[ToolCall] → Vec<AgentResult<ToolCall>>`。chain 中 batch 实现逻辑复杂（过滤已拒绝调用继续传递），存在 Bug 风险
- **期望**：统一为 `before_tools_batch`（更通用），废弃单体 `before_tool`
- **估算**：1-2 天
- **来源**：2026-07-09 ultra-batch 架构审查

#### M4：中间件执行顺序缺乏声明性约束

- **文件**：`peri-agent/src/middleware/chain.rs`
- **现象**：19 个 middleware 的固定顺序仅记录在 CLAUDE.md 中，代码层面无任何机制表达"CompactMiddleware 必须在 GoalSteering 之前"等约束。第三方 middleware 若在错误位置 add 会破坏 prompt cache
- **期望**：引入 `fn priority() -> i32` → `MiddlewareChain::add()` 自动按优先级排序
- **估算**：3-5 天
- **来源**：2026-07-08 architecture #2

#### M5：事件与 SQLite schema 无版本号

- **文件**：`peri-agent/src/agent/events_v2.rs`、`peri-agent/src/thread/sqlite_store.rs`
- **现象**：RenderEvent/StateEvent/ObserveEvent 无 `#[serde(tag = "version")]` 机制，未来变体变更导致旧数据无法反序列化；SQLite 无 `PRAGMA user_version`，迁移通过 "duplicate column name" 错误隐式检测
- **期望**：事件类型添加版本标记；SQLite 设置 `PRAGMA user_version` 并递增检查
- **估算**：2-3 天
- **来源**：2026-07-08 maintainability #2

#### M6：零 `#[deprecated]` 属性——无 API 演进过渡策略

- **文件**：整个 `peri-agent/src/`
- **现象**：v1 `ExecutorEvent` 部分变体（`TurnCommitted`/`StateSnapshotMeta`/`CompactCompleted`）在 v2 已有等价物，但未标记废弃。被 peri-acp/peri-tui/peri-middlewares 依赖的公共 API 无迁移指引
- **期望**：为计划移除的 v1 API 添加 `#[deprecated]` 并提供迁移指引
- **估算**：2-3 天
- **来源**：2026-07-08 maintainability #5

#### M7：无 feature flag——所有依赖无条件编译

- **文件**：`peri-agent/Cargo.toml`
- **现象**：`sqlx`、`reqwest`、`sysinfo`、`fuzzy-matcher` 等依赖无条件编译，仅使用事件系统或消息类型的消费者也需编译全部依赖
- **期望**：添加 `sqlite`、`stream`、`compact` 等 feature flag
- **估算**：1 天
- **来源**：2026-07-08 maintainability #7

---

### 低优先级

#### L1：`StageContext` 23 字段 God Object

- **文件**：`peri-agent/src/agent/stages/mod.rs:70-135`
- **现象**：23 个字段（10 个 `Option`），`compact_pre_hook`/`compact_post_hook`（ACP 注入回调）、`idle_inbox`/`idle_should_wait`（transport 唤醒策略）、`recall_buffer`（跨 hook 累加器）散落在结构体中
- **期望**：提取为 `StageHooks`、`CompactResources`、`TransportHooks` 等子结构体
- **估算**：2-3 天
- **来源**：2026-07-08 architecture #3

#### L2：`AgentGroup` 仍使用 v1 `ExecutorEvent`

- **文件**：`peri-agent/src/group/mod.rs:68`
- **现象**：`AgentGroup::event_tx` 使用 `UnboundedSender<ExecutorEvent>`，v2 核心使用 `EventBus` 三层系统
- **期望**：将 AgentGroup 迁移到 v2 EventBus
- **估算**：1-2 周
- **来源**：2026-07-08 architecture #4

#### L3：`run_react_loop` 控制流过于复杂

- **文件**：`peri-agent/src/agent/stages/mod.rs:516-732`
- **现象**：216 行函数混合正常五阶段循环、End 阶段 should_continue 新一轮 turn、Idle wake 路径（`tokio::select!` + `drain_for_end` + `woken_once` 守卫），三层嵌套在一个 `for` 循环中
- **期望**：提取 idle-wake 路径为独立函数 `try_idle_wake()`，SubAgent 完成检测逻辑下沉到中间件或队列消费回调
- **估算**：1 天
- **来源**：2026-07-08 maintainability #3 + 2026-07-09 架构审查

#### L4：`full_compact_inner` 单个函数承担 7 个步骤

- **文件**：`peri-agent/src/agent/compact_v2.rs:355-472`
- **现象**：117 行函数承担预处理消息、构造 LLM 请求、调用 LLM、后处理摘要、标记 excluded、追加 Human 摘要、re-inject 文件+Skills 共 7 个步骤
- **期望**：拆分为 `full_compact_collect` + `full_compact_summarize` + `full_compact_commit`
- **估算**：2-3 天
- **来源**：2026-07-08 maintainability #4

#### L5：`MiddlewareState` setter 为 no-op

- **文件**：`peri-agent/src/middleware/state.rs:25-26`、`agent_context.rs:90-128`
- **现象**：`set_cwd` 和 `set_current_step` 在 `AgentContext` 中是 no-op，`AgentState` 的实现是真实的。trait 定义过度泛化
- **期望**：标记 `#[deprecated]` 并计划删除，或拆分为 `ReadonlyMiddlewareState` + `MutableMiddlewareState`
- **估算**：1 天
- **来源**：2026-07-09 ultra-batch 架构审查

#### L6：Tool `invoke` 返回 `Box<dyn Error>` 丢失错误结构

- **文件**：`peri-agent/src/tools/mod.rs:48-52`
- **现象**：`invoke()` 返回 `Result<String, Box<dyn std::error::Error + Send + Sync>>`，与 agent 框架其余部分使用 `AgentError` 不一致。`dispatch_concurrent` 中只能拿到 `e.to_string()` 的字符串表示
- **期望**：改为返回 `AgentResult<String>` 或提供 `Into<AgentError>` 转换路径
- **估算**：2-3 天
- **来源**：2026-07-09 ultra-batch 架构审查

#### L7：`channel_broker` 静默吞错 + `events_v2` 26 unwrap 无文档

- **文件**：`peri-agent/src/interaction/channel_broker.rs:87`、`peri-agent/src/agent/events_v2.rs`
- **现象**：`serde_json::to_value(&req).unwrap_or_default()` 序列化失败静默吞掉发送 blank params；`events_v2.rs` 26 处 `.unwrap()` 无 panic 安全说明
- **期望**：channel_broker 失败时日志告警并拒绝发送；events_v2 unwrap 添加 "Safety: infallible" 注释
- **估算**：1 天
- **来源**：2026-07-09 ultra-batch 实现质量审查 + 测试卫生审查

#### L8：文档不足——`lib.rs` 仅 5 行 + 6 个 `mod.rs` 无模块文档

- **文件**：`peri-agent/src/lib.rs`、`agent/mod.rs`、`messages/mod.rs`、`middleware/mod.rs`、`tools/mod.rs`、`thread/mod.rs`、`interaction/mod.rs`
- **现象**：`lib.rs` 仅 2 行简述，缺少架构概览；6 个核心模块 `mod.rs` 只有 `pub mod xxx` 声明
- **期望**：`lib.rs` 补充 ReAct 循环/Middleware/Session/Turn 概念概述；各 `mod.rs` 添加 3-5 行模块职责说明
- **估算**：1-2 天
- **来源**：2026-07-09 ultra-batch 测试卫生审查

#### L9：`sqlite_store_test` 硬编码 sleep 做排序断言

- **文件**：`peri-agent/src/thread/sqlite_store_test.rs:30`
- **现象**：`tokio::time::sleep(Duration::from_millis(5))` 硬编码延时做排序断言，在 CI 或负载机器上可能因调度抖动误报
- **期望**：改用显式时间戳比较或依赖 SQLite `updated_at` 字段排序
- **估算**：30 分钟
- **来源**：2026-07-09 ultra-batch 测试卫生审查

#### L10：`CompactConfig` Default 与 serde 默认值双写

- **文件**：`peri-agent/src/agent/compact/config.rs`
- **现象**：每个字段写一个显式 `fn default_*()`，然后在 `Default` impl 和 `#[serde(default = "...")]` 中各调用一次，两个 impl 须保持同步
- **期望**：`derive(Default)` → serde `default` 指向同一个 `Default::default()` 值
- **估算**：1 天
- **来源**：2026-07-08 maintainability #6

#### L11：`sqlite_store` 函数签名过长 / `ThreadId` 是类型别名

- **文件**：`peri-agent/src/thread/sqlite_store.rs:212-250`、`peri-agent/src/thread/types.rs:8`
- **现象**：`meta_from_row` 14 个参数；`pub type ThreadId = String` 失去编译期类型安全
- **期望**：用 struct 一次性解构；改为 `struct ThreadId(String)` newtype
- **估算**：1 天 + 30 分钟
- **来源**：2026-07-08 maintainability #8-9

#### L12：`dispatch_tools` unwrap_or_else 掩盖不变量 + `compact` 空壳目录 + `Reasoning` 职责过载

- **文件**：`peri-agent/src/agent/stages/tool_dispatch.rs:106`、`peri-agent/src/agent/compact/mod.rs`、`peri-agent/src/agent/react.rs:131-146`
- **现象**：`source_message.clone().unwrap_or_else(...)` 提供虚假 fallback；`compact/mod.rs` 空壳仅导出 `CompactConfig`；`Reasoning` 同时包含业务数据、追踪元数据和桥接数据
- **期望**：`unwrap_or_else` → `expect()` 暴露不变量；删除 `compact/` 空壳；拆分 `ReasoningOutput` + `ReasoningMetadata`
- **估算**：30 分钟 + 1 天 + 1 天
- **来源**：2026-07-08 code-quality #6 + architecture #5-6

#### L13：`AgentError` 不实现 Clone / `RedactedThinking` 缺少显式变体

- **文件**：`peri-agent/src/error.rs`、`peri-agent/src/messages/content.rs`
- **现象**：测试中需手动 `clone_error()`；`RedactedThinking` 用 `Unknown(b.clone())` 魔法字符串透传
- **期望**：`AgentError` derive Clone；新增 `RedactedThinking { data: String }` 变体
- **估算**：30 分钟 + 1 天
- **来源**：2026-07-08 code-quality #7-8

---

## 涉及文件总览

| 文件 | 相关子项 |
|------|---------|
| `peri-agent/src/agent/events_v2_mapper.rs` | H1, H2 |
| `peri-agent/src/agent/events_v2.rs` | M5, L7 |
| `peri-agent/src/agent/stages/mod.rs` | H2, L1, L3 |
| `peri-agent/src/agent/stages/act.rs` | H2 |
| `peri-agent/src/agent/stages/end.rs` | H2 |
| `peri-agent/src/agent/stages/receive.rs` | H2 |
| `peri-agent/src/agent/stages/compact.rs` | H2 |
| `peri-agent/src/agent/stages/reason.rs` | H2 |
| `peri-agent/src/agent/stages/middleware_runner.rs` | H2 |
| `peri-agent/src/agent/stages/tool_dispatch.rs` | L12 |
| `peri-agent/src/agent/agent_context.rs` | M2 |
| `peri-agent/src/agent/compact_v2.rs` | L4 |
| `peri-agent/src/agent/compact/config.rs` | L10 |
| `peri-agent/src/agent/compact/mod.rs` | L12 |
| `peri-agent/src/agent/react.rs` | L12 |
| `peri-agent/src/llm/retry.rs` | M1 |
| `peri-agent/src/llm/anthropic/invoke.rs` | H3 |
| `peri-agent/src/llm/openai/invoke.rs` | H4, H5 |
| `peri-agent/src/middleware/trait.rs` | M3 |
| `peri-agent/src/middleware/chain.rs` | M3, M4 |
| `peri-agent/src/middleware/state.rs` | L5 |
| `peri-agent/src/tools/mod.rs` | L6 |
| `peri-agent/src/interaction/channel_broker.rs` | L7 |
| `peri-agent/src/group/mod.rs` | L2 |
| `peri-agent/src/thread/sqlite_store.rs` | M5, L11 |
| `peri-agent/src/thread/sqlite_store_test.rs` | L9 |
| `peri-agent/src/thread/types.rs` | L11 |
| `peri-agent/src/error.rs` | L13 |
| `peri-agent/src/messages/content.rs` | L13 |
| `peri-agent/src/lib.rs` | L8 |
| `peri-agent/Cargo.toml` | M7 |

---

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-09 | — | Open | agent | 合并 2026-07-08 三项 Partial issue + 2026-07-09 ultra-batch 新发现 |

## 修复记录

（待后续 fix-issue 或 issue-verify 追加）
