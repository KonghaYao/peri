> 归档于 2026-07-06，原路径 spec/issues/2026-07-05-enter-clear-hook-mismatch-panic.md

# Enter 提交 & /clear 清屏触发 Hook type mismatch panic 导致 TUI 崩溃

**状态**：fixed+ verify
**优先级**：高
**创建日期**：2026-07-05

## 问题描述

Enter 提交消息或 /clear 清屏后，ratatui-kit 渲染循环 panic：

```
Hook type mismatch, ensure the hook is of the correct type
```

进程直接崩溃退出。此问题与 `2026-07-04-slash-clear-chat-cmd-history-thread-freeze.md` 属于同一 bug 家族（ratatui-kit hook 顺序违规），本次聚焦 `MessageArea` 内的 hook 调用顺序问题。

## 症状详情

| 触发操作 | 表现 |
|----------|------|
| Enter 提交文本 | 用户输入框清空后界面立即崩溃 |
| /clear 清屏 | 旧会话关闭、新会话创建后崩溃 |
| ThreadBrowser 加载历史 | 同 `2026-07-04-slash-clear-chat-cmd-history-thread-freeze.md` |

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 启动 Peri TUI
  2. 输入任意文本 → Enter（崩溃）
  3. 或：`/clear` → 回车（崩溃）
  4. 或：`/history` → 选择历史 thread → Enter（崩溃）
- **环境**：macOS

## 涉及文件

- `peri-tui/src/kit/message_area.rs` —— `build_footer_lines` 函数内条件早退 + 调用点在 `if empty` 分支后
- `peri-tui/src/kit/submit_consumer.rs` —— /clear 触发新 session

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-05 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
