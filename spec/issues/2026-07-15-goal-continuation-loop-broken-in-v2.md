# Goal 自驱续跑在 v2 架构下完全断裂

**状态**：Pending
**优先级**：高
**创建日期**：2026-07-15

## 问题描述

agent 正确调用了 `goal(create)` 创建 goal，但一轮后就停止，不再自动续跑。agent 也能正确发现并调用 `goal(complete)` / `goal(block)`，但 peri 没有帮大模型持续运行——goal 的自驱续跑循环完全失效。

两处断裂同时导致：(1) goal active 后 agent 一轮退出；(2) 即使 terminal 转换成功（complete/block），loop 也不处理结果。

## 症状详情

### 现象 1：goal active 后一轮退出

| 步骤 | 期望行为 | 实际行为 |
|------|----------|----------|
| agent 调用 `goal(create)` | goal 创建成功，状态：Active | goal 创建成功，状态：Active |
| 第一轮完成后 | GoalMiddleware 注入 steering 后触发续跑，agent 自驱进入第二轮 | agent 退出，loading 停止，等待用户输入 |
| steering 消息 | 被消费并唤醒新 turn | steering 被 Receive 消费写入 transcript，但 End 阶段不唤醒循环 |

**关键表现**：steering 消息在 Receive 阶段被正常消费（`MessageKind::Info` → Receive 排空），但 End 阶段 `drain_for_end` 中 Info 类型永不唤醒循环（`queue.rs:35` 注释："仅 Receive 消费，永不唤醒循环"）。`should_continue = false`，`run_react_loop` 返回 `LoopResult::Completed`。

### 现象 2：block_continue 被静默丢弃

| 代码位置 | 操作 | 结果 |
|----------|------|------|
| `goal_middleware.rs:124` | `output.block_continue = Some("goal_active")` | block_continue 正确设置 |
| `act.rs:119` | `let output_after = run_after_agent(ctx, output).await?;` | after_agent 返回完整 AgentOutput（含 block_continue） |
| `act.rs:135` | `final_answer: Some(output_after.text)` | **仅提取 .text，block_continue 被丢弃** |

`ActOutput` 结构体没有 `block_continue` 字段，GoalMiddleware 设置的续跑信号在 act 阶段被吞掉。

### 现象 3：complete/block 后状态转换无法驱动续跑

agent 正确调用了 `goal(complete)` / `goal(block)`：
- `GoalController` 上状态转换正确执行（Active → Complete/Blocked）
- 但 agent 不会因为状态转换为终态而自动进入新 turn 处理结果
- （注：complete 调用本身的验证逻辑、状态转换逻辑均正常，仅续跑循环失效）

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. agent 调用 `goal(create, objective="xxx")`
  2. goal 创建成功，状态为 Active
  3. agent 完成第一轮回答
  4. 观察到：agent 退出而非自动续跑
- **环境**：v2 架构（`build_and_execute_agent_v2` → `run_react_loop` 路径）

## 涉及文件

- `peri-middlewares/src/goal_middleware.rs:109-124` —— GoalMiddleware 用 `MessageKind::Info` 注入 steering + 设置 block_continue
- `peri-agent/src/agent/stages/act.rs:119-135` —— block_continue 被丢弃（仅提取 .text）
- `peri-agent/src/session/queue.rs:29-43` —— `MessageKind::Info` 定义：永不唤醒循环
- `peri-agent/src/agent/stages/end.rs:14-40` —— `drain_for_end` 仅消费 Prompt/Defer，Info 保留
- `peri-acp/src/session/executor_helpers.rs:722` —— `run_react_loop` 被调用一次，无 block_continue 续跑检测

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-15 | — | Open | agent | 创建 |
| 2026-07-15 | Open | Pending | agent | 修复完成，等待用户验证 |
| 2026-07-15 | Pending | Verified | agent | 后端续跑通过，TUI UX 未通过 |
| 2026-07-15 | Verified | Reopened | agent | 用户反馈 TUI 无感知，见补充问题 |
| 2026-07-15 | Reopened | Fixed | agent | 补充修复完成，见修复 #2 |

## 修复记录

### 修复 #1（2026-07-15）

- **操作人**：agent
- **用户原意**：goal 自驱续跑在 v2 架构下完全失效——agent 调用 `goal(create)` 后一轮就退出，不自动继续；`goal(complete)` / `goal(block)` 也无法驱动后续处理
- **修复内容**：`goal_middleware.rs` 将 steering 注入由 `MessageKind::Info` 改为 `MessageKind::Defer`。Info 在 `drain_for_end` 中永远不唤醒循环，Defer 则原生触发 `should_continue=true` 实现自驱续跑。complete/block 后 goal 进入终态不 push Defer，循环自然退出。同步更新 2 处单元测试（`drain_for_receive` → `drain_for_end`）。
- **涉及文件**：
  - `peri-middlewares/src/goal_middleware.rs`（1 行代码改动 + 文档注释）
  - `peri-middlewares/src/goal_middleware_test.rs`（2 处测试更新）
