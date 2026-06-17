# 主输入框长行行尾终端光标消失

**状态**：Reopen
**优先级**：高
**创建日期**：2026-06-17
**Reopen 日期**：2026-06-17

## 问题描述

在主聊天输入框中，当某行文字宽度超过视口宽度（触发水平滚动）后，光标移动到该长行的末尾时，终端光标**完全不可见**——不是位置偏移，而是光标块/下划线在整个屏幕上消失。用户无法从视觉上判断当前编辑位置。

按下 `←` 方向键也无法恢复光标显示，光标持续不可见。

## 症状详情

| 维度 | 现象 |
|------|------|
| **触发位置** | 底部主聊天输入框（textarea） |
| **触发条件** | 输入的文字宽度超过视口宽度，textarea 内部触发水平滚动后，光标位于长行末尾 |
| **实际行为** | 终端光标完全不可见——无光标块、无下划线，视觉上完全消失 |
| **次要行为** | 按 `←` 左移光标无法恢复光标显示 |
| **期望行为** | 光标在长行任何位置（包括行尾）都应始终可见 |
| **复现频率** | 必现（只要行宽超过视口宽度 + 光标在行尾） |

## 复现条件

- **复现频率**：必现
- **触发步骤**：
  1. 启动 `peri-tui`（`cargo run -p peri-tui`）
  2. 在主输入框中持续输入文字，直至当前行宽度超过视口宽度（触发水平滚动）
  3. 观察：当光标位于长行末尾时，终端光标消失
  4. 尝试按 `←`，观察光标是否恢复
- **环境**：macOS（用户当前平台）

## 涉及文件

- `peri-tui/src/app/ime.rs:45-74` —— `textarea_cursor_pos` 函数，负责计算终端光标的坐标位置
- `peri-tui/src/ui/main_ui/mod.rs:274-278` —— `set_cursor_position` 调用入口，将计算出的光标位置设置到终端
- `peri-tui/src/app/edit_utils.rs:32-34` —— `build_textarea_with_hint`，禁用 tui-textarea 自身光标，依赖终端光标

## 关联

- 与 `spec/issues/2026-06-16-main-textarea-cursor-position-mismatch.md`（状态 Partial）疑似同根因——该 issue 的现象 2 记录了 `textarea_cursor_pos` 在水平滚动场景下的位置计算错误，本 issue 描述的是相同场景下的**更严重表现**：光标完全消失而非位置偏移

## 调研记录（2026-06-17）

### 根因定位

**Bug 位置**：`peri-tui/src/app/ime.rs:65-66` 的水平滚动推断公式。

```rust
let scroll_col = cursor_display_col.saturating_sub(visible_width.saturating_sub(1));
let visible_col = cursor_display_col.saturating_sub(scroll_col);
```

**机制**：该公式**始终**把 `visible_col` 设为 `visible_width - 1`，即光标被无条件钉在视口最右列。而 tui-textarea-2 0.11.0 的 `next_scroll_top` 逻辑在光标于视口内移动时**保持 `top_col` 不变**——两者推断的 `visible_col` 不一致。

**数值推演**（inner 宽 78，文本 100 字符全 ASCII）：

| 操作 | 光标 display_col | tui-textarea 实际 visible_col | 我们计算 visible_col | 终端 cx |
|------|-----------------|------------------------------|---------------------|---------|
| 输入到行尾 | 100 | 77 | 77 | 79 |
| ← 一次 | 99 | **76** | **77** | **79** ← 不动 |
| ← 两次 | 98 | **75** | **77** | **79** ← 不动 |
| ← N 次 | 100-N | 递减 | 始终 77 | 始终 79 |
| ← 24 次 | 76 | 0 | 76 | 78 ← 终于动一格 |

**按 24 次 ←，终端光标坐标才动一格**。之前始终卡在 `cx = inner.x + (inner.width - 1)`，即 textarea 的最右列、终端屏幕的物理最右格。

### 为什么「完全消失」而非「位置偏移」

- 终端宽 80，cx 始终为 **79**（最右列 0-79）：部分终端模拟器在最右列**裁剪或隐藏光标块/下划线**（下划线在最后一列无渲染空间）
- 部分终端在最右列准备自动折行，光标渲染行为异常
- 连续按 ← 时，~~光标位置偏移~~ → 光标**坐标完全不动**（始终 `visible_col = visible_width - 1`），用户感知为"消失"

### 确认的事实

- **tui-textarea-2 0.11.0 的 `Widget::render` 绝不调用 `Frame::set_cursor_position`**，因此不存在光标位置冲突（`widget.rs:130-179`）
- **`set_cursor_style(Style::default())` 安全**：仅禁用 textarea 自身光标的 Buffer 级视觉染色（移除 REVERSED 修饰符），不影响终端光标
- **tui-textarea 的 `top_col` 跨帧持久**：`Viewport::scroll_top()` 是 `pub`，但 `TextArea::viewport` 字段是 `pub(crate)`——外部无法直接读取真实 scroll 偏移
- **`peri-widgets/src/scrollable.rs` 与本 bug 无关**：仅管理消息区/面板的垂直滚动，不参与 textarea 的水平滚动

