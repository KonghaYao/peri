> 归档于 2026-07-06，原路径 spec/issues/2026-07-05-message-area-crashes-and-rendering.md

# 消息区多场景崩溃/白屏/滚动异常

**状态**：Fixed
**优先级**：高
**创建日期**：2026-07-05

## 问题描述

kit 架构迁移后，消息区在以下场景出现崩溃或渲染异常：输入内容后按上下方向键崩溃、滚轮滚动崩溃、选中复制后崩溃、内容超出可视区时滚轮滚动导致内容消失/白屏、history 会话切换后滚动不到底部。

## 症状详情

| 操作 | 期望行为 | 实际行为 |
|------|----------|----------|
| 输入 "hello" 后按 Enter → 按 Down ↓ | 正常浏览历史 | 程序崩溃 |
| 输入 "hello" 后按 Enter → 消息区滚轮上滚 | 消息区正常滚动 | 程序崩溃 |
| 输入任意文本 → 按 Enter → 上方向键 | 正常浏览历史 | 程序崩溃 |
| 鼠标拖选消息区文本 → 松开复制 | 文本写入剪贴板，状态栏提示 | 程序崩溃 |
| 消息超过一屏 → 滚轮滚动 | 正常滚动，滚动条显示 | 内容消失/白屏，滚动条不显示或异常 |
| session/load 切换到历史会话 | 消息显示并自动滚到最底部 | 消息显示但滚动位置停留在顶部 |

### 崩溃信息

**场景 1：Enter + 方向键/滚轮**
```
thread panicked at 'attempt to add with overflow'
at peri-tui/src/kit/message_area.rs:103:19
```
异常日志示例（agent-tui.log）：
- 输入 `/his` → Enter → Down → 第二条 Enter → 崩溃
- 输入 `hello` → Enter → ScrollUp → 崩溃

**场景 2：滚轮滚动**
```
thread panicked at 'attempt to add with overflow'
at peri-tui/src/kit/message_area.rs:255:42
```
异常日志示例（agent-tui.log）：
- 输入 `hello` → Enter 触发了 agent → 消息区 ScrollUp → 崩溃

**场景 3：复制崩溃**
日志无 panic 记录，拖选 `Down(Left)` → `Drag` → `Up(Left)` 后进程直接退出。

**场景 4：滚动内容消失/白屏**
条件：消息超过终端一屏高度（需要滚动条时必定出现）。滚轮滚动后消息区全部变白，内容不可见。无需滚动条时（内容在一屏内）正常显示。

## 复现条件

- **复现频率**：必现（kit 架构迁移后一直存在）
- **触发步骤**：
  1. 启动 Peri TUI
  2. 在输入框输入任意文本（如 "hello"），按 Enter 提交
  3. 等待 agent 回复有足够内容超出终端一屏
  4. 执行任一崩溃操作：按 ↑/↓ 方向键、消息区滚轮、鼠标拖选复制
  5. 或直接滚轮滚动查看消息（内容消失/白屏）
- **环境**：macOS，kit 架构。`view_render.rs` 有遗留编译错误（括号不匹配）可能阻塞验证。

## 涉及文件

- `peri-tui/src/kit/message_area.rs` —— 消息区渲染、`viewport_clip` 视口裁剪（u16 overflow 来源）、鼠标事件坐标换算（u16 overflow 来源）、sticky header 嵌套 Fill(1) 布局
- `peri-tui/src/kit/text_selection.rs` —— 文本选区高亮
- `peri-tui/src/kit/render_bridge.rs` —— `build_wrap_map` 视觉行映射生成
- `peri-tui/src/kit/layout.rs` —— SessionColumn 布局，MessageArea 无显式 height

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-05 | — | Fixed | agent | 创建并完成修复 |


## 修复记录

### 修复 #1：u16 overflow 崩溃（2026-07-05）

- **操作人**：agent
- **用户原意**：修复 TUI 输入后按方向键/滚轮/复制时程序崩溃
- **修复内容**：
  - `message_area.rs:103`：`scroll_y + vis_height` → `scroll_y.saturating_add(vis_height)`（Enter + Down 崩溃）
  - `message_area.rs:248-256`：鼠标命中判断和视觉坐标换算三处 u16 加法 → `saturating_add`/`saturating_sub`（滚轮崩溃）
  - `message_area.rs:293-312`：arboard 剪贴板写入从 UI event handler 移到 `std::thread::spawn` 独立线程（复制崩溃）
- **涉及文件**：`peri-tui/src/kit/message_area.rs`
- **验证状态**：待验证

### 修复 #2：滚动内容消失/白屏（2026-07-05）

- **操作人**：agent
- **用户原意**：修复消息超过一屏时滚动内容消失/白屏
- **修复内容**：
  - 移除 `Paragraph.scroll((local_offset, 0))` 与 ScrollView 的双重滚动冲突
  - 添加 spacer `View(height: Constraint::Length(content_top))` 将 visible_lines 定位在正确垂直偏移处
  - `build_wrap_map` 结果缓存到 `LineCache.cached_wrap_map`（修复白屏性能问题）
  - 临时禁用 sticky header（`show_sticky = false`）
- **涉及文件**：`peri-tui/src/kit/message_area.rs`
- **验证状态**：已验证（用户确认白屏已解决）

### 修复 #3：history 切换后滚动不到底部（2026-07-05）

- **操作人**：agent
- **用户原意**：history 会话加载后消息区应自动滚到最底部
- **修复内容**：
  - 在内容 key 变化时 `auto_scroll.set(true)`，覆盖 session/load 批量加载（无 CurrentTurn 流式事件）的场景
- **涉及文件**：`peri-tui/src/kit/message_area.rs`
- **验证状态**：待验证
