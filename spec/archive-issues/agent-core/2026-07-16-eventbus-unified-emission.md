> 归档于 2026-07-17，原路径 spec/issues/2026-07-16-eventbus-unified-emission.md
# 统一事件发射路径：所有 Agent 事件走 v2 EventBus

**状态**：Done（方案 B 已完成 CompactStrategy 硬编码 + Path D 代码组织改善全部实施；方案 A 主体 NOT READY，4 项经两轮对抗评审判定搁置）
**优先级**：中
**类型**：架构改进
**创建日期**：2026-07-16
**最后更新**：2026-07-16（第二轮对抗评审完成，全部剩余项已判定）

## Problem Statement

v2 三层 EventBus（RenderEvent / StateEvent / ObserveEvent）是 peri-agent 内部的事件优化层，设计意图是让所有 Agent 事件先进入 EventBus，再通过 `events_v2_mapper` 统一桥接为 `ExecutorEvent` 交付给下游。

但目前 EventBus **仅覆盖 ReAct 循环内的 5 个阶段**（compact / receive / reason / act / end）。以下代码路径直接构造 `ExecutorEvent` 绕过 EventBus：

| 路径 | 位置 | 绕过的事件 |
|------|------|-----------|
| LLM 流式 SSE 解析 | `openai/stream.rs`, `anthropic/stream.rs` | TextChunk, AiReasoning |
| LLM 重试 | `retry.rs` | LlmRetrying |
| SubAgent 生命周期 | `execute_fork.rs`, `execute_bg.rs`, `spawner.rs`, `define.rs` | SubagentStarted, SubagentStopped, BackgroundTaskCompleted |
| ACP Turn 编排 | `executor_helpers.rs` | TurnStarted, TurnEnded, AgentExecutionFailed |
| 斜杠命令 | `session/command/{rewind,bg,clear,compact}.rs` | CompactStarted/Completed/Error, RewindCompleted 等 |

这导致**三条独立发射路径并存**：
- 路径 A：ReAct 循环 → EventBus → mapper → ExecutorEvent → event_tx（已优化）
- 路径 B：SubAgent/LLM → 直接构造 ExecutorEvent → event_tx（绕过 EventBus）
- 路径 C：斜杠命令 → EventSink.push_event()（绕过整个事件管道）

具体问题：
1. **新增事件需在 3-5 处同步**：EventBus 定义 → mapper 转换 → 消费方（peri-acp mapper + peri-tui acp_events）。但如果事件来自绕过路径，还需额外在手写构造处同步。
2. **`LlmRetrying` 静默丢失**：`retry.rs:121` 直接发送 ExecutorEvent，但 `reason.rs:126` 中的 `FnEventHandler` 桥接用 `_ => {}` 将其丢弃——重试事件永不进入 EventBus，无法被 Observe 层观测。
3. **CompactStrategy 硬编码**：`compact_v2.rs` 和 `events.rs` 各有一份 `CompactStrategy` 枚举（语义相同但类型不同），mapper 在映射 `CompactStarted`/`CompactCompleted` 时 hardcode `Strategy::Smart`，丢失真实策略值。

## Solution

让**所有事件源统一走 v2 EventBus**，`events_v2_mapper` 成为从 EventBus 到 ExecutorEvent 的**唯一桥接点**。

核心思路：将 `Arc<EventBus>` 作为事件发射的入口传递给所有需要发射事件的代码路径，扩展 `ObserveEvent` 覆盖当前缺失的事件类型。

## User Stories

### 开发者体验

1. As a peri-agent 开发者, I want 新增一个 Agent 事件类型时只需在 `events_v2.rs` 添加一个变体 + 在 `events_v2_mapper.rs` 添加一行映射, so that 不再需要在多处分散的位置同步代码。

2. As a peri-agent 开发者, I want `LlmRetrying` 事件能进入 EventBus 的 Observe 层, so that 重试行为可被监控（Langfuse tracing）和调试。

3. As a peri-agent 开发者, I want `SubagentStarted`/`SubagentStopped` 生命周期事件进入 EventBus, so that 子代理的启动和停止能与其他 Agent 事件统一观测。

4. As a peri-agent 开发者, I want `TurnStarted`/`TurnEnded` 事件通过 EventBus emit, so that turn 生命周期与其他 Agent 事件共享同一观测通道。