- **验证状态**：✅ 已验证通过

### 修复 #2（2026-07-15）— TUI 消息可见性 + 迭代边界刷新

- **操作人**：agent（plan + coder sub-agents）
- **根因**：`stages/mod.rs` 中 `SyntheticUserMessage` emit 条件仅匹配 `MessageSource::SubAgentComplete`，GoalSteering/Cron/Workflow 等 Defer 消息写入 transcript 但不发射 TUI 事件。同时 `TurnCommitted` handler 是纯 no-op，自驱迭代边界不刷新 TUI atom。
- **修复内容**：
  1. `stages/mod.rs` 两处 emit 点（End 阶段 + post-wake drain）将条件从 `msg.source == MessageSource::SubAgentComplete` 改为 `msg.kind == MessageKind::Defer`，覆盖所有 Defer 来源
  2. `acp_events.rs` `TurnCommitted` handler 新增 `push_view_models(state)` + `push_acp_state(state)` 作为 ReAct 迭代边界刷新检查点
- **设计原理**：executor_helpers.rs 中 `MessageAdded → push_unstable_event + session/update` 双通道机制是 source-agnostic 的，只要 agent 端 emit `SyntheticUserMessage`，整个 BgCallbackBubble flush + LocalUserBubble push 链路自动运转
- **涉及文件**：
  - `peri-agent/src/agent/stages/mod.rs`（两处条件改动 + 注释更新）
  - `peri-tui/src/kit/acp_events.rs`（TurnCommitted 新增 2 行刷新调用）
- **验证状态**：待验证（1433 tests pass: peri-agent 616 + peri-tui 496 + peri-acp 321）

## 手动验证记录

### 2026-07-15 验证测试

**测试场景**：agent 调用 `goal(create)` 创建"数到 10"的目标，每轮数一个数字后等待 goal 唤醒。

**测试过程**：

| 轮次 | 操作 | 结果 |
|------|------|------|
| 1 | agent 调用 `goal(create)` | ✅ goal 创建成功，状态 Active |
| 1 结束 | agent 回复"1"，停止 | ✅ 第一轮正常结束 |
| 2 开始 | `<goal-message>` 注入到下一轮 | ✅ steering 消息通过 Defer 触发续跑，agent 自动进入第二轮 |
| 2 | agent 回复"2" | ✅ 自驱续跑生效 |
| 3 | 用户中断测试 | agent 调用 `goal(block)`，目标正常中断 |

**核心结论**：
- ✅ **现象 1 已修复**：`MessageKind::Info` → `MessageKind::Defer` 后，goal active 状态成功触发自驱续跑，agent 不会一轮退出
- ✅ **现象 3 已修复**：`goal(block)` 后 goal 进入终态，循环自然退出（不再推送 Defer）
- ❌ **TUI UX 断裂**：Defer 触发了后端续跑，但 TUI 前端用户完全无感知——loading 指示器停止后用户以为 agent 已完成，手动输入打破了 goal 循环。见下方补充问题。

## 补充问题：Goal 自驱续跑对 TUI 用户不可见

**严重程度**：高（后端修好了等于没修好，用户体感和现象 1 一致）

**复现**：
1. agent 调用 `goal(create)`，进入自驱模式
2. 第一轮结束，loading 停止
3. Defer 在后台触发新 turn，react loop 继续运行
4. **但 TUI 没有恢复 loading 状态**，用户看到的是"agent 已完成，等待输入"
5. 用户手动输入内容 → 用户消息插入到 goal steering 消息中间 → goal 流程被打断

**根因推测**：Defer 触发 `should_continue=true` 后 `run_react_loop` 进入下一轮迭代，但 TUI 的 `BRIDGE_STATE` / `VIEW_MODELS` 没有收到新的 turn 开始事件。可能是 `ExecutorEvent::TurnStarted` 在 Defer 续跑时未正确发射，或者 ACP bridge 没有把新 turn 的 loading 状态同步到 TUI。

**涉及文件**（待排查）：
- `peri-acp/src/session/executor_helpers.rs` —— `run_react_loop` 调用点，新 turn 事件发射
- `peri-tui/src/kit/acp_bridge.rs` —— bridge 状态同步
- `peri-tui/src/kit/acp_events.rs` —— `push_view_models` 事件映射
