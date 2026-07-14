# /clear 变成聊天消息 & /history 加载 thread 卡死

**状态**：Open
**优先级**：高
**创建日期**：2026-07-04

## 问题描述

两个 slash 命令执行异常：

1. **/clear**：输入 `/clear` 后，命令没有被执行（未清空会话），而是被当作普通文本发送给了 agent，表现为聊天消息。
2. **/history**：`/history` 可以打开 ThreadBrowser 面板，但选中某个历史 thread 按 Enter 加载时，界面直接卡死（无响应）。

## 症状详情

### /clear

| 项目 | 内容 |
|------|------|
| 输入 | `/clear` |
| 期望行为 | 清空当前会话 |
| 实际行为 | 变成了普通聊天消息，发送给 agent |

### /history

| 项目 | 内容 |
|------|------|
| 触发方式 | `/history` 或 Ctrl+T |
| 面板打开 | 正常（ThreadBrowser 面板可以打开，thread 列表可见） |
| 卡死时机 | 选中某个历史 thread 并按 Enter 加载时 |
| 卡死表现 | 界面完全冻结，无响应 |

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 启动 Peri TUI
  2. `/history` → 打开 ThreadBrowser 面板
  3. 选中历史 thread → Enter → 卡死
- **环境**：macOS, feature/v2-architecture 分支

## 涉及文件

- `peri-tui/src/kit/input_area.rs` —— slash 命令识别与分发逻辑
- `peri-tui/src/kit/thread_load_consumer.rs` —— thread 历史加载后台任务

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-04 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