### 修复方向

| 方案 | 评估 |
|------|------|
| 在 app state 维护 sticky `last_scroll_col`，每帧用 `next_scroll_top` 逻辑更新 | 完全模拟 textarea 行为，准确；需要跨帧状态跟踪 |
| fork/vendor tui-textarea-2，加 `pub fn scroll_top(&self) -> (u16, u16)` getter | 最准确、改动最小；引入依赖维护成本 |
| 在 `textarea_cursor_pos` 中内联 `next_scroll_top` 逻辑 | 改动集中在 `ime.rs`；需要 static 或外部传入上一次的 scroll 状态 |

### 排除的怀疑点

| 怀疑点 | 结论 |
|------|------|
| ratatui 后续渲染覆盖了 `set_cursor_position` | 排除——行 277 之后的所有渲染（prediction、❯、hints、status_bar、bg_bar）均不调用 `set_cursor_position` |
| `inner.width` 为 0 导致 `textarea_cursor_pos` 返回 `None` | 排除——验证了正常 layout 下 `inner.width` 始终 ≥ 1 |
| tui-textarea 自己也调 `set_cursor_position` 造成冲突 | 排除——tui-textarea-2 render 签名是 `(Rect, &mut Buffer)`，无 Frame，无法设置光标位置 |

### 涉及代码行（修复点）

- `peri-tui/src/app/ime.rs:58-66` —— 水平 scroll 反推逻辑，需从"假设光标在视口最右"改为正确追踪 textarea 真实 `top_col`
- 可能需要新增的状态字段：在 UI state 中维护 `last_scroll_col`（跨帧 remember）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-06-17 | — | Open | agent | 创建 |
| 2026-06-17 | Open | Open | agent | 追加调研记录：根因确认为 `ime.rs:65-66` 水平滚动推断公式始终把光标钉在视口最右列，导致终端最右列光标裁剪/消失 |
| 2026-06-17 | Open | Verified | agent | 用户验证通过：vendor tui-textarea-2 + scroll_top()。水平滚动用真实 viewport 偏移，垂直滚动保留原始公式。CJK 正常，无残影。 |
| 2026-06-17 | Verified | Reopen | agent | 用户反馈删除/换行时仍有残影光标。根因：tui-textarea `cursor_at_end` REVERSED 空格帧间残留。修复：移除 cursor_at_end 空格 + visible_col 钳位。 |

## 修复记录

### 修复 #1（2026-06-17）

- **操作人**：agent
- **用户原意**：长行行尾终端光标完全不可见，← 光标坐标不动。需要光标在长行任何位置都始终可见。
- **修复内容**：
  1. Vendor tui-textarea-2 0.11.0，添加 `pub fn scroll_top()` 暴露 Viewport 真实 scroll 状态
  2. `peri-tui/Cargo.toml`：tui-textarea-2 改为 path 依赖
  3. `ime.rs:66`：水平滚动改 `scroll_top()` 读取真实 `top_col`。垂直滚动保留原始推断公式 `cursor_row - (height-1)`
  4. `edit_utils.rs`：保持 `REVERSED` 光标样式
- **涉及 commit**：88fe053e
- **验证状态**：部分验证（删除时有残影回归）

### 修复 #2（2026-06-17）

- **操作人**：agent
- **用户原意**：删除字符时不应留下残影光标（换行和 Backspace/Delete 后的残留 REVERSED 空格）
- **修复内容**：
  1. `peri-tui/vendor/tui-textarea-2/src/highlight.rs`：移除 `cursor_at_end` 的 REVERSED 空格渲染（`into_spans` 中两处）。光标可视化改由终端光标（`Frame::set_cursor_position`）全权负责，消除 tui-textarea 内部残留问题。
  2. `peri-tui/src/app/ime.rs`：`visible_col` 添加 `.min(visible_width.saturating_sub(1))` 钳位，防止坐标超出 inner 区域导致终端忽略光标移动、产生定位残留。
- **涉及 commit**：待提交
- **验证状态**：待验证

### 验证 #1（2026-06-17）—— 通过

用户反馈：光标位置正确，CJK 正常显示反色，换行/删除无残影。

### 验证 #2（2026-06-17）—— 失败（部分回归）

用户反馈：在删除字符时，仍会留下残影光标。具体表现：
- 换行（Enter）后，前一行行尾残留一个光标块
- 删除（Backspace/Delete）后，旧光标位置残留一个光标块

根因分析：tui-textarea-2 的 `LineHighlighter::into_spans()` 在 `cursor_at_end=true` 时渲染一个 REVERSED 空格。该空格在帧间移动时，前帧的 REVERSED 空格位置在 ratatui diff 中可能未被正确清除，导致终端上显示为「残影」。
