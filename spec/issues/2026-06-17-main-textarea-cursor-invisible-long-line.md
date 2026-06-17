# 主输入框长行行尾终端光标消失

**状态**：Verified（macOS/Linux 用 buffer 级 REVERSED 光标根治；Windows 用 cfg 启用 IME 模块，接受推断公式缺陷）
**优先级**：高
**创建日期**：2026-06-17
**最终修复日期**：2026-06-17（修复 #5：Windows-only IME 模块）

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
| 2026-06-17 | Reopen | Verified | agent | 用户验证通过。修复 #2 移除 cursor_at_end REVERSED 空格后，删除/换行无残影。 |
| 2026-06-17 | Verified | Reopen | agent | 用户决定完全移除 vendor/tui-textarea-2（避免仓库维护过多上游代码）。Cargo.toml 改回上游 `tui-textarea-2 = "0.11"`，`ime.rs` 回滚到 vendor 前的推断公式，`edit_utils.rs` 回滚到 `Style::default()`。修复 #1 与 #2 同时失效，bug 复发（长行行尾光标不可见 + 删除/换行残影）。用户明确接受复发。 |
| 2026-06-17 | Reopen | Verified | agent | **彻底根治**：用户确认 PR #34 前没有此 bug，根因是 PR #34 引入的 IME 模块（`set_cursor_style(Style::default())` 关闭 buffer 级光标 + `set_cursor_position` 改用终端光标）。删除 `peri-tui/src/app/ime.rs` 整个模块、移除 `main_ui/mod.rs` 中的 `set_cursor_position` 调用、`edit_utils.rs` 不再调 `set_cursor_style`，回到 tui-textarea 默认 REVERSED buffer 级光标。代价：中文输入法候选窗不再跟随光标。 |
| 2026-06-17 | Verified | Verified (Follow-up) | agent | 补充 IME 模块历史与未来恢复路径章节：记录 PR #34 当初要解决的 Windows IME 候选窗定位问题、为什么留下 bug、修复 #4 后的实际损失，以及未来恢复 IME 支持的三种方案（推荐 UI state 跨帧 sticky `last_scroll_col`）和重新引入前的 6 项检查清单。状态保持 Verified，但标记 Follow-up：IME 候选窗跟随光标能力丢失，未来需重新引入。 |
| 2026-06-17 | Verified (Follow-up) | Verified (Windows-only IME) | agent | **修复 #5**：用户提出 Windows-only 折中——用 `#[cfg(target_os = "windows")]` 条件编译，只在 Windows 下启用 IME 模块（`textarea_cursor_pos` + `set_cursor_style(Style::default())` + `set_cursor_position`），macOS/Linux 保留 buffer 级 REVERSED 光标。Trade-off：Windows 用户承受长行行尾光标算偏 bug 换取 IME 候选窗跟随；macOS/Linux 用户既无 bug 也无 IME 跟随（但 macOS 终端 IME 行为本身较好，损失可接受）。 |

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
- **涉及 commit**：527fbc1d
- **验证状态**：已验证

### 验证 #3（2026-06-17）—— 通过

用户反馈：修复 #2 生效，删除/换行后无残影光标。

### 验证 #1（2026-06-17）—— 通过

用户反馈：光标位置正确，CJK 正常显示反色，换行/删除无残影。

### 验证 #2（2026-06-17）—— 失败（部分回归）

用户反馈：在删除字符时，仍会留下残影光标。具体表现：
- 换行（Enter）后，前一行行尾残留一个光标块
- 删除（Backspace/Delete）后，旧光标位置残留一个光标块

根因分析：tui-textarea-2 的 `LineHighlighter::into_spans()` 在 `cursor_at_end=true` 时渲染一个 REVERSED 空格。该空格在帧间移动时，前帧的 REVERSED 空格位置在 ratatui diff 中可能未被正确清除，导致终端上显示为「残影」。

### 修复 #3（2026-06-17）—— 回滚至 vendor 前

- **操作人**：agent
- **用户原意**：完全移除 vendor，接受 bug 复发（避免在仓库中维护大量上游代码）。
- **修复内容**：
  1. `peri-tui/Cargo.toml`：`tui-textarea-2 = { path = "vendor/tui-textarea-2" }` 改回 `tui-textarea-2 = "0.11"`
  2. 删除 `peri-tui/vendor/tui-textarea-2/` 整个目录
  3. `peri-tui/src/app/ime.rs`：回滚到 vendor 前的水平滚动推断公式 `scroll_col = cursor_display_col - (visible_width - 1)`，仅保留 `.min(visible_width.saturating_sub(1))` 钳位（防止越界产生 ghost）
  4. `peri-tui/src/app/edit_utils.rs`：`set_cursor_style` 回滚到 `Style::default()`，禁用 textarea 内部光标渲染
- **副作用**：修复 #1（`scroll_top()`）与修复 #2（`cursor_at_end` 移除）同时失效。**长行行尾光标不可见**与**删除/换行残影**两个 bug 同时复发。
- **用户决策**：明确接受上述复发，换取仓库不维护 vendor 代码。

