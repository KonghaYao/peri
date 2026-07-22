> 归档于 2026-07-17，原路径 spec/issues/2026-07-11-cancel-no-rollback-no-restore.md
# Ctrl+C 取消后未回滚用户消息、未恢复文本到输入框

**状态**：fixed
**优先级**：中
**创建日期**：2026-07-11

## 问题描述

用户在 Agent loading 期间按 Ctrl+C 取消请求。当 Agent 尚未产出任何 AI 响应（零产出）时，期望行为是：撤回本次的用户消息气泡 + 把用户提交的文本恢复到输入框。但实际行为是：用户气泡仍残留在聊天区，文本也不恢复到输入框，用户需要重新手动输入。

这是 **kit 单路径迁移（Phase 2.6）的回归**——v1 架构中已实现完整回滚逻辑（commit `e12fbeaf`，2026-05-25），迁移到 ratatui-kit 后丢失。

## 症状详情

### 现象 1：用户气泡残留

| 步骤 | 期望 | 实际 |
|------|------|------|
| 1. 输入 "帮我写一个函数"，Enter 提交 | 用户气泡显示 | 用户气泡显示 ✓ |
| 2. LLM 尚未开始回复，按 Ctrl+C | 用户气泡消失 | 用户气泡仍在 ✓（残影） |
| 3. 用户重新输入新内容提交 | 只有新消息 | 旧消息气泡仍在 |

### 现象 2：输入框文本未恢复

| 步骤 | 期望 | 实际 |
|------|------|------|
| 2. 按 Ctrl+C 取消后 | 输入框出现 "帮我写一个函数" | 输入框为空 |

### 现象 3：过期注释

`peri-tui/src/acp_server/prompt.rs:255-258` 的注释声称 TUI 会做这些事情：

```rust
// Roll back to pre-submit state — the TUI's handle_interrupted will also
// truncate view_messages and restore text to input for the no-tool-call case.
```

但当前 `acp_events.rs` 的 `TurnInterrupted` 处理器没有实现这些逻辑，该注释描述的是 v1 的旧行为。

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 在输入框输入任意文本
  2. Enter 提交
  3. 在 LLM 开始流式输出**之前**按 Ctrl+C（越快越好，LLM 还没产出任何内容时）
  4. 观察：用户气泡是否消失、文本是否回到输入框
- **环境**：所有环境

## 涉及文件

| 文件 | 当前状态 | 说明 |
|------|----------|------|
| `peri-tui/src/kit/acp_events.rs:267-282` | 需修改 | TurnInterrupted 处理器，需新增：移除用户气泡 + 恢复文本到输入框 |
| `peri-tui/src/kit/acp_bridge.rs` | 需修改 | BridgeState 中可能需要新增标记位，记录本轮提交的文本 |
| `peri-tui/src/kit/submit_consumer.rs` | 确认 | 确认提交时将用户文本写入可恢复的位置 |
| `peri-tui/src/acp_server/prompt.rs:255-258` | 注释修正 | server 端注释应更新以反映当前 TUI 行为 |

## 回滚判定条件

当前 server 端 `prompt.rs:228` 的条件：

```rust
result.messages.len() > history_len + 1
```

即：提交后的消息数 > 原历史长度 + 1（用户消息）。如果仅多了用户消息（零 AI 产出），就回滚。

**注意**：这与"无工具调用"不完全一致——即使 AI 生成了纯文本但没有工具调用，`messages.len()` 也会 > `history_len + 1`，此时 server 端**不会**回滚。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-11 | — | Open | agent | 创建 |

## 修复记录

### 修复 #1（2026-07-11）

- **操作人**：agent
- **修复内容**：
  1. `acp_events.rs` BridgeState 新增 `last_submitted_text: Option<String>` 字段，LocalUserBubble 到达时保存文本
  2. `acp_events.rs` TurnInterrupted 处理器新增零产出回滚分支（`current_turn.is_empty() && last_submitted_text.is_some()`）：移除 committed 中最后一条用户气泡 + 恢复文本 + 清 INPUT_BUFFER
  3. `atoms.rs` 新增 `INPUT_RESTORE_TEXT: OnceLock<Mutex<Option<String>>>` 非 atom 存储（避免 render body 写 atom 产生反馈回路）
  4. `input_area.rs` render body 中直接读锁消费恢复文本
  5. `prompt.rs` 更新过期注释
  6. 新增测试 `test_turn_interrupted_zero_output_rollback`
- **涉及文件**：`acp_events.rs`, `atoms.rs`, `input_area.rs`, `prompt.rs`, `acp_bridge.rs`
- **测试**：peri-tui 451/451 通过
- **验证状态**：待用户手动验证
