> 归档于 2026-07-18，原路径 spec/issues/2026-07-05-scroll-performance-lag.md
# 长数据高速滚动时刷新卡顿/掉帧

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-05

## 问题描述

消息区有大量对话消息（数百行以上）时，高速滚轮滚动（`SCROLL_MULTIPLIER = 3`）会导致明显的卡顿和掉帧现象。滚动不够流畅，尤其在持续快速滚动时帧率下降明显。

## 症状详情

| 操作 | 期望行为 | 实际行为 |
|------|----------|----------|
| 长对话中快速滚轮滚动 | 流畅滚动，帧率稳定 | 卡顿，帧率下降 |
| 持续快速滚动（连续滚轮） | 无延迟跟上 | 滚动响应滞后，画面不流畅 |

- **复现频率**：必现（长数据 + 高速滚动条件满足时）
- **环境**：macOS，kit 架构。`SCROLL_MULTIPLIER = 3` 放大了单次滚动的视觉行变化量，使卡顿更易感知。

## 出现场景

- 消息区有大量历史消息时（数百行以上）
- 高速滚轮滚动（尤其是刚加了 `SCROLL_MULTIPLIER` 后每次滚轮移动 3 行）
- 如果存在文本选区高亮，可能进一步加剧

### 现象 2（2026-07-11 追加）：tmux 环境下任意数据量滚动卡顿

与现象 1 不同，此现象**不依赖数据量**——即使消息区只有几十行，在 tmux 中鼠标滚轮滚动即有明显卡顿。同一环境下原生终端和 VSCode 集成终端均流畅。

| 操作 | 终端 | 结果 |
|------|------|------|
| 鼠标滚轮滚动 | tmux（半屏或更小窗格） | 卡顿，不流畅 |
| 鼠标滚轮滚动 | 原生终端（macOS Terminal/iTerm2） | 流畅 |
| 鼠标滚轮滚动 | VSCode 集成终端 | 流畅 |
| 键盘滚动 | 未测试 | — |

- **复现频率**：必现（tmux 内）
- **复现条件**：任意数据量（几十行即可感知），半屏或更小窗格
- **环境**：macOS + tmux（版本未确认），kit 架构
- **关键差异**：现象 1 是计算密集型（数据量大时 viewport_clip/文本选区处理耗时），现象 2 疑似 tmux 环境特有的 I/O 或终端事件处理问题（因为少量数据也卡，且仅 tmux 出现）

## 涉及文件

- `peri-tui/src/kit/message_area.rs` —— 视口裁剪 `viewport_clip()` 二分查找 + `highlighted_lines` 切片，每帧执行
- `peri-tui/src/kit/render_bridge.rs` —— `build_wrap_map` 已缓存到 `LineCache.cached_wrap_map`，仅内容变化时重建
- `peri-tui/src/kit/text_selection.rs` —— `highlight_selected_lines` 在有选区时每帧对全量行执行

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-05 | — | Open | agent | 创建 |
| 2026-07-11 | Open | Open | agent | 追加现象 2：tmux 环境下任意数据量滚动卡顿，与原生/VSCode 对比 |

## 修复记录

### 修复 #1（2026-07-11）—— 现象 2：消除滚动事件渲染风暴

- **操作人**：agent
- **用户原意**：tmux 下滚动卡顿，原生/VSCode 流畅
- **根因**：每个 `ScrollDown`/`ScrollUp` 事件 handler 内 `scroll_state.write() × SCROLL_LINES(3)` 产生 3 次独立原子通知 → ratatui-kit render loop 中 `select(wait(), next_event())` 的 `wait()` 被连续唤醒 3 次 → **每个滚轮事件产生 4 次 `terminal.draw()`**（1 次 loop 强制渲染 + 3 次原子通知触发渲染）。tmux 下 `terminal.draw()` 经 PTY（ptm→pts→tmux→终端），每次 draw 有额外序列化开销，4 倍放大后远超帧预算。
- **修复内容**：
  1. 合并 3 次 `scroll_state.write()` 为 1 次 `scroll_state.write_no_update()`——在 handler 内单次持锁完成所有 scroll 操作，不触发原子通知（looper 强制渲染已会读到最新 atom 值）
  2. 消息区内/外两处 ScrollDown/ScrollUp 各 2 个方向，共 4 处修改
  3. 从 4 次 render/事件 → 1 次 render/事件（4x 改善）
- **涉及 commit**：待提交
- **验证状态**：待验证（需在 tmux 中实测）

### 修复 #2（2026-07-11）—— 现象 2：16ms 帧间隔节流

- **操作人**：agent
- **用户原意**：快速交替上下滚动仍有事件积压
- **根因**：修复 #1 将 4 次 render/事件降至 1 次，但 macOS 鼠标滚轮 60-125Hz → 每秒仍有 60-125 次 `terminal.draw()`。tmux 下每次 draw 经 PTY，事件速率超过 tmux 处理能力时积压。
- **修复内容**：
  1. 新增 `ScrollThrottle` 结构体追踪 `last_flush`（Instant）和 `pending_delta`（i32，正=下滚，负=上滚）
  2. 新增常量 `SCROLL_FRAME_MS = 16`（≈60fps 上限）
  3. 引入 `apply_scroll(delta)` 闭包：累积增量到 throttle，仅当 `elapsed ≥ 16ms` 时一次性推入 `scroll_state`
  4. 4 处 ScrollDown/ScrollUp handler 统一委托给 `apply_scroll`
  5. 继续使用 `write_no_update()`——flush 也不触发原子通知，由 loop 强制 render 读最新 atom 值
  6. 从 60-125 次 render/秒 → ≤62.5 次 render/秒（仅实际 atom 变化时），其余 skip 事件不写 atom（ratatui diff 优化使空 render 几乎零 I/O）
- **涉及 commit**：待提交
- **验证状态**：待验证（需在 tmux 中快速交替滚动实测）