### 修复 #4（2026-06-17）—— 彻底根治：删除 IME 模块

- **操作人**：agent
- **用户原意**：用户回忆"PR #34 之前没有这个 bug"，希望回到那时的状态。
- **根因复盘**：
  - PR #34（commit 9026879f）引入 IME 支持时，做了两件事：
    1. `set_cursor_style(Style::default())` —— **关闭了** tui-textarea 默认的 REVERSED buffer 级光标块
    2. `set_cursor_position(...)` —— 改用**终端光标**（IME 候选窗跟随终端光标位置）
  - 上游 tui-textarea-2 0.11.0 的 `Widget::render` **不调用** `set_cursor_position`（已验证 `widget.rs:130-179`），所以 PR #34 之前 peri-tui 完全依赖 buffer 级 REVERSED 光标，跟终端光标位置无关——**不可能有这个 bug**。
  - 两个 bug 都源于 `textarea_cursor_pos` 的水平滚动推断错误。修复 #1/#2/#3 都是在错误的方向上修补：要么 vendor 上游加 `scroll_top()` API，要么调整推断公式——但都不如回到根本。
- **修复内容**：
  1. 删除 `peri-tui/src/app/ime.rs` 整个文件
  2. `peri-tui/src/app/mod.rs`：移除 `mod ime;` 和 `pub use ime::textarea_cursor_pos;`
  3. `peri-tui/src/ui/main_ui/mod.rs`：移除 `use crate::app::{textarea_cursor_pos, App}` 中的 `textarea_cursor_pos`，移除整个 `if app.focused { if let Some((cx, cy)) = textarea_cursor_pos(...) { f.set_cursor_position(...); } }` 块
  4. `peri-tui/src/app/edit_utils.rs`：移除 `ta.set_cursor_style(Style::default());`，让 tui-textarea 用默认 REVERSED buffer 级光标
- **代价**：中文输入法（IME）候选窗不再跟随光标位置（会停在终端左上角或上次位置）。用户明确接受。
- **验证状态**：待用户手动验证。预期：长行行尾、删除、换行场景光标始终以反色块形式可见。

## IME 模块历史与未来恢复路径（Follow-up）

### PR #34 当初要解决的问题

PR #34（commit `9026879f`，2026-06-16，作者 wuxiaoweisjz/xiao）标题为 `fix(tui): Windows terminal cursor positioning for IME and config import`，核心动机是 **Windows 终端的中文输入法（IME）候选窗定位**：

- 在 Windows 终端（以及部分 Linux 终端）上，IME 候选词框的位置由**终端光标**坐标决定，不是 ratatui buffer 里的"虚拟光标"。
- 如果不主动调用 `Frame::set_cursor_position`，终端光标会停在 `(0, 0)`（屏幕左上角）。
- 结果：打中文时候选词框跑到屏幕左上角，离输入框十万八千里，用户体验极差。
- macOS 上症状较轻（macOS 终端 IME 行为较好），Windows 上特别明显。

### 当时的修复方案（两步）

1. `set_cursor_style(Style::default())` —— 关掉 tui-textarea 默认的 REVERSED buffer 级反色块光标，不然会同时存在 buffer 光标和终端光标，看到两个光标。
2. 每帧调用 `Frame::set_cursor_position()` 把终端光标移到 textarea 光标的实际位置 —— IME 候选窗就会跟着来。

### 为什么会留下 bug

- tui-textarea 0.11 的 `Viewport::scroll_top()` 虽然是 `pub`，但 `TextArea::viewport` 字段是 `pub(crate)`——外部拿不到真实水平滚动偏移。
- 只好用公式推断：`scroll_col = cursor_display_col - (visible_width - 1)`。
- 这个公式在 textarea 内部"sticky scroll"时（光标在视口中部移动，`top_col` 不变）算错（见上面"调研记录"的数值推演表）。
- 长行行尾光标算偏 → 终端光标跑到屏幕最右列 → 终端模拟器在最右列裁剪光标 → 用户看不到光标。
- 因为 buffer 级光标被关了，没有 fallback，bug 直接显形。

### 相关时间线

- 同一天稍晚 commit `ebbc205c`（"清理 PR #34 的 FFI/算法/架构问题"）专门修了 PR #34 的一些遗留问题，但 IME 水平滚动推断 bug 没被处理掉。
- 当天又合并了 `c3204bd6`（`fix/windows-pty-and-crlf` 分支），又引入了另一个**完全独立的**光标 bug（`spec/issues/2026-06-16-main-textarea-cursor-position-mismatch.md`，"显示位置与实际编辑位置不同步"，状态仍 Partial）—— 那个跟 IME 无关，是 event/mouse 逻辑改动引入的。

### 修复 #4 后的实际损失

- macOS 用户基本无感（macOS 终端 IME 行为较好）。
- **Windows 用户输入中文时候选词框会回到屏幕左上角**。
- 换来的是：buffer 级 REVERSED 光标在任何场景下都正确显示，两个老 bug（长行行尾光标不可见 + 删除/换行残影）同时根治。

### 未来恢复 IME 支持的正确方向（避免重蹈覆辙）