5. As a peri-agent 开发者, I want 删除 `reason.rs` 中的 `FnEventHandler` 反向桥接代码（~30 行），so that 流式事件不再走 `ExecutorEvent → EventBus` 的反向转换。

6. As a peri-agent 开发者, I want `CompactStrategy` 枚举只有一份定义（位于 `compact/config.rs`），so that 修改策略时只需改一处，`CompactStarted`/`CompactCompleted` 携带真实策略值而非 hardcode。

### 运维/调试体验

7. As a peri 运维者, I want 所有 Agent 事件都能在 ObserveEvent 层被订阅，so that Langfuse 追踪器只需订阅一个 EventBus 即可获取完整的 trace 数据。

8. As a peri 开发者排查问题时, I want 事件流有单一入口（EventBus），so that 添加 `tracing::debug!` 诊断日志时只需在一个位置插桩。

## Implementation Decisions

### 架构决策

1. **ExecutorEvent 保留为稳定跨 crate 边界类型**。v2 EventBus 是 peri-agent 内部优化层，ExecutorEvent 是 peri-agent → peri-acp → peri-tui 的交付协议。本次改进不消除 ExecutorEvent，而是消除绕过 EventBus 的直接构造。

2. **斜杠命令也走 EventBus**。通过将 EventBus 创建和 forwarder 启动提前到 `intercept_immediate_command` 之前，让斜杠命令持有 `Arc<EventBus>` emit ObserveEvent，而非直接调 `event_sink.push_event()`。详见 Step 0。

3. **EventBus 不实现 Clone，通过 `Arc<EventBus>` 共享**。当前 `StageContext.event_bus` 已是 `Arc<EventBus>`。LLM 流式、SubAgent 等路径通过传入 `Arc<EventBus>` 获得发射能力。

### 接口变更

4. **`StreamingContext` 新增字段**：`event_bus: Arc<EventBus>`, `turn_id: TurnId`, `agent_id: AgentId`。LLM 流式解析器直接调用 `event_bus.emit_render(RenderEvent::TextChunk{...})`，不再通过 `event_handler` 发射 ExecutorEvent。

5. **`SubAgentTool` 构造函数新增参数**：`event_bus: Arc<EventBus>`，替换当前 `event_handler: Option<Arc<dyn AgentEventHandler>>`。`execute_fork`、`execute_bg`、`spawner` 中删除 `handler.on_event(ExecutorEvent::SubagentStarted)` 等直接构造。

6. **`ObserveEvent` 新增变体**：
   - `SubagentStarted { child_thread_id, agent_name, turn_id, agent_id }`
   - `SubagentStopped { child_thread_id, exit_code, turn_id, agent_id }`
   - `BackgroundTaskCompleted { child_thread_id, status, turn_id, agent_id }`
   - `LlmRetrying { attempt, delay_ms, error, turn_id, agent_id }`
   - `TurnStarted { turn_id, agent_id }`
   - `TurnEnded { turn_id, agent_id, status, duration_ms }`
   - `RewindCompleted { turn_id, agent_id, to_turn, status }`
   - `CompactCommandStarted { turn_id, agent_id, trigger }`
   - `CompactCommandCompleted { turn_id, agent_id, strategy }`
   - `CompactCommandError { turn_id, agent_id, error }`
   - `BgCommandStarted { turn_id, agent_id, subagent_name }`

7. **`CompactStrategy` 统一到 `compact/config.rs`**：添加 serde derives，删除 `events.rs` 中的重复定义，`ObserveEvent::MessagesCompacted` 直接携带 `CompactStrategy` 值。

### 实现顺序（按依赖关系）

