# Loading 指示器完全不显示——TUI ACP 重构后遗症

**状态**：Open
**优先级**：中
**创建日期**：2026-07-08

## 问题描述

TUI 中的 Loading 指示器（Spinner 动画 + 烹饪动词）在提交 prompt 后完全不显示。用户输入消息按 Enter 提交后，界面上没有任何"AI 正在处理中"的视觉反馈——spinner 从未出现、从未开始旋转。消息流输出、输入框、面板切换等其他功能正常。

用户反馈此问题是在 TUI ACP 链路重构后出现（可能与 ViewModel 消除重构 `2026-07-08-viewmodel-elimination` Phase 1-5 相关）。

## 症状详情

| 表现 | 详情 |
|------|------|
| Spinner 动画 | 从不显示 |
| 加载状态文字（如"✻ 炖煮中..."） | 从不显示 |
| 消息流式输出 | 正常（文本逐字出现） |
| 工具调用卡片 | 正常 |
| 输入框提交 | 正常（loading 中可缓冲排队） |
| 其他面板 | 正常切换 |

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 启动 Peri TUI
  2. 输入任意文本并按 Enter 提交
  3. 观察：消息区底部无 spinner、无加载动画
- **环境**：macOS 26.5.1，Rust 开发环境

## 涉及文件

- `peri-tui/src/kit/acp_events.rs` —— `dispatch_and_notify` 处理各类 ACP 事件，`push_acp_state()` 从 `BridgeState.phase` 派生 `is_loading` 并写入 `ACP_STATE` atom（第 485-489 行）
- `peri-tui/src/kit/submit_consumer.rs` —— `handle_agent_text_submit` 提交时直接写 `ACP_STATE.is_loading = true`（第 161 行）
- `peri-tui/src/kit/message_area.rs` —— `build_footer_lines` 从 `ACP_STATE.is_loading` 读取并驱动 spinner 显示（第 256 行）
- `peri-tui/src/kit/atoms.rs` —— `ACP_STATE` atom 定义，包含 `is_loading` 字段

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-08 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
