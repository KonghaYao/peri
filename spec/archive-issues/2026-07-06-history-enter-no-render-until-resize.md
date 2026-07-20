# History 面板 Enter 切换 session 后消息区不渲染（显示 Welcome），改宽度后才恢复


> 归档于 2026-07-20，原路径 spec/issues/2026-07-06-history-enter-no-render-until-resize.md
**状态**：Fixed
**优先级**：高
**创建日期**：2026-07-06

## 问题描述

在 History 面板（ThreadBrowser）中选择一个历史 session 并按 Enter 切换后，消息区不显示该 session 的消息，而是显示 Welcome 空白页。但改变终端宽度（resize）后，消息立即正常渲染出来。

切换前后消息区状态变化：原 session 消息可见 → Enter 切换 → Welcome 页面（无消息）→ 改变终端宽度 → 消息正常出现。

## 症状详情

| 维度 | 观察 |
|------|------|
| 触发操作 | History 面板选择历史 session → 按 Enter 切换 |
| 实际表现 | 消息区显示 Welcome 页面（无任何消息渲染） |
| 期望表现 | 消息区显示目标 session 的历史消息 |
| 恢复方式 | 改变终端宽度（resize）后消息正常出现；Ctrl+L、鼠标滚轮等操作无效 |
| 复现频率 | 必现 |
| 影响 session 类型 | 长 session（多轮对话），短 session 是否受影响待确认 |

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 在 session A 中进行多轮对话（产生较长消息历史）
  2. Ctrl+B 打开 History 面板
  3. 选择一个有较长历史消息的 session B，按 Enter
  4. 观察消息区 → 显示 Welcome 页面（无任何消息）
  5. 改变终端窗口宽度（如拖拽窗口边缘）→ 消息立即正常出现
- **环境**：macOS 26.5.1

## 涉及文件

| 文件 | 角色 |
|------|------|
| `peri-tui/src/kit/message_area.rs:325` | Welcome 判定：`empty = cache_snapshot.entries.is_empty() && !is_loading`，当 RENDER_CACHE 为空时显示 Welcome |
| `peri-tui/src/kit/render_bridge.rs` | RENDER_CACHE 构建——事件驱动路径未能触发重建；resize 路径通过 `rebuild_all` 从 VIEW_MODELS atom 重新读取并正常构建 |
| `peri-tui/src/kit/thread_load_consumer.rs` | History 面板 Enter → BRIDGE_RESET_COUNTER + load_session RPC |
| `peri-tui/src/kit/acp_events.rs:323-335` | push_view_models 将 BridgeState 写入 VIEW_MODELS atom |
| `peri-tui/src/kit/acp_bridge.rs` | 事件路由与 BRIDGE_RESET_COUNTER 检测 |

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-06 | — | Open | agent | 创建 |

## 修复记录

（待修复）