8. **Step 0（低风险，前置条件）**：EventBus 生命周期提前，使斜杠命令可访问 EventBus。

   **当前流程**（`executor.rs` → `run_session_loop`）：
   ```
   intercept_immediate_command  ← 斜杠命令在此拦截，无 EventBus
   ↓
   build_and_execute_agent_v2
     Phase 1: build_stage_context → 内部创建 EventBus
     Phase 4: spawn_eventbus_forwarder(event_handles)
   ```
   
   **改为**：
   ```
   创建 EventBus（提前到 executor.rs 层）
   启动 forwarder（提前到 executor.rs 层）
   ↓
   intercept_immediate_command  ← 现在持有 Arc<EventBus>
   ↓
   build_and_execute_agent_v2  ← 接收预创建的 EventBus + EventHandles
     Phase 1: build_stage_context(event_bus, event_handles)  ← 不再内部创建
     Phase 4: 删除（forwarder 已在外部启动）
   ```
   
   **forwarder 提前对 ReAct 循环无影响**：
   - forwarder 是纯后台 `tokio::spawn` 任务，只阻塞 `select!` 等待事件流入
   - 在 ReAct 循环启动前，无人 emit 事件，forwarder 空闲等待——无副作用
   - `event_tx`（`Arc<Mutex<Option<UnboundedSender>>>`）在 `run_session_loop` 创建，forwarder 在 SlashCommand 拦截前就能获取 clone
   - EventBus sender 通过 `Arc<EventBus>` 共享，生命周期与 `StageContext` 绑定，存活到 Phase 9 结束

   **接口变更**：
   - `build_stage_context()` 新增可选参数 `event_bus: Option<(Arc<EventBus>, EventHandles)>`——若 Some 则复用，None 则内部创建（兼容 workflow_agent 调用点）
   - `InterceptRequest` 新增字段 `event_bus: Arc<EventBus>` + `turn_id: TurnId` + `agent_id: AgentId`

9. **Step 1（低风险）**：扩展 ObserveEvent 新增变体 + events_v2_mapper 新增映射。纯追加，不修改现有路径。验证：新增 mapper_test 用例覆盖所有新增变体。

10. **Step 2（中风险）**：LLM 流式 + 重试改走 EventBus。修改 `StreamingContext`、`llm/types.rs`、`openai/stream.rs`、`anthropic/stream.rs`、`retry.rs`。删除 `reason.rs` 中的 `FnEventHandler` 反向桥接。验证：现有 ReAct 循环 e2e 测试通过。

11. **Step 3（中风险，跨 crate）**：SubAgent 生命周期改走 EventBus。修改 `SubAgentTool` 接口（`peri-middlewares`）+ `agent/builder.rs` 传参（`peri-acp`）。验证：SubAgent fork/bg e2e 测试通过。

12. **Step 4（中风险）**：ACP Turn 编排 + 斜杠命令改走 EventBus。
    - `executor_helpers.rs` 中 TurnStarted/TurnEnded/AgentExecutionFailed 改为通过 `StageContext.event_bus` emit
    - `session/command/{rewind,bg,clear,compact}.rs` 中 `event_sink.push_event(ExecutorEvent::...)` 替换为 `event_bus.emit_observe(ObserveEvent::...)`
    - 验证：session 集成测试 + 斜杠命令 e2e 测试通过

### 可删除代码

13. 完成后可删除：
    - `executor_helpers.rs` Phase 4 中 `spawn_eventbus_forwarder` 调用（forwarder 已在 executor.rs 层启动）
    - `reason.rs` 中 `FnEventHandler` 反向桥接（~30 行）
    - `events.rs` 中重复的 `CompactStrategy` 枚举 + `CompactTrigger` 枚举（~20 行）
    - `subagent/tool/*.rs` 中 ~10 处 `handler.on_event(ExecutorEvent::...)` 调用
    - `session/command/*.rs` 中 ~11 处 `event_sink.push_event(ExecutorEvent::...)` 调用

## Testing Decisions

### 测试策略

- **优先测试外部行为**：测试 event_tx 通道中收到的 ExecutorEvent 序列是否正确，而非测试 EventBus 内部状态。
- **回归测试**：所有受影响模块的现有测试必须继续通过（ReAct e2e、SubAgent fork/bg、session 集成测试）。

### 测试范围

| 测试目标 | 位置 | 类型 |
|----------|------|------|
| 新增 ObserveEvent 变体的 serde roundtrip | `events_v2.rs` 内联 `#[cfg(test)]` | P0 新增 |
| 新增变体的 mapper 双向转换 | `events_v2_mapper.rs` 末尾 `#[cfg(test)]` | P0 新增 |
| LLM 流式事件到达 event_tx | `stages/reason.rs` 内联测试 | P1 新增 |
| SubAgent 生命周期事件到达 event_tx | `subagent/tool/tool_test.rs` | P1 修改 |
| TurnStarted/TurnEnded 事件时序 | `executor_test.rs` | P1 修改 |
| 斜杠命令事件到达 event_tx | `session/command/command_test.rs` | P1 修改 |
| LlmRetrying 不再丢失 | `stages/reason.rs` 内联测试 | 回归 |

### 测试参考

