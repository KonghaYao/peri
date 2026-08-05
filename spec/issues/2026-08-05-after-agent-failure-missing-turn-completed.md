# run_after_agent 失败：最终回答已写 transcript 但无 TurnCompleted（TUI 与持久化不一致）

**状态**：Open
**优先级**：中
**创建日期**：2026-08-05
**关联计划**：`2026-08-05-core-flow-bugfix-plan.md` S5.3

## 问题描述

`act.rs` 中 `transcript.append(ai_msg)`（`:97`）在 `run_after_agent`（`:125`）之前；`run_after_agent` 返回 Err 时 loop 直接结束，`:130-139` 的 `TurnCompleted` 永不执行。TUI 的 committed 视图不含最终回答，而 transcript（以及后续持久化）含最终回答——下次恢复时重建出的视图与用户上次看到的完全不同。与工具路径的同类缺口（审批 cancel 无 TurnCompleted，见挂起项 P4-1）一起，说明"Err 路径不提交迭代边界"是系统性缺口。

## 症状详情

- 这是异常路径收尾缺口中最容易修的项：失败时从 transcript 读快照 emit `TurnCompleted` 再传播错误即可；RwLock 无死锁风险（append 已释放锁）
- **镜像问题**（可一并处理或记录）：工具路径 cancel 后 transcript 有消息但 `TurnCompleted` 不 emit（`act.rs:60-77` 在 dispatch 成功后）——TUI MessagePipeline 不提交迭代，UI 与 transcript 不一致

## 复现条件

- **复现频率**：偶发（`run_after_agent` 失败路径）
- **触发步骤**：最终回答后 after_agent middleware（error_suggest 等）抛错

## 涉及文件

- `peri-agent/src/agent/stages/act.rs:92-139` —— append 先于 run_after_agent，Err 路径无 TurnCompleted

## 修复方向（对抗 review 确认可行）

- `run_after_agent` 失败时，先从 transcript 读快照 emit `TurnCompleted`，再传播错误
- 同步评估 `act.rs:60-77` 工具路径 cancel 的镜像缺口（可拆子任务）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-08-05 | — | Open | agent | 创建（peri-agent 审查发现；对抗 review 验证低风险可行） |
| 2026-08-05 | Open | Fixed | agent | 修复：run_after_agent 失败先 emit TurnCompleted 再传播错误；镜像项（工具路径 dispatch 失败）一并修复 |

## 修复记录

### 修复 #1（2026-08-05）

- **修复内容**（`peri-agent/src/agent/stages/act.rs`）：提取 `emit_turn_completed` helper（从 transcript 读 `visible_snapshot` → `emit_render(RenderEvent::TurnCompleted)`，RwLock 无死锁风险——append 已释放锁），三处共用：
  1. 工具路径成功（原 68-77 逻辑原样迁移）；
  2. 最终回答成功（原 130-139 逻辑原样迁移）；
  3. **最终回答失败**：`run_after_agent` 返回 Err 时先 `emit_turn_completed` 再传播错误——最终回答已 append 到 transcript（:97），TUI committed 视图必须与 transcript/持久化一致。
- **镜像问题（一并修复，改动面小）**：工具路径 `dispatch_tools` 失败（cancel / middleware 错误）时也先 emit TurnCompleted 再传播错误——阶段 B（staging commit）之后 transcript 已含本轮 AI 消息 + tool 结果，TUI MessagePipeline 提交迭代边界，UI 与 transcript 保持一致。审批阶段 cancel（transcript 无本轮消息）时 emit 的快照为旧内容，与"取消不留痕"语义不冲突（TurnCompleted 只影响渲染提交，不写 transcript）。
- **测试**（`act_test.rs`）：新增
  - `test_act_after_agent_failure_emits_turn_completed`：注入 after_agent 恒失败的测试 middleware，断言错误传播 + TurnCompleted 已 emit 且快照含最终回答；
  - `test_act_tool_path_cancel_emits_turn_completed`：预取消 cancel token，断言 `Err(Interrupted)` + TurnCompleted 已 emit。
- **涉及文件**：`peri-agent/src/agent/stages/act.rs`、`peri-agent/src/agent/stages/act_test.rs`
- **验证状态**：待验证（`cargo test -p peri-agent --lib` 645 passed；`cargo clippy -p peri-agent --lib -- -D warnings` 通过）
