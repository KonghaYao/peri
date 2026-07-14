# 消息区域无文本选中和复制能力

**状态**：Partial
**优先级**：中
**创建日期**：2026-07-05

## 问题描述

当前 kit 架构的消息区域（`message_area.rs`）不支持鼠标拖拽选中文本，也无法将选中的消息内容复制到系统剪贴板。用户需要复制 agent 输出或工具调用结果时，只能回退到终端自身的原生选中功能（选中后 Ctrl+Shift+C），而这在 raw mode 下不可用。

## 症状详情

| 操作 | 期望行为 | 实际行为 |
|------|----------|----------|
| 鼠标拖拽选择消息区文本 | 显示选择高亮，选中区域视觉反馈 | 无反应，无法选中 |
| 选中文本后自动复制 | 松开鼠标后文本写入系统剪贴板 | 无反应 |
| 选中后状态栏提示 | 显示 "已复制 N 字符" 提示 | 无提示 |

## 复现条件

- **复现频率**：必现（功能未实现）
- **触发步骤**：
  1. 在消息区域用鼠标拖拽尝试选中文本
  2. 观察无选中高亮、无复制行为
- **环境**：所有平台，kit 架构

## 涉及文件

- `peri-tui/src/kit/message_area.rs` —— 消息区渲染与事件处理，需新增鼠标选中逻辑
- `peri-tui/src/kit/event_handlers.rs` —— 全局事件处理，可能需要注册消息区鼠标事件
- `peri-tui/src/kit/atoms.rs` —— 可能需要新增 text_selection / copy 相关 atom

## 参考实现

旧 v1 架构（`peri-main`，已废弃）有完整的选中+复制实现，位于：

- `peri-main/peri-tui/src/app/text_selection.rs` —— `TextSelection` 数据结构（start/end 视觉坐标、dragging 状态、selected_text）
- `peri-main/peri-tui/src/event/mouse.rs` —— `copy_selection_to_clipboard()` / `copy_panel_selection_to_clipboard()`，使用 `arboard::Clipboard`
- `peri-main/peri-tui/src/event/mod.rs:882-926` —— 鼠标松开时提取选中文本并复制
- `peri-main/peri-tui/src/ui/main_ui/message_area.rs:347-368` —— 消息区选中坐标与渲染缓存的交互

核心流程：鼠标拖拽在消息区记录起始/结束视觉坐标 → 松开时从 `wrap_map`（折行映射）提取对应纯文本 → `arboard::Clipboard::set_text()` 写入系统剪贴板 → 状态栏显示 "已复制 N 字符"。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-05 | — | Open | agent | 创建 |
| 2026-07-05 | Open | Partial | agent | 初版实现已写入，但存在崩溃和选区高亮不一致问题，根因未查清，搁置待后续处理 |

## 修复记录

### 修复 #1（2026-07-05）

- **操作人**：agent
- **用户原意**：从旧 v1 架构移植消息区鼠标选中 + 复制到剪贴板功能
- **修复内容**：
  - 新建 `peri-tui/src/kit/text_selection.rs`：TextSelection 结构体 + 字符级文本提取 + 选区高亮
  - 修改 `peri-tui/src/kit/message_area.rs`：集成鼠标拖拽选中、arboard 剪贴板、MsgAreaTracker Hook
  - 修改 `peri-tui/src/kit/atoms.rs`：新增 COPY_CHAR_COUNT / COPY_MESSAGE_UNTIL atom
  - 修改 `peri-tui/src/kit/status_bar.rs`：新增 "已复制 N 字符" 提示
  - 修改 `peri-tui/src/kit/mod.rs`：注册 text_selection 模块
- **验证状态**：验证失败（见下方残留问题）

## 残留问题（2026-07-05，搁置）

### 1. 启动后立即崩溃

**现象**：TUI 启动后未做任何操作即崩溃（具体错误信息未捕获）。
**调查状态**：`cargo test` 326 全过，headless 模式无法复现。code-review 扫描未发现明确 panic 路径。疑似与终端兼容性或 arboard macOS 初始化有关。
**排查方向**：用 `RUST_BACKTRACE=1 peri-tui 2>panic.log` 在真实终端中重现获取堆栈。

### 2. `use_event_handler` 每帧累积

**现象**：`message_area.rs` 每帧重渲染注册新的 Global 事件处理器，旧处理器不删除。导致 O(帧数) 次闭包调用 + `Arc<Vec<Line>>` 内存泄漏。
**影响**：长时间运行后性能退化。
**排查方向**：改为 `use_state` 管理的单次注册，或框架层支持 handler 去重/替换。

### 3. macOS arboard 剪贴板可能静默失败

**现象**：事件处理在 ratatui-kit 事件循环线程（非主线程），macOS 下 arboard 需要 AppKit run loop，`Clipboard::new()` 可能返回 Err。
**影响**：macOS 上复制功能不可用（不崩溃，已用 `if let Ok` 兜底）。
**排查方向**：主线程初始化 `Clipboard` 并缓存，或用 `clipboard-rs` 替代。