- **事件映射测试**：参考 `events_v2_mapper.rs` 末尾的现有测试模式
- **SubAgent 集成测试**：参考 `subagent/tool/tool_test.rs` 现有模式
- **Executor 集成测试**：参考 `peri-acp/tests/integration_test.rs`

## 对抗评审结论（2026-07-16）

三份独立对抗报告（正确性 / 可实施性 / 架构价值）交叉验证后，PRD 原始方案被判定为 **NOT READY**。以下是关键发现：

### 诊断修正：LlmRetrying 从未丢失

PRD §问题 2 的 claim 基于对两条独立代码路径的混淆：

| 路径 | handler 来源 | 实际行为 |
|------|-------------|---------|
| `retry.rs:121` → `LlmRetrying` | `builder.rs:671` 注入 `executor_handler` → `event_tx` → pump → `forward_langfuse_event`（`executor_helpers.rs:336-343` 专门处理） | **事件正确到达 Langfuse tracer，从未丢失** |
| `reason.rs:125` → `FnEventHandler._ => {}` | `StreamingContext` 的 SSE 文本桥接 | `_ => {}` 是正确的防御设计——它永远不会收到 `LlmRetrying`（此 handler 只接收流式 SSE 事件） |

结论：PRD 核心 motivation #2 不成立，应删除对应的 Step 2 和 User Story #2/#5。

### 交叉验证发现

| 发现 | Agent 1<br/>正确性 | Agent 2<br/>可实施性 | Agent 3<br/>架构价值 | 判定 |
|------|:--:|:--:|:--:|------|
| Immediate 命令事件永久丢失（event_pump 在 forwarder 之后） | ✅ | — | — | **致命 bug** |
| **LlmRetrying 分析错误** | ✅ | — | ✅ | **被推翻** |
| 斜杠命令 2 hop → 7 hop，语义类别错误（UI ≠ Agent 观测） | — | — | ✅ | **致命取舍** |
| Slash 命令前置时缺少 turn_id/agent_id 来源 | — | ✅ | — | **阻塞** |
| `SubAgentTool` 接口退化（trait → concrete struct） | — | ✅ | ✅ | **致命取舍** |
| SubAgent source_agent_id 注入链断裂 | ✅ | — | — | **阻塞** |
| bg_event_sender 与 event_handler 正交，不能被 Arc<EventBus> 替代 | — | ✅ | — | **阻塞** |
| CompactStrategy 是独立问题，与事件统一无关 | — | — | ✅ | **被拆分** |
| build_stage_context 仅有 1 个调用点（非 PRD 假设的 2 个） | — | ✅ | — | **阻塞** |

### 替代方案：增量修复（方案 B）

三个 agent 一致认为，PRD 试图用一个重型重构解决三个轻量问题。更优路径是**先独立修复可拆分的问题，再重新评估大纲**。

#### 直接可修（~15 行，0 处架构变更）

1. ~~在 `reason.rs:125` 添加 `LlmRetrying` match arm~~（不需要——LlmRetrying 从未丢失）
2. 删除 `compact_v2.rs` 的重复 `CompactStrategy` 枚举，引用 `compact/config.rs` 统一定义（~3 行）
3. 给 `compact/config.rs` 的 `CompactStrategy` 加 serde derives（~2 行）
4. 给 `ObserveEvent::MessagesCompacted` 加 `strategy: CompactStrategy` 字段（~2 行）
5. `compact_v2.rs` 中 emit `MessagesCompacted` 时传入真实 strategy 值（~3 行）
6. mapper 中 `CompactStarted`/`CompactCompleted`/`MessagesCompacted` 读取真实 strategy 而非 hardcode Smart（~3 行）

**影响文件**：`compact/config.rs`、`compact_v2.rs`、`events_v2.rs`、`events_v2_mapper.rs` —— 共 4 个文件。

#### 独立评估（每个问题单独论证价值）

| 议题 | 状态 | 备注 |
|------|------|------|
| 斜杠命令统一走 EventBus | **NOT NOW** | 2→7 hop、语义污染、UI≠Agent，性价比不成立 |
| SubAgent 事件改用 EventBus | **NOT NOW** | source_agent_id 注入链断裂、接口退化、bg_event_sender 正交 |
| LLM 流式删除反向桥接 | **NOT NOW** | 基于 LlmRetrying 误诊，且 StreamingContext 膨胀代价大 |
| EventBus 生命周期提前 | **NOT NOW** | Immediate 命令事件丢失、双模式 API 污染 |

