> 归档于 2026-07-30，原路径 spec/issues/2026-07-30-cancel-loses-agent-loop-context.md

# 取消后下一轮 Agent loop 丢失全部前文

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-30

## 问题描述

在 macOS TUI 会话中，用户在 Agent 生成普通文本回复时执行 cancel，随后发起的 Agent loop 偶发丢失全部既有前文。重启并从持久化 history 恢复同一会话后，前文又会重新出现在上下文中。期望 cancel 仅中止当前 turn，不能影响后续 turn 可用的已完成历史。

## 症状详情

- 取消后的后一个 Agent loop 偶发看不到整段既有对话，而非仅缺少被取消的 turn。
- 重启客户端并从 history 恢复会话后，先前丢失的前文再次可用。
- 当前未观察到稳定复现步骤。

## 复现条件

- **复现频率**：偶发
- **触发步骤**：
  1. 在 macOS TUI 中启动一个已有对话历史的会话。
  2. 在 Agent 生成普通文本回复期间执行 cancel。
  3. 发送新的用户消息，观察后一个 Agent loop 的上下文。
  4. 发生问题时，后一个 loop 丢失全部既有前文；重启并从 history 恢复会话后，前文重新可用。
- **环境**：macOS TUI；cancel 发生于 Agent 生成普通文本回复时。

## 涉及文件

- `peri-agent/src/agent/` —— Agent loop、turn 取消与 transcript 上下文处理的实现位置（待定位）。
- `peri-acp/src/session/` —— 会话持久化与 history 恢复的实现位置（待定位）。
- `peri-tui/src/` —— macOS TUI 中 cancel 操作与后续 prompt 发起的入口（待定位）。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-30 | — | Open | agent | 创建 |
| 2026-07-30 | Open | Fixed | agent | 修复取消结果写回：拒绝不完整 transcript，显式接受已提交 Full Compact 摘要快照 |

## 修复记录

### 修复 #1（2026-07-30）

- **操作人**：agent
- **用户原意**：cancel 后下一轮 Agent loop 必须保留已完成前文，且不应依赖 history 恢复才重新可用。
- **修复内容**：Full Compact 在事务提交成功后于 `MessageTranscript` 标记 history replacement；executor 将该语义传递至 `PromptResult`；TUI 仅据此接受缺少旧首条消息的 compact 摘要快照，其他不完整取消结果保留原 `SessionState.history`，且不写入 ThreadStore。
- **涉及 commit**：未提交
- **验证状态**：已验证
