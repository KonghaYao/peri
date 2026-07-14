> 归档于 2026-07-06，原路径 spec/issues/2026-07-05-mouse-move-cpu-spike.md

# 鼠标晃动导致 CPU 暴涨

**状态**：fixed
**优先级**：中
**创建日期**：2026-07-05

## 问题描述

TUI 运行时，鼠标在终端窗口内晃动（无需点击或拖拽，仅移动光标）即可导致 CPU 使用率大幅上升。触发范围是整个终端窗口，不仅限于消息区。

## 症状详情

| 操作 | 期望行为 | 实际行为 |
|------|----------|----------|
| 鼠标在终端窗口内移动 | CPU 保持低位，无明显开销 | CPU 暴涨 |
| 鼠标静止 | CPU 正常 | CPU 正常 |
| 鼠标移动范围 | — | 整个终端窗口均有此现象 |

- **复现频率**：必现
- **触发步骤**：
  1. 启动 Peri TUI
  2. 在终端窗口内晃动鼠标
  3. 观察 CPU 使用率（如 macOS Activity Monitor 或 `top`）
- **环境**：macOS，kit 架构

## 出现场景

- 任何 TUI 运行状态（无论 agent 是否在运行、是否有消息内容、是否打开了面板）
- 整个终端窗口范围内移动鼠标均触发

## 涉及文件

- `peri-tui/src/kit/message_area.rs` —— 注册了 `EventScope::Global` 的事件处理器，消费所有 `Event::Mouse` 事件（包括 `MouseMove`），每帧重渲染可能累积注册

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-05 | — | Open | agent | 创建 |
| 2026-07-05 | Open | Fixed | agent | 修复：MouseMove 事件提前忽略，不走 state 读写/锁获取 |

## 修复记录

### 修复 #1（2026-07-05）

- **操作人**：agent
- **用户原意**：修复鼠标晃动导致 CPU 暴涨的问题
- **修复内容**：在 `message_area.rs` mouse 事件处理入口处增加 `MouseEventKind::Moved` 提前忽略（返回 `EventResult::Ignored`），避免每次 MouseMove 高频事件获取 `scroll_state` 写锁和 `auto_scroll.set(false)` state 写入
- **涉及文件**：`peri-tui/src/kit/message_area.rs`（第 321-324 行）
- **验证状态**：待验证
