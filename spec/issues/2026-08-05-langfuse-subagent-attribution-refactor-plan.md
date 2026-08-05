# Refactor Plan: Langfuse subagent 内容归属 —— 身份注册表替代 LIFO 栈

**状态**：Open（plan 阶段）
**创建日期**：2026-08-05
**关联 issue**：`2026-08-05-langfuse-subagent-attribution-stack-lifetime.md`（根因与设计定稿）

## Problem Statement

Langfuse 上报中，subagent 的 ReAct 执行内容（stage span、LLM generation、tool observation）全部错挂到主 agent 的 `agent-run` 下，subagent 的 AGENT observation 是 17ms/19ms 空壳。根因（三路调查 + advisor 确认）：归属依赖无身份的 `SubagentStack` LIFO 栈顶近似，栈顶 `has_started` 标志被无关 agent 事件污染，主/subagent 两个独立 forwarder task 消费顺序无保证，导致栈在 subagent 内容产生前被弹出。补丁式时序调整不可行，需以"身份注册表"重构归属机制。

## Solution

以两张身份注册表替代 LIFO 栈，归属完全按事件侧 agent_id 路由：

1. **`subagents_by_agent_id: HashMap<事件侧AgentId, ActiveSubagent>`** —— stage/generation/tool 内容的 parent 归属
2. **`subagent_invocations: HashMap<(父AgentId, ToolCallId), SubagentInvocation>`** —— Agent 工具调用、ToolBatch、deferred_output、child 绑定

生命周期由 `ObserveEvent::SubagentStart`（创建 AGENT obs）与 `SubagentStop`（关闭）驱动；`ToolEnded` 不再关闭 subagent；`on_turn_end` 仅作异常兜底。事件乱序经"注册闸门"有界缓存 + parent-first 重放，未知/丢失一律进入 incomplete 诊断分支，禁止静默挂主 agent。

用户决策（2026-08-05 采访）：补发 v2 事件（非消费 v1）；**完全删除 SubagentStack**（registry 全量接管，workflow 路径 fallback 主 agent——已确认 workflow 无 subagent 场景）；分 3 阶段实施；自动测试 + 真机 E2E 验收。

## Commits

### 阶段 ①：事件身份打通（emit + 身份键统一）

**C1. 统一 subagent 身份键**
subagent session 的 `agent_id`（`v2_bridge.rs:126` `AgentId::new()`）与 `child_thread_id`（`SubagentStarted.instance_id`）当前是独立生成的随机 UUID。验证两者关系并统一（或建立双向映射），确保 v2 `SubagentStart.child_agent_id` 与后续内容事件携带的 agent_id 是同一值。这一步不改上报行为，只补测试锁定身份契约。

**C2. 生产补发 `ObserveEvent::SubagentStart`**
在现有 `ExecutorEvent::SubagentStarted` 的 emit 点（fork 路径、bg 路径、spawner 构造处）旁，经 event_bus 补发 v2 `SubagentStart`，携带 `child_agent_id`（= 统一后的 session AgentId）、父 `agent_id`、`agent_name`、`is_background`；若工具上下文可拿到 `tool_call_id` 则一并携带（供 invocation join）。事件字段复用 `events_v2.rs:342-357` 现有定义，不新增必填字段。序列化/反序列化契约测试。

**C3. 生产补发 `ObserveEvent::SubagentStop`**
在 child `run_react_loop` 的 success/error/cancel 所有退出路径（finally/RAII）补发 `SubagentStop`，携带 `result`、`is_error`、child/parent agent_id。与 C2 配对，测试每个退出路径恰好一次。

**C4. bridge 消费 Start/Stop（最小接入）**
bridge 处理 `SubagentStart/Stop`：维护最小身份注册/注销（此阶段仅验证事件到达与字段完整，暂不影响归属；归属切换在阶段 ②）。副作用：tracing 日志 + 计数指标，供阶段 ② 对照。

### 阶段 ②：registry + 路由改造（归属切换）

**C5. 引入身份注册表，替换 SubagentStack 内部实现**
新增 registry 数据结构（两张表 + 注册闸门缓存），`SubagentStack` 的公开方法迁移到 registry 语义；tracer 内部切换到新结构。此提交保持外部行为不变（仍是旧归属逻辑），先跑绿现有测试再动路由。

**C6. stage/generation 归属按事件 agent_id 查 registry**
`StageStarted` 分支的 parent 从 `current_agent_id()`（栈顶）改为按事件 agent_id 查 `subagents_by_agent_id`；未知 agent 走注册闸门缓存或 incomplete 分支，不再 fallback 主 agent。generation parent 从该 agent 的 active stage 取得，禁止降级主 agent。

**C7. tool 路由按 owner agent_id + tool_call_id**
`on_tool_start/end` 的非 Agent 工具路由到该 agent 自己的 ToolBatch；Agent 工具经 `subagent_invocations` 关联 child；移除"所有层 ToolBatch 搜索 tool_call_id"（`is_agent_tool_anywhere`）的归属判定。