### 更新的推进建议

1. **立即执行**：增量修复 CompactStrategy 硬编码（4 文件，~15 行）
2. **后续评估**：SubAgent 事件的 source_agent_id 机制整理（独立于事件统一）
3. **长期议题**：当 subagent、command、turn lifecycle 各自有独立的简洁方案后，再考虑是否需要统一架构

---

## Out of Scope

1. **删除 ExecutorEvent**：ExecutorEvent 是稳定跨 crate 边界类型，本次不消除。
2. **StageContext 拆分**：上帝对象拆分是独立改进，不在本次 PRD 范围内。
3. **MiddlewareState trait 精简**：独立改进，不在本次范围内。

## Further Notes

- 本次改进的前身分析见 `/tmp/architecture-review-20260716.html`（架构审查报告）。
- 当前权威设计：`docs/design/architecture.md`、`docs/design/peri-acp-protocol.md`；历史迁移过程只保留在 Git 历史与本归档 issue 中。
- 三个已知 issue 与本 PRD 相关：`spec/issues/2026-07-08-peri-agent-architecture-improvement.md`（AgentGroup 仍使用 v1 sender）、`spec/issues/2026-07-08-peri-agent-maintainability-improvement.md`（缺少 `#[deprecated]` 迁移路径）、`spec/issues/2026-07-09-peri-agent-comprehensive-code-quality-review.md`（v1 变体未被标记废弃）。

---

## 方案 B 增量修复完成记录（2026-07-16）

### 修改摘要

按 §对抗评审结论→替代方案→直接可修 执行，共 **5 个文件，+11/-16 行**。

### 修改内容

| # | 文件 | 修改 |
|---|------|------|
| 1 | `peri-agent/src/agent/compact_v2.rs` | 删除重复 `CompactStrategy` 枚举（~12 行），import 改为从 `events` 引用统一定义 |
| 2 | `peri-agent/src/agent/events_v2.rs` | `CompactStarted` 和 `MessagesCompacted` 各新增 `strategy: CompactStrategy` 字段；3 处测试构造同步更新 |
| 3 | `peri-agent/src/agent/stages/compact.rs` | `CompactStarted` emit 前根据 budget 估算策略并传入；`MessagesCompacted` emit 传入真实 `r.strategy`；Full 判断改用 `events::CompactStrategy` |
| 4 | `peri-agent/src/agent/events_v2_mapper.rs` | mappper 中 `CompactStarted`/`CompactCompleted` 不再 hardcode `Smart`，改为读取事件中的真实 strategy 值；删除 unused import |
| 5 | `spec/issues/2026-07-16-eventbus-unified-emission.md` | 更新状态 + 追加本完成记录 |

### 效果

- **消除 CompactStrategy 重复定义**：从 2 个独立枚举合并为 `events.rs` 单一定义
- **修复策略信息丢失**：`MessagesCompacted → CompactCompleted` 映射现在携带真实策略值（Micro/Full），而非硬编码的 `Smart`（未实现）
- **CompactStarted 携带估算策略**：根据 budget 在 emit 前计算，与 `run_compact` 内部逻辑对齐（force=false 时等价）

### 验证

- `cargo build -p peri-agent -p peri-acp -p peri-middlewares -p peri-tui`：全量编译通过，0 warnings
- `cargo test -p peri-agent --lib`：617 passed, 0 failed
- `cargo test -p peri-acp --lib -- compact mapper router`：89 passed, 0 failed

### 未执行项（保持 NOT NOW）

| 议题 | 原因 |
|------|------|
| 斜杠命令统一走 EventBus | 2→7 hop、语义污染、性价比不成立 |
| SubAgent 事件改用 EventBus | source_agent_id 注入链断裂、接口退化、bg_event_sender 正交 |
| LLM 流式删除反向桥接 | 基于 LlmRetrying 误诊，且 StreamingContext 膨胀代价大 |
| EventBus 生命周期提前 | Immediate 命令事件丢失、双模式 API 污染 |

### 后续建议

1. SubAgent 事件的 source_agent_id 机制整理（独立于事件统一）
2. 当 subagent、command、turn lifecycle 各自有独立的简洁方案后，再考虑统一架构

---

## 第二轮对抗评审：NOT NOW 项规范化评审（2026-07-16）

