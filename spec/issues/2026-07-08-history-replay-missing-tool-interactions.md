# History 面板恢复的对话历史缺少工具调用和工具结果，消息内容与原始对话不一致

**状态**：Open
**优先级**：高
**创建日期**：2026-07-08
**类型**：Bug

## 问题描述

用户通过 History 面板（`/history`）切换到历史 session 后，消息区显示的历史消息缺少原对话中的**工具调用卡片**（如 Read / Bash / Edit 等）及**工具执行结果**。恢复后用户只能看到交替的 Human/AI 文本气泡，整个对话流程不完整。此外 AI 回复的文字内容也与原始对话不完全一致。

## 症状详情

| 维度 | 观察 |
|------|------|
| 触发操作 | History 面板选择 session → 按 Enter 切换 |
| 实际表现 | 消息区只显示 Human/AI 文本气泡，工具调用卡片和工具结果均不可见 |
| 期望表现 | 恢复结果应与原始对话一致，包含工具调用卡片和工具执行结果 |
| 复现频率 | 每次切换必现 |
| 影响范围 | 所有包含工具调用的历史 session |

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 进行一次包含工具调用的对话（比如 "帮我读取 src/main.rs 的内容"）
  2. 观察实时对话中工具调用的正常展示（含工具卡片和结果）
  3. 退出或切换到其他 session
  4. 通过 `/history` 打开 History 面板
  5. 选择刚才的对话，按 Enter 恢复
  6. 观察消息区——工具调用卡片和工具结果消失，仅有交替的 Human/AI 文本气泡
- **环境**：任意模型、macOS、任意配置

## 涉及文件

- `peri-tui/src/kit/acp_notifier.rs`（line 307-447）—— `handle_session_update` 处理 `agent_message_chunk` 时，`is_session_replay=true` 分支硬编码映射为 `ReplayAssistantBubble`，直接写入 committed，绕过了正常的 `current_turn` 管道
- `peri-tui/src/kit/acp_events.rs`（line 325-346）—— `ReplayUserBubble` 和 `ReplayAssistantBubble` 处理分支直接将 ViewModel 追加到 `state.committed`，不经过 `current_turn` 的 `ToolStarted`/`ToolEnded` 流式路径
- `peri-tui/src/kit/acp_bridge.rs`（line 58-93）—— BRIDGE_RESET_COUNTER 检测和 session filter 的时序窗口

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-08 | — | Open | deepseek-v4-pro | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
