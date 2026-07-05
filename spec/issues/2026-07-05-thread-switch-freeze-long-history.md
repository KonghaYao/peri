# Thread 切换后 TUI 完全卡死——加载长历史 thread 时渲染后冻结

**状态**：Open
**优先级**：高
**创建日期**：2026-07-05

## 问题描述

在 Thread Browser 面板中选中一个消息数量较多的历史 thread 并 Enter 切换后，消息区短暂显示出旧消息内容，随后整个 TUI 界面完全冻结——无法输入任何按键、画面不再刷新、Ctrl+C 也无响应或延迟极大。短历史 thread 切换正常。

## 症状详情

- **切换行为**：ThreadBrowser 面板 Enter → 发送 `THREAD_LOAD_TX` → ACP `session/load` → ViewCommit 事件 → 消息区渲染
- **冻结时机**：旧消息渲染完成后立即冻结，不是渲染过程中卡顿
- **卡死表现**：完全无响应。界面静止、键盘输入无效、无渲染更新
- **触发条件**：仅当 thread 中消息数量很多（几十条以上）时复现；短 thread 正常
- **相关面板**：ThreadBrowser 面板（仿 Login 面板的手动渲染模式）

## 复现条件

- **复现频率**：必现（选择有大量消息的 thread 时）
- **触发步骤**：
  1. 启动 TUI，打开 ThreadBrowser 面板
  2. 选择一个消息数量较多的历史 thread
  3. 按 Enter 切换
  4. 观察到旧消息短暂出现在消息区
  5. 随后 TUI 完全卡死
- **环境**：macOS，任意模型

## 涉及文件

- `peri-tui/src/kit/panels/thread_browser.rs` —— ThreadBrowser 面板，Enter 发送 thread ID
- `peri-tui/src/kit/thread_load_consumer.rs` —— 消费 THREAD_LOAD_TX，调用 load_session
- `peri-tui/src/kit/render_bridge.rs` —— 收到 ViewCommit 后重建 RENDER_CACHE
- `peri-tui/src/kit/acp_events.rs` —— ViewCommit → push_view_models

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-05 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