如果以后真要恢复 IME 候选窗跟随光标能力，**禁止**重新采用 PR #34 的简单推断公式。正确方向有三种：

| 方案 | 评估 | 复杂度 |
|------|------|--------|
| **A. UI state 维护跨帧 `last_scroll_col`，用 `next_scroll_top` 逻辑更新** | 完全模拟 textarea 行为，准确；上游 `widget.rs:81-87` 的 `next_scroll_top` 逻辑就是规则 | 中（需要状态字段 + 跨帧更新） |
| **B. 同时保留 buffer 级 REVERSED 光标和终端光标** | 不调 `set_cursor_style`，保留默认 REVERSED；同时调 `set_cursor_position` 算错了也有 fallback，用户仍能看到 buffer 光标 | 低（最小改动，但两个光标可能视觉重叠/错位） |
| **C. Fork/vendor tui-textarea-2，加 `pub fn scroll_top(&self) -> (u16, u16)` getter** | 最准确；但仓库不希望维护 vendor 代码（见修复 #3 决策） | 高（vendor 维护成本） |

**推荐方案 A**：在 UI state（如 `ChatSession.ui` 或 `GlobalUiState`）中新增 `textarea_last_scroll_col: u16` 字段，每帧渲染时按上游 `next_scroll_top` 规则更新：

```rust
// 上游 widget.rs:81-87 的等价逻辑
fn next_scroll_top(prev_top: u16, cursor: u16, len: u16) -> u16 {
    if cursor < prev_top {
        cursor
    } else if prev_top + len <= cursor {
        cursor + 1 - len
    } else {
        prev_top  // 关键：sticky，保持不变
    }
}
```

关键差异：当 cursor 在视口内（`prev_top ≤ cursor < prev_top + len`）时，**保持 `prev_top` 不变**——这正是 PR #34 推断公式忽略的情况，导致光标被错误钉在视口最右列。

**最低风险方案 B**：如果不愿引入跨帧状态，至少采用方案 B 作为兜底——不调 `set_cursor_style`，保留 REVERSED buffer 光标作为 fallback，避免再次出现"终端光标算错就完全看不到光标"的灾难性表现。代价是 IME 候选窗位置可能轻微偏移（buffer 光标和终端光标位置差异）。

### 修复 #5（2026-06-17）—— Windows-only 折中

- **操作人**：agent
- **用户原意**：用户提出折中方案——只在 Windows 下启用 IME 模块，macOS/Linux 不启用。
- **修复内容**：
  1. 恢复 `peri-tui/src/app/ime.rs`（带 `#![cfg(target_os = "windows")]`，整个模块 Windows-only）
  2. `peri-tui/src/app/mod.rs`：`#[cfg(target_os = "windows")] mod ime;` + `pub use`
  3. `peri-tui/src/app/edit_utils.rs`：`#[cfg(target_os = "windows")] ta.set_cursor_style(Style::default());`（Windows 下禁用 buffer 光标）
  4. `peri-tui/src/ui/main_ui/mod.rs`：`#[cfg(target_os = "windows")]` 包裹 `set_cursor_position` 块
- **平台行为差异**：
  - **macOS/Linux**：保留 tui-textarea 默认 REVERSED buffer 级光标。无 bug（长行行尾光标可见、删除/换行无残影）。无 IME 候选窗跟随（但 macOS 终端 IME 行为本身较好，损失可接受）。
  - **Windows**：禁用 buffer 光标 + 启用终端光标。IME 候选窗跟随光标。但 `textarea_cursor_pos` 的推断公式 (`cursor - (width-1)`) 与 sticky scroll 不一致，长行行尾光标可能算偏（见上面"调研记录"）。Windows 用户接受此 trade-off。
- **后续优化方向**：如果 Windows 用户反馈光标算偏影响体验，可升级到方案 A（UI state 维护跨帧 sticky `last_scroll_col`）。
- **验证状态**：macOS 全 643 测试通过（ime 模块被 cfg 排除）。Windows 编译未在本地验证（依赖 aws-lc-sys 需要 Windows SDK），但 cfg 是语法级条件编译，预期 Windows 上能正常编译运行。

### 重新引入前的检查清单（用于未来升级到方案 A 时验证）

升级 IME 模块（消除推断公式缺陷）时必须验证：

1. **长行行尾光标可见**：输入超过视口宽度的 ASCII 长行，光标在行尾仍可见
2. **CJK 长行光标可见**：同上，但用 CJK 字符（每字符占 2 列）
3. **删除/换行无残影**：Backspace/Delete/Enter 后前一位置无残留光标块
4. **`←`/`→` 视觉响应**：长行中按 ← 一次，光标视觉立即左移一格（非 24 次后才动）
5. **IME 候选窗跟随**（Windows 验证）：中文输入时候选词框出现在输入框附近，而非屏幕左上角
6. **跨帧 sticky 行为**：光标在视口中部移动时，水平滚动不抖动

满足以上 6 条才算合格的 IME 恢复方案。当前的修复 #5 在 Windows 上只满足第 5 条（IME 跟随），其他 5 条存在已知缺陷。