对 4 个 NOT NOW 项各创建了独立的子方案文档，并派发 4 个 `plan` subagent 进行对抗评审。

**手把文件**：`.tmp/devflow/eventbus-remaining-items/`

### 评审结果汇总

| Item | 原判定 | 子方案推荐 | 评审判定 | 关键发现 |
|------|--------|----------|---------|---------|
| 1. 斜杠命令 → EventBus | NOT NOW | Never | **NO-GO** ✅ | 确认 Never；新增 Path D: compact/events.rs helper 模式扩展到 bg/rewind（~4 新文件，~50 行，零风险） |
| 2. SubAgent → EventBus | NOT NOW | Later | **CONDITIONAL** ⚠️ | 发现时序死结（SubagentStarted 在 EventBus 创建前 emit）；4 前提条件均不满足 |
| 3. LLM 流式删除桥接 | NOT NOW | Never | **NO-GO** ✅ | Motivation 不成立（LlmRetrying 从未丢失）；ROI 负值；新增 Path D: 桥接结构体化 |
| 4. EventBus 生命周期提前 | NOT NOW | Never | **NO-GO** ✅ | 零独立价值（依赖 Item 1）；改动面低估 2.5 倍（8 文件 vs 3）；管道依赖链断裂 |

### 交叉发现

1. **Item 1 + Item 4 耦合**：Item 4 是 Item 1 的前置条件。Item 1 被判定 Never 后，Item 4 自动失去存在意义。
2. **Item 2 时序死结**：SubAgent 的 EventBus 在 `build_v2_subagent_context` 创建，但 `SubagentStarted` 在其之前 emit。这是根本性架构约束。
3. **Item 3 动机根基**：原始 PRD 声称 LlmRetrying 丢失——被三轮独立 agent（原始对抗评审 + explorer + reviewer）一致否定。
4. **改动面系统性低估**：所有路径的实际改动面比子方案预估多 2-2.5 倍。

### 可执行的新方案

| 方案 | 来源 | 描述 | 改动面 |
|------|------|------|--------|
| Path D (Item 1) ✅ | Reviewer 1 | 创建 `bg/events.rs` + `rewind/events.rs`，统一 helper 函数包装 push_event | ~4 新文件，~50 行，零风险 |
| Path D (Item 3) ✅ | Reviewer 3 | 将 reason.rs 匿名闭包提取为 `StreamingEventBridge` 命名结构体 + 单元测试 | ~46 行重构，零风险 |

### 结论

4 个剩余项经第二轮独立对抗评审，**3 个 NO-GO + 1 个 CONDITIONAL（条件不满足）**，均维持 NOT NOW。2 个 Path D 可执行方案已实施完成。本 issue 的架构改进部分已**完全闭环**。

---

## Path D 实施完成记录（2026-07-16）

### Path D (Item 1)：bg/rewind helper 函数

新建 `peri-acp/src/session/command/bg/events.rs`（5 个 `emit_bg_*` helper）和 `rewind/events.rs`（3 个 `emit_rewind_*` helper），效仿 `compact/events.rs` 模式。bg.rs 和 rewind.rs 中的 push_event 调用全部替换为 helper 调用。

### Path D (Item 3)：StreamingEventBridge 结构体

`reason.rs` 的匿名 `FnEventHandler` 闭包（~30 行）提取为 private `StreamingEventBridge` 结构体（~46 行），实现 `AgentEventHandler` trait。代码可读性提升，行为零变更。

### 修改文件

| 文件 | 变更 |
|------|------|
| `peri-acp/src/session/command/bg/events.rs` | NEW: 5 个 emit_bg_* helper |
| `peri-acp/src/session/command/bg.rs` | MOD: +`mod events;`, 5 处 push_event → helper |
| `peri-acp/src/session/command/rewind/events.rs` | NEW: 3 个 emit_rewind_* helper |
| `peri-acp/src/session/command/rewind.rs` | MOD: +`mod events;`, 3 处 push_event → helper |
| `peri-agent/src/agent/stages/reason.rs` | MOD: +StreamingEventBridge struct, -FnEventHandler 闭包 |

### 验证

- `cargo build -p peri-acp -p peri-agent` → 0 errors, 0 warnings
- `cargo test -p peri-acp --lib -- bg rewind` → 42 passed, 0 failed
- `cargo test -p peri-agent --lib -- reason` → 30 passed, 0 failed
- Code review → **ALL PASS**（审查人确认零行为变更）