**C8. 生命周期迁移**
`on_tool_end` 的 Agent 分支只结束父工具记录、更新 invocation，不再 `end_subagent` 创建 AGENT obs；AGENT obs 的创建/关闭改由 `SubagentStart/Stop` 驱动（start=Start 时刻、end=Stop 时刻）；`on_turn_end` 降级为仅清理未收 Stop 的活跃条目（incomplete 标记）。17ms 空壳场景在单元测试中显式断言不再出现。

**C9. 重写 subagent 状态机测试**
按新语义重写：8 步事件流、注册闸门乱序缓存与重放、未知 agent_id / 缺失 Start / 重复 Start/Stop / 缓存溢出 → incomplete、parent 冻结与防环、ToolEnded 不关 child。删除与旧栈实现绑定的测试。

**C10. 删除 SubagentStack 与过时测试**
移除旧栈代码、`mark_top_started`/`top_has_started`/`current_agent_id` 等栈顶近似 API 及绑定测试。全仓编译 + 既有 tracer 测试回归。

### 阶段 ③：bridge 集成测试 + 真机验证

**C11. bridge 集成测试框架**
双 event producer（可控顺序）模拟主/subagent forwarder + 共享 tracer 锁，覆盖所有跨 task 乱序排列（不依赖 yield_now）。断言观测图：child stage 的 parent 链为该 child AGENT、child LLM/tool 不指向主 agent-run、child AGENT `start ≤ 最早 child 事件` 且 `end ≥ 最晚 child 事件`、每 obs 至多一个 parent、图无环、主 agent 既有结构不变。

**C12. 乱序场景矩阵**
Start 先/后于父 ToolStart、Stop 先/后于 ToolEnded、Start 丢失、Stop 丢失、缓存溢出 → 对应降级断言。

**C13. 全仓回归 + 真机 E2E**
`cargo test -p peri-acp`、`cargo clippy --workspace --all-targets -- -D warnings`；跑真实顺序 + 并行 subagent 场景，拉 trace 用数据断言（AGENT 子树非空、活跃期 stage 不再 100% 指向主 agent-run、无 17-19ms 空壳 + 首 stage 后置关系、无残留事件回退主 agent）。

## Decision Document

- **模块**：`peri-acp` langfuse tracer + bridge；`peri-agent` ObserveEvent emit；`peri-middlewares` subagent spawn/fork/bg 工具
- **接口变更**：`ObserveEvent::SubagentStart/Stop` 从仅测试定义变为生产 emit；bridge 新增 Start/Stop 处理分支；tracer 内部 registry 取代 SubagentStack（公开 API 收敛，v1 回退 `"main"` 语义保留）
- **架构决策**：归属 = 事件 agent_id → 注册表查表，禁止任何"栈顶/当前活跃"近似；AGENT obs 生命周期 = Start/Stop 事件，ToolEnded 不关 child；`on_turn_end` 仅兜底；注册闸门处理乱序；incomplete 分支替代静默 fallback
- **身份键**：subagent session AgentId 为唯一关联键（C1 统一 child_thread_id，或建立映射表）
- **v1/workflow 路径**：不产生 subagent，维持现有主 agent fallback，不做额外兼容
- **已知取舍**：Start/Stop 事件经 observe 通道（有界 mpsc + try_send），满时丢失 —— 丢失进入 incomplete 分支并以指标暴露（不静默挂主 agent）；不为此引入可靠投递

## Testing Decisions

- **好的测试**：只断言外部行为（观测图结构、parent 链、生命周期时间窗口、incomplete 分类），不测内部实现结构
- **测试模块**：registry 状态机（tracer 层，替代 `subagent_test.rs`）；bridge 双 producer 集成（新增，`bridge.rs` 目前零测试）；tracer 归属回归（`tracer_test.rs` 风格）
- **Prior art**：`peri-acp/src/langfuse/tracer/subagent_test.rs`（现有状态机测试风格）、`tracer_test.rs`（tracer 集成风格）；事件契约测试参照 `events_v2_test.rs`
- **真机验证**：E2E 用 trace 数据断言（C13 指标），不依赖人工 UI 目测

## Out of Scope

- v1/workflow 路径的 subagent 支持（已确认无此场景）
- TUI 的 subagent 展示逻辑（`child_thread_id` 关联已工作，不动）
- Langfuse UI 渲染/树构建
- 上报性能优化（Mutex 粒度、批量提交）
- Start/Stop 可靠投递（重试/持久化）

## Further Notes

- 阶段 ① 的 C1 是关键路径：若 session AgentId 无法与 child_thread_id 统一，需在 spawner 层建立 `child_thread_id → session AgentId` 映射并在 SubagentStart 中透传
- bridge 的 `active_stage`（HashMap<agent_id, StageHandle>）与 generation `(agent_id, step)` 隔离是 08-03 已正确的部分，重构不得回退
- 真机 E2E 需在本地 langfuse（localhost:23332）可用时进行，用 `curl /api/public/observations?traceId=...` 拉数据断言
