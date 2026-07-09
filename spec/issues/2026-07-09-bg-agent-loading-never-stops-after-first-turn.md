# Agent 工具（background 模式）启动 bg agent 后 loading 不停止

**状态**：fixed
**优先级**：高
**类型**：Bug
**创建日期**：2026-07-09

## 问题描述

当主 agent 在 ReAct 循环中调用 Agent 工具（`run_in_background: true`）启动后台 agent 后，AI 回复一句（如"Background agent bg-xxx started"），按照正常流程本轮 turn 应该结束、loading spinner 应该停止。但实际行为是 **loading 一直转**，不会在这一轮结束时停止。直到 bg agent 完成后 callback 消息唤醒主 agent 的下一轮 turn，且该轮 turn 也完全结束后，loading 才停止。

这与 `/bg` 命令不同——`/bg` 是 Immediate 命令，绕过 agent 循环直接启动 bg agent 后调用 `push_done`。本 issue 的场景是 **AI 主动调用 Agent 工具**（background 模式），走完整的 ReAct 循环路径。

**正常流程**（期望）：
```
用户输入 → ReAct 循环 → AI 调用 Agent 工具 (background)
  → bg agent 启动 → AI 回复"已启动"→ TurnDone → loading 停止
  → bg agent 后台运行...
  → bg agent 完成 → callback 唤醒 → Turn2: loading 启动 → AI 回复 → TurnDone → loading 停止
```

**实际流程**（bug）：
```
用户输入 → ReAct 循环 → AI 调用 Agent 工具 (background)
  → bg agent 启动 → AI 回复"已启动"→ loading ❌ 不停止
  → bg agent 后台运行... loading 还在转
  → bg agent 完成 → callback 唤醒 → Turn2: loading 继续 → AI 回复 → TurnDone → loading ✅ 停止
```

## 症状详情

| 时机 | 期望行为 | 实际行为 |
|------|---------|---------|
| AI 调用 Agent 工具并回复完后 | loading spinner 停止，输入框可继续输入 | ❌ loading 继续旋转 |
| bg agent 运行期间 | loading 应该停止（idle 状态） | ❌ loading 一直转 |
| bg callback 唤醒新 turn | loading 重新开始旋转（新 turn 开始） | loading 继续旋转（无"停止→重新开始"过渡） |
| callback 触发的 turn 完成 | loading 停止 | ✅ loading 停止 |

**关键观察**：loading 在 AI 调用 Agent → callback → 新 turn 完成这个完整链条中，表现为**一个连续不中断的 loading 周期**，而不是"停止→重新开始"的两段式。

## 复现条件

- **复现频率**：必现（每次 AI 调用 Agent 工具 background 模式后）
- **触发步骤**：
  1. 启动 Peri TUI
  2. 让主 agent 在对话中调用 Agent 工具（`run_in_background: true`，例如说"帮我后台搜索一下 XXX"）
  3. 观察：AI 回复 "Background agent bg-xxx started" 后 loading 不停止
  4. 等待 bg agent 完成
  5. 观察：bg callback 唤醒新 turn 并完成后，loading 才最终停止
- **环境**：macOS、`feature/v2-architecture` 分支
- **回归判定**：在 bg callback 修复（commit 062c85f4）之前就已存在，非新增回归

## 与已有 issue 的关系

- **关联** `spec/issues/2026-07-07-bg-agent-complete-no-resume.md`：描述 bg agent 完成后主 agent 永久卡死、合成消息未注入的问题（已通过双通道 flush-then-push 修复）。本 issue 描述同一流程中的另一个问题——续跑链路已通，但首轮 turn 结束后 loading 不停止。
- **区别**：2026-07-07 的问题是"bg 完成后主 agent 续跑从未发生"；本 issue 是"续跑链路已通，但首轮 loading 生命周期不对"。

## 涉及文件

- `peri-agent/src/agent/stages/mod.rs` —— ReAct 循环 End 阶段，MQ drain + `SyntheticUserMessage` emit 逻辑（bg callback 修复新加的 emit 点）
- `peri-middlewares/src/subagent/tool/execute_bg.rs` —— Agent 工具 background 模式，bg agent 启动入口
- `peri-tui/src/kit/acp_bridge.rs` —— TUI BridgeState，管理 `is_loading` 状态
- `peri-tui/src/kit/acp_events.rs` —— ACP 事件处理，`TurnDone` 等事件在此设置 loading 状态

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-09 | — | Open | agent | 创建（2026-07-09 修正：触发源为 Agent 工具 background 模式，非 /bg 命令） |

## 修复记录

（待修复）
