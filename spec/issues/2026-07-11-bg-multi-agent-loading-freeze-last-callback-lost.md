# 单轮内多次 bg agent 完成后 loading 卡死 + 最后一个 callback 消息丢失

**状态**：Open
**优先级**：高
**类型**：Bug
**创建日期**：2026-07-11

## 问题描述

当 AI 在**一次回复中连续调用多个 Agent 工具**（`run_in_background: true`），即同一轮 ReAct 迭代中启动多个 bg agent 后，所有 bg agent 顺序执行并完成。此时出现两个异常：

1. **loading spinner 永久不退出**：所有 bg agent 都完成后，loading 一直转，不会停止
2. **最后一个 bg agent 的 callback 消息丢失**：前面几个 bg 的 callback 消息（如「[后台任务 bg-xxx 已完成] 输出：...」）正常出现在消息区，但最后一个 callback 消息完全不显示

**与已有双通道 flush-then-push 修复的关系**：bg callback 气泡的五次修复（2026-07-09 最终方案 `flush-then-push`）解决的是**单个 bg agent** 的场景。本 issue 描述的是**多个 bg agent 在同一轮 ReAct 中被调用**的新场景——大概率与 flush/push 在多次触发时的时序和状态管理有关。

## 症状详情

| 观察点 | 期望行为 | 实际行为 |
|--------|---------|---------|
| 前 N-1 个 bg callback 消息 | 正常显示「[后台任务 bg-xxx 已完成]」气泡 | ✅ 正常显示 |
| 第 N 个（最后一个）bg callback 消息 | 应正常显示同款气泡 | ❌ **完全不显示** |
| 所有 bg 完成后 loading 状态 | loading spinner 应停止，输入框恢复可用 | ❌ **loading 一直转** |
| 卡死期间的交互 | N/A | 输入框**仍可正常输入**新消息（TUI 未完全冻结） |

**关键现象信号**：
- **callback 消息不是全丢，只丢最后一个**：说明整体链路大部分是通的，问题出在多实例场景下的终结/清理逻辑
- **loading 不退出但输入框可用**：说明事件泵和 TUI 主循环仍存活，loading 的清除条件/事件未触发
- **复现条件是单轮内多个 bg agent**：单 bg agent 场景已验证正常（五次修复后的行为）

## 复现条件

- **复现频率**：必现（每次 AI 在同一回复中调用 ≥2 个 Agent 工具 background 模式时触发）
- **触发步骤**：
  1. 启动 Peri TUI（`cargo run -p peri-tui -- -a`）
  2. 让主 agent 在**一次回复**中连续调用多个 Agent 工具（`run_in_background: true`）
     - 例如说：「帮我后台搜索一下 Rust async，同时后台再查一下 tokio spawn blocking」
  3. 观察 AI 回复中包含了多次 Agent 工具调用（≥2 次）
  4. 等待所有 bg agent 完成（状态栏计数归零）
  5. **观察**：
     - 前 N-1 个 bg callback 消息正常出现在消息区
     - 最后一个 bg callback 消息**不出现**
     - loading spinner**一直旋转**不停止
  6. 输入框仍可正常输入，可继续发下一条消息（但 loading 仍不消失）
- **环境**：macOS，任意模型

## 与已有 issue 的关系

| Issue | 状态 | 关联说明 |
|-------|------|---------|
| `2026-07-07-bg-agent-complete-no-resume.md` | Open | 描述单个 bg agent 完成后合成消息不出现 + loading 卡死。本 issue 是**多个 bg agent** 场景下的扩展 |
| `2026-07-08-mq-injected-user-message-not-in-tui.md` | Open | 描述 MQ 注入消息不通过 ACP 反馈到 TUI。本 issue 中**仅最后一个**消息丢失，前几个正常——提示问题不在 MQ 链路本身，而在多实例的衔接/清理 |
| `2026-07-09-bg-agent-loading-never-stops-after-first-turn.md` | Fixed（已归档） | 描述单个 bg agent 首轮 loading 不停止。双通道 flush-then-push 方案已修复单个场景，但多实例场景未覆盖 |

## 涉及文件

从已有修复路径推断的关联文件（本 issue 不做诊断，仅列出供排查参考）：

- `peri-agent/src/agent/stages/mod.rs` —— ReAct 循环 End 阶段，MQ drain + `SyntheticUserMessage` emit
- `peri-agent/src/agent/events_v2.rs` / `events_v2_mapper.rs` —— `SyntheticUserMessage` 事件定义和映射
- `peri-acp/src/session/executor_helpers.rs` —— bg event pump 消费者
- `peri-tui/src/kit/acp_bridge.rs` —— BridgeState，`is_loading` / committed / current_turn 管理
- `peri-tui/src/kit/acp_events.rs` —— ACP 事件处理，loading 状态设置/清除
- `peri-tui/src/kit/submit_consumer.rs` —— BG_TASKS atom 与 TUI 交互

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-11 | — | Open | agent | 创建（issue-create skill 访谈还原现象） |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
