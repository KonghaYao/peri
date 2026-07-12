# message_area.rs 重构实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 `peri-tui/src/kit/message_area.rs`（1782 行单文件）重组为 `peri-tui/src/kit/message_area/` 子目录，6 个职责清晰的子模块，100% 行为不变。

**Architecture:** 按职责横向拆分（mod/render/selection/scroll/footer/props）。`use_event_handler` 和 `use_effect` 闭包改为薄壳委托模块级函数。`extract_visual_range` 从组件函数体内的嵌套函数提升为 `selection.rs` 的模块级函数。所有 28 个 hooks.use_* 调用种类和顺序 byte-for-byte 保持。

**Tech Stack:** Rust 2021、ratatui-kit（reactive 函数组件 + hooks）、peri-tui 内部 crate（atoms / text_selection / tui_render_unit / markdown / focus_router / welcome）。

**Spec:** `docs/superpowers/specs/2026-07-12-message-area-refactor-design.md`

---

## 文件结构

```
peri-tui/src/kit/message_area/  ← 新建目录
├── mod.rs          (~320 行) MessageArea 组件 + hook 装配 + 视口裁剪渲染骨架
├── render.rs       (~450 行) TuiRenderUnit → Vec<Line> 转换 + 摘要辅助 + 间距辅助
├── selection.rs    (~270 行) wrap_map + viewport_logical_range + 选区 + 剪贴板
├── scroll.rs       (~280 行) ScrollThrottle/DragThrottle + 鼠标事件 + 吸底 effect
├── footer.rs       (~200 行) build_footer_lines + TodoItem
└── props.rs        (~130 行) MessageAreaProps + MsgAreaTracker + ScrollbarFields/Hook + mouse_in_area
```

**搬运顺序（叶子优先）**：props → footer → render → selection → scroll → mod.rs 收尾。每个 task 后 `cargo build -p peri-tui` + `cargo test -p peri-tui --lib` 验证。

**搬迁铁律（来自 spec §3 trap 表）**：
- `lines_cache.2` / `wrap_map_cache.2` 必须保持 `Arc<Vec<...>>`，**不得改为 `Vec`**
- Drag/Up 事件中 `selection_down_pos.read()` / `text_sel.read()` 必须**先 copy 出 owned 值，drop guard，再 write**——parking_lot 同 thread 死锁
- 所有缓存更新用 `write_no_update()`，不用 `write()`
- 28 个 hook 调用顺序在 mod.rs 中 byte-for-byte 保持

---

## Task 0: 准备工作 + 记录基线

**Files:**
- 无修改，仅记录基线

- [ ] **Step 1: 确认当前 cargo test 基线**

```bash
cargo test -p peri-tui --lib 2>&1 | tail -5
```

记录基线 passed 数量（应为 ~394）。后续每个 task 完成后必须达到同样数量。

- [ ] **Step 2: 确认 git 工作区状态**

```bash
git status peri-tui/src/kit/message_area.rs
```

确认当前文件状态。如果存在未提交改动（文本选区/视口裁剪相关），**用户已表态"我会清理的"——本计划假设文件已是干净状态，或用户自行处理未提交改动后再执行本计划**。如果文件有未提交改动，先停下来询问用户是否已 commit。

---

## Task 1: 创建子目录结构

**Files:**
- Delete: `peri-tui/src/kit/message_area.rs`
- Create: `peri-tui/src/kit/message_area/mod.rs`（内容 = 原 `message_area.rs` 全部）
- Create: `peri-tui/src/kit/message_area/{render,selection,scroll,footer,props}.rs`（空文件）

**目标**：仅做物理位置迁移，单文件 → 单 mod.rs，仍然保持编译通过。后续 task 才把代码分拆到子模块。

- [ ] **Step 1: 创建目录并迁移文件**

```bash
mkdir -p peri-tui/src/kit/message_area
git mv peri-tui/src/kit/message_area.rs peri-tui/src/kit/message_area/mod.rs
```

`git mv` 保留 git rename 检测，diff 更可读。

- [ ] **Step 2: 创建 5 个空的子模块文件**

为每个文件写入一行占位注释（避免空文件被 cargo 警告）：

```bash
cat > peri-tui/src/kit/message_area/render.rs <<'EOF'
//! TuiRenderUnit → Vec<Line> 渲染（待 Task 4 填充）。
EOF
cat > peri-tui/src/kit/message_area/selection.rs <<'EOF'
//! 文本选区 + 折行映射（待 Task 5 填充）。
EOF
cat > peri-tui/src/kit/message_area/scroll.rs <<'EOF'
//! 滚动节流 + 鼠标事件 + 吸底（待 Task 6 填充）。
EOF
cat > peri-tui/src/kit/message_area/footer.rs <<'EOF'
//! Footer + Todo（待 Task 3 填充）。
EOF
cat > peri-tui/src/kit/message_area/props.rs <<'EOF'
//! Props + 位置 Hook + 滚动条 Hook（待 Task 2 填充）。
EOF
```

- [ ] **Step 3: 暂不添加 `mod` 声明**

mod.rs 顶部**先不加** `mod render;` 等声明——空文件加进去也能编译，但为了减小本步 diff，留到后续 task 真正使用时再加。

- [ ] **Step 4: 验证编译**

```bash
cargo build -p peri-tui 2>&1 | tail -10
```

预期：成功（message_area 是私有模块，路径从 `kit/message_area` 到 `kit/message_area/mod.rs` 是 Rust 默认目录模式，外部调用路径 `crate::kit::message_area::X` 不变）。

- [ ] **Step 5: 验证测试**

```bash
cargo test -p peri-tui --lib 2>&1 | tail -5
```

预期：与基线相同的 passed 数。

- [ ] **Step 6: Commit**

```bash
git add peri-tui/src/kit/message_area/
git commit -m "refactor(tui): message_area 单文件迁移到 message_area/ 目录

物理位置迁移，零代码变化。后续 task 将代码拆分到 6 个子模块。"
```

---

## Task 2: 搬迁 props.rs（叶子模块）

**Files:**
- Modify: `peri-tui/src/kit/message_area/props.rs`（从占位变为完整内容）
- Modify: `peri-tui/src/kit/message_area/mod.rs`（删除搬迁代码 + 加 `mod props;` + `use props::*`）

**搬迁内容**（mod.rs 当前行号）：
- 226–230: `fn mouse_in_area(...)`
- 234–248: `struct MsgAreaTracker` + `impl MsgAreaTracker::new()` + `impl Hook for MsgAreaTracker`
- 253–258: `struct ScrollbarFields`
- 263–299: `struct ScrollbarHook` + `impl Hook for ScrollbarHook`
- 303–306: `pub struct MessageAreaProps`

- [ ] **Step 1: 写入 props.rs 完整内容**

把 mod.rs 226–306 行（含 `mouse_in_area`、`MsgAreaTracker`、`ScrollbarFields`、`ScrollbarHook`、`MessageAreaProps`）**原样**搬到 `peri-tui/src/kit/message_area/props.rs`。

可见性调整：
- `pub struct MessageAreaProps` 保持 `pub`（外部 mod.rs 会 `pub use`）
- `struct MsgAreaTracker` 改为 `pub(super) struct MsgAreaTracker`
- `struct ScrollbarFields` 改为 `pub(super) struct ScrollbarFields`（mod.rs 中 `scrollbar_fields: use_state(ScrollbarFields::default)` 需要访问）
- `struct ScrollbarHook` 改为 `pub(super) struct ScrollbarHook`
- `fn mouse_in_area` 改为 `pub(super) fn mouse_in_area`

props.rs 必备 imports（参考 mod.rs 1–39 行的 imports，挑出 props.rs 实际使用的）：

```rust
use ratatui_kit::prelude::State;
use ratatui_kit::ratatui::layout::Rect;
use ratatui_kit::ratatui::style::{Modifier, Style};
use ratatui_kit::ratatui::widgets::Scrollbar as RatatuiScrollbar;
use peri_theme::atoms::THEME_ATOM;
```

`Hook` trait 和 `ComponentDrawer` 通过 `ratatui_kit::prelude::*` 或具体路径 import（与 mod.rs 现有写法一致）。

`Props` derive macro 的 import：与 mod.rs 一致（通常通过 `ratatui_kit::prelude::*`）。

- [ ] **Step 2: 修改 mod.rs**

(a) 在 mod.rs 顶部 imports 区下方（约第 39 行后）添加：

```rust
mod props;
use props::{MessageAreaProps, MsgAreaTracker, ScrollbarFields, ScrollbarHook, mouse_in_area};
```

(b) 删除 mod.rs 中已搬迁的代码块：226–306 行（mouse_in_area / MsgAreaTracker / ScrollbarFields / ScrollbarHook / MessageAreaProps）。删除后 mod.rs 行号会变化，后续 task 引用新行号。

(c) mod.rs 顶部 `use ratatui_kit::ratatui::widgets::Scrollbar` 等如果只被 ScrollbarHook 使用，可以从 mod.rs imports 中移除（避免 unused import 警告）。

- [ ] **Step 3: 验证编译**

```bash
cargo build -p peri-tui 2>&1 | tail -10
```

预期：成功。如果有 unused import 警告，按提示清理 mod.rs 顶部 imports。

- [ ] **Step 4: 验证测试**

```bash
cargo test -p peri-tui --lib 2>&1 | tail -5
```

预期：与基线相同 passed 数。

- [ ] **Step 5: Commit**

```bash
git add peri-tui/src/kit/message_area/mod.rs peri-tui/src/kit/message_area/props.rs
git commit -m "refactor(tui): 拆出 message_area/props.rs

搬迁 MessageAreaProps / MsgAreaTracker / ScrollbarFields / ScrollbarHook / mouse_in_area 到独立子模块。零行为变化。"
```

---

## Task 3: 搬迁 footer.rs（叶子模块）

**Files:**
- Modify: `peri-tui/src/kit/message_area/footer.rs`
- Modify: `peri-tui/src/kit/message_area/mod.rs`

**搬迁内容**（mod.rs 当前行号，注意 Task 2 后行号已变化）：
- `TodoStatus` enum
- `TodoItem` struct
- `fn hash_todo_items`
- `fn render_todo_lines`
- `fn build_footer_lines`
- 测试中的：`test_render_todo_lines_icons_and_crossed`、`test_render_todo_lines_empty`、`test_spinner_summary_after_loading_ends`、`test_token_count_no_write_when_unchanged`、`test_footer_loading_steady_state_has_no_control_state_transition`

- [ ] **Step 1: 用 grep 获取 Task 2 后的精确行号**

```bash
grep -n "^pub enum TodoStatus\|^pub struct TodoItem\|^fn hash_todo_items\|^pub fn render_todo_lines\|^fn build_footer_lines\|^// ── footer\|^// ── Todo\|^#\[cfg(test)\]" peri-tui/src/kit/message_area/mod.rs
```

记录每段的起止行号。

- [ ] **Step 2: 写入 footer.rs 完整内容**

把上述 5 个函数/类型 + 5 个测试**原样**搬到 `peri-tui/src/kit/message_area/footer.rs`。

可见性调整：
- `pub enum TodoStatus` 保持 `pub`
- `pub struct TodoItem` 保持 `pub`
- `fn hash_todo_items` 改为 `pub(super) fn hash_todo_items`（mod.rs 仍调用）
- `pub fn render_todo_lines` 改为 `pub(super) fn render_todo_lines`（mod.rs 调用；如果原本就是 `pub` 想保留也可以，但 `pub(super)` 更精确）
- `fn build_footer_lines` 改为 `pub(super) fn build_footer_lines`

footer.rs 必备 imports（从 mod.rs 1–39 行挑出 footer 实际使用的）：

```rust
use std::hash::{Hash, Hasher};
use std::time::Instant;

use crate::i18n;
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;
use peri_widgets::spinner::{SpinnerMode, SpinnerState};
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::style::{Modifier, Style};
use ratatui_kit::ratatui::text::{Line, Span};

use crate::kit::atoms::{LOADING_EPOCH, BRIDGE_RESET_COUNTER};
```

测试模块的 5 个 test 函数放在 footer.rs 末尾的 `#[cfg(test)] mod tests { use super::*; ... }`。

- [ ] **Step 3: 修改 mod.rs**

(a) 在 mod.rs 顶部 imports 区下方添加：

```rust
mod footer;
pub use footer::{TodoItem, TodoStatus};
use footer::{build_footer_lines, hash_todo_items, render_todo_lines};
```

`pub use footer::{TodoItem, TodoStatus}` 保证外部 5 处通过 `crate::kit::message_area::TodoItem`/`TodoStatus` 调用的路径完全不变。

(b) 删除 mod.rs 中已搬迁的代码块（TodoStatus/TodoItem/hash_todo_items/render_todo_lines/build_footer_lines）和测试中相关 5 个 test 函数。

- [ ] **Step 4: 验证外部调用路径未破坏**

```bash
cargo build -p peri-tui 2>&1 | grep -E "error|warning: unused" | head -20
```

特别检查 `atoms.rs`、`acp_events.rs`、`acp_notifier.rs`、`thread_load_consumer.rs`、`submit_consumer.rs` 这 5 个引用 `TodoItem`/`TodoStatus` 的文件是否仍编译通过。

- [ ] **Step 5: 验证测试**

```bash
cargo test -p peri-tui --lib 2>&1 | tail -5
```

预期：与基线相同 passed 数（5 个 footer 测试现在从 footer.rs 运行）。

- [ ] **Step 6: Commit**

```bash
git add peri-tui/src/kit/message_area/mod.rs peri-tui/src/kit/message_area/footer.rs
git commit -m "refactor(tui): 拆出 message_area/footer.rs

搬迁 TodoItem / TodoStatus / hash_todo_items / render_todo_lines / build_footer_lines 及 5 个相关测试到独立子模块。外部调用路径 crate::kit::message_area::TodoItem 保持不变。"
```

---

## Task 4: 搬迁 render.rs（叶子模块）

**Files:**
- Modify: `peri-tui/src/kit/message_area/render.rs`
- Modify: `peri-tui/src/kit/message_area/mod.rs`

**搬迁内容**：
- `COLLAPSED_BY_DEFAULT`、`AUTO_EXPAND`、`FORCE_EXPAND_ON_COMPLETE` 常量
- `compact_summary`、`compact_output_lines`、`format_running_duration`、`diff_change_summary`、`truncate_str` 辅助函数
- `vm_to_lines`
- 9 个变体函数：`render_reasoning_block`、`render_reminder_condensed`、`render_tool_card_lines`、`render_system_note_lines`、`render_subagent_group_lines`、`render_collapsed_group_lines`、`render_divider_lines`、`render_ask_user_block_lines`
- `trim_trailing_blank_lines`、`with_message_spacing`

- [ ] **Step 1: 用 grep 获取精确行号**

```bash
grep -n "^const COLLAPSED\|^const AUTO_EXPAND\|^const FORCE_EXPAND\|^fn compact_summary\|^fn compact_output_lines\|^fn format_running_duration\|^fn diff_change_summary\|^fn truncate_str\|^// ── vm_to_lines\|^fn vm_to_lines\|^// ── 各变体渲染\|^fn render_\|^// ── 消息间距\|^fn trim_trailing_blank_lines\|^fn with_message_spacing\|^// ── 组件" peri-tui/src/kit/message_area/mod.rs
```

- [ ] **Step 2: 写入 render.rs 完整内容**

把上述所有内容**原样**搬到 `peri-tui/src/kit/message_area/render.rs`。

可见性调整：
- 所有函数改为 `pub(super) fn ...`（mod.rs 调用 `vm_to_lines` 和 `with_message_spacing`，其余通过 `vm_to_lines` 间接调用）
- 3 个常量保持私有（`const COLLAPSED_BY_DEFAULT: &[&str] = ...`）——它们仅在 `render_tool_card_lines` 中使用，无需 `pub(super)`

render.rs 必备 imports：

```rust
use crate::i18n;
use crate::kit::tui_render_unit::{
    TuiAskUserBlock, TuiCollapsedGroup, TuiDivider, TuiHunkLineKind, TuiRenderUnit,
    TuiSubAgentGroup, TuiSystemNote, TuiToolCard,
};
use fluent_bundle::FluentValue;
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::style::{Modifier, Style};
use ratatui_kit::ratatui::text::{Line, Span};
use ratatui_kit::ratatui::widgets::{Paragraph, Wrap};
```

- [ ] **Step 3: 修改 mod.rs**

(a) 在 mod.rs 顶部 imports 区下方添加：

```rust
mod render;
use render::{vm_to_lines, with_message_spacing};
```

（如果 mod.rs 还需要 `trim_trailing_blank_lines` 等，按需添加——但通常仅 `vm_to_lines` + `with_message_spacing` 是对外使用的）

(b) 删除 mod.rs 中已搬迁的代码块（所有渲染相关函数 + 常量）。

(c) 清理 mod.rs 顶部不再使用的 imports（如 `TuiAskUserBlock`、`TuiCollapsedGroup`、`TuiHunkLineKind`、`FluentValue`、`Paragraph`、`Wrap` 等如果只被 render 函数使用）。

- [ ] **Step 4: 验证编译**

```bash
cargo build -p peri-tui 2>&1 | tail -10
```

- [ ] **Step 5: 验证测试**

```bash
cargo test -p peri-tui --lib 2>&1 | tail -5
```

- [ ] **Step 6: Commit**

```bash
git add peri-tui/src/kit/message_area/mod.rs peri-tui/src/kit/message_area/render.rs
git commit -m "refactor(tui): 拆出 message_area/render.rs

搬迁 vm_to_lines + 9 个变体渲染函数 + 摘要辅助 + 间距辅助到独立子模块。"
```

---

## Task 5: 搬迁 selection.rs（叶子模块）

**Files:**
- Modify: `peri-tui/src/kit/message_area/selection.rs`
- Modify: `peri-tui/src/kit/message_area/mod.rs`

**搬迁内容**：
- `WrappedLineInfo` struct
- `build_wrap_map` 函数
- `visual_to_logical` 函数
- `viewport_logical_range` 函数
- `copy_to_clipboard` 函数
- `mark_copy_message` 函数
- `highlight_logical_range` 函数（确认其在 mod.rs 中的当前位置；如已被视口裁剪改造删除，则跳过）
- **`extract_visual_range`** 函数（当前是嵌套在 `MessageArea` 函数体内的 closure 外函数，提升为 `selection.rs` 的模块级函数）

- [ ] **Step 1: 检查 highlight_logical_range 是否仍存在**

```bash
grep -n "fn highlight_logical_range\|highlight_logical_range" peri-tui/src/kit/message_area/mod.rs
```

如果存在则搬迁；如果已被视口裁剪改造时内联到 mod.rs 渲染骨架中，则跳过（保留在 mod.rs 内联）。

- [ ] **Step 2: 用 grep 获取精确行号**

```bash
grep -n "^// ── wrap_map\|^struct WrappedLineInfo\|^fn build_wrap_map\|^fn visual_to_logical\|^fn viewport_logical_range\|^// ── 剪贴板\|^fn copy_to_clipboard\|^pub(super) fn mark_copy_message\|^    fn extract_visual_range\|^// ── 消息区位置追踪" peri-tui/src/kit/message_area/mod.rs
```

记录 `extract_visual_range` 的位置（嵌套在 MessageArea 函数体内的 `fn extract_visual_range`，前面有 4 个空格缩进）。

- [ ] **Step 3: 写入 selection.rs 完整内容**

把上述函数**原样**搬到 `peri-tui/src/kit/message_area/selection.rs`。

可见性调整：
- `struct WrappedLineInfo` 改为 `pub(super) struct WrappedLineInfo`（mod.rs 和 scroll.rs 都要用）
- 字段 `logical_idx`/`visual_start`/`visual_end` 改为 `pub(super)`
- `fn build_wrap_map` 改为 `pub(super) fn build_wrap_map`
- `fn visual_to_logical` 改为 `pub(super) fn visual_to_logical`
- `fn viewport_logical_range` 改为 `pub(super) fn viewport_logical_range`
- `fn highlight_logical_range`（如存在）改为 `pub(super) fn`
- `fn extract_visual_range` 改为 `pub(super) fn extract_visual_range`（**关键：去掉原本的 4 空格缩进，提到模块级；签名第 5 参数 `width: u16` 不变——它原本就是参数化的**）
- `fn copy_to_clipboard` 改为 `pub(super) fn copy_to_clipboard`
- `pub(super) fn mark_copy_message` 保持

selection.rs 必备 imports：

```rust
use std::cmp::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::kit::atoms::{COPY_CHAR_COUNT, COPY_MESSAGE_UNTIL};
use crate::kit::text_selection::{self, TextSelection};
use peri_theme::atoms::THEME_ATOM;
use ratatui_kit::ratatui::style::Style;
use ratatui_kit::ratatui::text::{Line, Span};
use ratatui_kit::ratatui::widgets::{Paragraph, Wrap};
```

注意 `text_selection::{self, TextSelection}` 的 `self` import：因为 `extract_visual_range` 调用 `text_selection::line_to_plain_text` 和 `text_selection::visual_col_to_byte_offset`。

- [ ] **Step 4: 修改 mod.rs**

(a) 在 mod.rs 顶部 imports 区下方添加：

```rust
mod selection;
use selection::{
    build_wrap_map, copy_to_clipboard, extract_visual_range, mark_copy_message,
    viewport_logical_range, visual_to_logical, WrappedLineInfo,
};
```

（按 mod.rs 实际使用的函数调整列表）

(b) 删除 mod.rs 中已搬迁的代码块：
- `WrappedLineInfo` / `build_wrap_map` / `visual_to_logical` / `viewport_logical_range`
- `copy_to_clipboard` / `mark_copy_message`
- `highlight_logical_range`（如存在）
- **嵌套的 `extract_visual_range`** —— 这是 mod.rs 中最关键的删除点。它在 `MessageArea` 函数体内（约 1233–1305 行附近，4 空格缩进），删除整个函数定义。

(c) 修改 `MessageArea` 内调用 `extract_visual_range` 的地方（鼠标 Up 事件中）——调用路径不变（因为 mod.rs `use selection::extract_visual_range` 了），但确认 `vis_width` 参数仍正确传入。

- [ ] **Step 5: 验证编译**

```bash
cargo build -p peri-tui 2>&1 | tail -10
```

特别注意：如果出现 `cannot find function extract_visual_range in this scope`，说明 mod.rs 中调用处未正确 resolve——检查 `use selection::extract_visual_range` 是否到位。

- [ ] **Step 6: 验证测试**

```bash
cargo test -p peri-tui --lib 2>&1 | tail -5
```

- [ ] **Step 7: Commit**

```bash
git add peri-tui/src/kit/message_area/mod.rs peri-tui/src/kit/message_area/selection.rs
git commit -m "refactor(tui): 拆出 message_area/selection.rs

搬迁 WrappedLineInfo / build_wrap_map / visual_to_logical / viewport_logical_range / extract_visual_range / copy_to_clipboard / mark_copy_message 到独立子模块。extract_visual_range 从组件函数体内提升为模块级函数。"
```

---

## Task 6: 搬迁 scroll.rs（依赖 selection + props）

**Files:**
- Modify: `peri-tui/src/kit/message_area/scroll.rs`
- Modify: `peri-tui/src/kit/message_area/mod.rs`

**搬迁内容**：
- `SCROLL_LINES`、`SCROLL_FRAME_MS` 常量
- `ScrollThrottle` struct + impl Default
- `DragThrottle` struct + impl Default
- **提取**：把 `use_event_handler` 闭包内的鼠标/键盘处理逻辑提取为 `pub(super) fn handle_event(...)`（9 参数函数）
- **提取**：把 `use_effect` 闭包内的吸底逻辑提取为 `pub(super) fn run_auto_follow(ctx: &AutoFollowCtx)` + `pub(super) struct AutoFollowCtx { ... }`
- **提取**：把 `apply_scroll` 内嵌闭包提取为 `fn apply_scroll(delta: i32, scroll_throttle: &State<ScrollThrottle>, scroll_state: &State<ScrollViewState>)`
- 测试中的：`proximity_check` + 7 个 `test_proximity_*`

- [ ] **Step 1: 用 grep 获取精确行号**

```bash
grep -n "^// ── 滚动速度\|^const SCROLL_LINES\|^const SCROLL_FRAME_MS\|^struct ScrollThrottle\|^impl Default for ScrollThrottle\|^// ── 拖拽选中节流\|^struct DragThrottle\|^impl Default for DragThrottle\|^    hooks.use_event_handler\|^    hooks.use_effect\|^// ── 吸底\|^// ── 鼠标事件处理\|^fn proximity_check\|^fn test_proximity" peri-tui/src/kit/message_area/mod.rs
```

- [ ] **Step 2: 写入 scroll.rs 的常量 + struct**

把以下内容**原样**搬到 `peri-tui/src/kit/message_area/scroll.rs` 顶部：

- `// ── 滚动速度控制` 注释 + `SCROLL_LINES`/`SCROLL_FRAME_MS` 常量
- `ScrollThrottle` struct + `impl Default for ScrollThrottle`
- `// ── 拖拽选中节流` 注释 + `DragThrottle` struct + `impl Default for DragThrottle`

可见性：
- `const SCROLL_LINES: u16 = 3;` 改为 `pub(super) const SCROLL_LINES: u16 = 3;`
- `const SCROLL_FRAME_MS: u64 = 16;` 改为 `pub(super) const SCROLL_FRAME_MS: u64 = 16;`
- `struct ScrollThrottle` 改为 `pub(super) struct ScrollThrottle`，字段 `last_flush`/`pending_delta` 改为 `pub(super)`
- `struct DragThrottle` 改为 `pub(super) struct DragThrottle`，字段 `last_flush` 改为 `pub(super)`

- [ ] **Step 3: 写入 scroll.rs 的 `apply_scroll` 私有函数**

```rust
/// 滚动节流：累积 delta，仅在距上次 flush ≥ SCROLL_FRAME_MS(16ms) 时推入 scroll_state。
/// write_no_update 不触发 notifier.wake()——依赖 dispatch 后 ratatui-kit loop 强制 render。
pub(super) fn apply_scroll(
    delta: i32,
    scroll_throttle: &State<ScrollThrottle>,
    scroll_state: &State<ScrollViewState>,
) {
    let mut st = scroll_throttle.write_no_update();
    st.pending_delta += delta;
    let now = Instant::now();
    if now.duration_since(st.last_flush) >= Duration::from_millis(SCROLL_FRAME_MS) {
        let pending = st.pending_delta;
        st.pending_delta = 0;
        st.last_flush = now;
        drop(st);
        if pending != 0 {
            let mut state = scroll_state.write_no_update();
            if pending > 0 {
                for _ in 0..(pending as u16) {
                    state.scroll_down();
                }
            } else {
                for _ in 0..((-pending) as u16) {
                    state.scroll_up();
                }
            }
        }
    }
}
```

**注意**：从 mod.rs 当前 `apply_scroll` 实现原样搬迁。如果当前实现略有不同（如返回值或细节），以 mod.rs 实际代码为准。

- [ ] **Step 4: 写入 scroll.rs 的 `handle_event` 函数**

把 mod.rs 中 `use_event_handler` 闭包体（约 240 行，包括鼠标 Down/Drag/Up 处理、键盘处理、PERI_DISABLE_DRAG_SELECT 分支、parking_lot 死锁规避）**原样**搬到一个 `pub(super) fn handle_event(...)` 函数体中。

**关键签名**：

```rust
pub(super) fn handle_event(
    event: &Event,
    area_rect: Option<Rect>,
    vis_width: u16,
    scroll_state: &State<ScrollViewState>,
    scroll_throttle: &State<ScrollThrottle>,
    text_sel: &State<TextSelection>,
    selection_down_pos: &State<Option<(u16, u16)>>,
    drag_throttle: &State<DragThrottle>,
    wrap_map_cache: &State<(u64, u16, Arc<Vec<WrappedLineInfo>>)>,
    lines_cache: &State<(u64, usize, Arc<Vec<Line<'static>>)>,
) -> EventResult {
    // 原 use_event_handler 闭包体原样搬迁
    // 包括：
    //   - Event::Key 路径：focus_router::message_accepts_key + scroll_state.write().handle_event
    //   - Event::Mouse::Moved 提前返回
    //   - apply_scroll 内嵌调用改为 super::apply_scroll(...) 或 apply_scroll(...)
    //   - PERI_DISABLE_DRAG_SELECT 环境变量检测
    //   - 鼠标 Down/Drag/Up 三态处理（含 parking_lot 死锁规避）
    //   - selection_down_pos.read() → copy 出 owned → drop guard → write
    //   - text_sel.read() → copy 出 owned → drop guard → write
    //   - 调用 super::extract_visual_range / super::copy_to_clipboard / super::mark_copy_message
    //   - 滚动处理（区域内外通用）
}
```

**搬迁约束**（必须保留，不得"清理"）：
1. `PERI_DISABLE_DRAG_SELECT` 分支保留
2. `drag_throttle.read()` + 16ms 节流逻辑保留
3. `selection_down_pos.write_no_update()`（不是 `write()`）
4. 同一 thread 不能同时 read+write guard：先 copy 出 owned 值（如 `let down_pos = *selection_down_pos.read();`），drop guard，再 write
5. Up 事件中先 copy `let dragging = text_sel.read().dragging;`，再判断；再 copy `let bounds = text_sel.read().normalized_bounds();`，drop guard，再处理
6. 提取后 `let bounds = ...` 路径中的 `extract_visual_range` 调用，wrap_map 和 lines 通过 `wrap_map_cache.read().2`（Arc deref）和 `lines_cache.read().2`（Arc deref）传入

- [ ] **Step 5: 写入 scroll.rs 的 `AutoFollowCtx` + `run_auto_follow`**

```rust
pub(super) struct AutoFollowCtx {
    pub total_visual_rows: u16,
    pub vis_height: u16,
    pub scroll_state: State<ScrollViewState>,
    pub prev_items_len: State<usize>,
    pub last_scrolled_at: State<u16>,
    pub items_len: usize,
    pub is_loading: bool,
}

pub(super) fn run_auto_follow(ctx: &AutoFollowCtx) {
    // mod.rs 当前 use_effect 闭包体原样搬迁
    // 注意：当前代码已删除 "prev == 0 && len > 0" 分支（视口裁剪改造时清理）
    // 保留所有其他逻辑：
    //   - total_visual_rows == 0 || vis_height == 0 → return
    //   - loading 分支：增量吸底 + last_scrolled_at 更新
    //   - len < prev 分支：scroll_to_bottom
    //   - 邻近检测：(vis_height / 4).max(5) 阈值
}
```

字段访问注意：原本 `*pl.read()` 改为 `*ctx.prev_items_len.read()`；`st.write().scroll_to_bottom()` 改为 `ctx.scroll_state.write().scroll_to_bottom()`；其余类同。

- [ ] **Step 6: 写入 scroll.rs 的 proximity 测试**

把 mod.rs 测试区中的 `proximity_check` 辅助函数 + 7 个 `test_proximity_*` 测试**原样**搬到 scroll.rs 末尾的 `#[cfg(test)] mod tests { use super::*; ... }`。

注意：`proximity_check` 在测试模块内部用 `(vis_height / 2).max(5)`（与生产代码 1/4 不一致）——保留现状，**不要顺手修**（spec 明确指出）。

- [ ] **Step 7: 写入 scroll.rs 必备 imports**

```rust
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::kit::focus_router;
use crate::kit::text_selection::TextSelection;
use ratatui_kit::components::ScrollViewState;
use ratatui_kit::crossterm::event::{Event, KeyEventKind, MouseButton, MouseEventKind};
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::layout::Rect;

use super::props::mouse_in_area;
use super::selection::{
    copy_to_clipboard, extract_visual_range, mark_copy_message, visual_to_logical, WrappedLineInfo,
};
```

- [ ] **Step 8: 修改 mod.rs 顶部 imports**

mod.rs 顶部添加：

```rust
mod scroll;
use scroll::{
    apply_scroll, handle_event, run_auto_follow, AutoFollowCtx, DragThrottle, ScrollThrottle,
    SCROLL_FRAME_MS, SCROLL_LINES,
};
```

注：`apply_scroll` 在 scroll.rs 内是 `pub(super)`（仅 scroll.rs 内部用），但 mod.rs 不需要直接调用——所以 mod.rs 的 `use scroll::{...}` 不包含 `apply_scroll`。修正：

```rust
mod scroll;
use scroll::{
    handle_event, run_auto_follow, AutoFollowCtx, DragThrottle, ScrollThrottle,
    SCROLL_FRAME_MS, SCROLL_LINES,
};
```

`apply_scroll` 保持 scroll.rs 内私有（`fn apply_scroll`，不是 `pub(super) fn`），通过 `handle_event` 内部调用。

- [ ] **Step 9: 修改 mod.rs 的 use_event_handler 闭包为薄壳**

把当前的 `hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| { ... 240 行 ... });` 改为：

```rust
hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
    if let Event::Key(key) = &event {
        let _ = focus_router::message_accepts_key(key);
    }
    scroll::handle_event(
        &event,
        area_rect,
        vis_width,
        &scroll_state,
        &scroll_throttle,
        &text_sel,
        &selection_down_pos,
        &drag_throttle,
        &wrap_map_cache,
        &lines_cache,
    )
});
```

注意：原闭包内的 `if let Event::Key(key) = &event { let _ = focus_router::message_accepts_key(key); }` 这一行移到薄壳中（因为 `handle_event` 内部已经也调用了 focus_router——查看实际实现，如果重复则只保留一处）。

**验证**：查看 mod.rs 当前 use_event_handler 闭包内的 focus_router 调用位置。如果薄壳和 handle_event 都调用，会导致重复调用——以 handle_event 内的为准，薄壳不调用。

修正薄壳（如果 handle_event 内已含 focus_router）：

```rust
hooks.use_event_handler(EventScope::Global, EventPriority::High, move |event| {
    scroll::handle_event(
        &event,
        area_rect,
        vis_width,
        &scroll_state,
        &scroll_throttle,
        &text_sel,
        &selection_down_pos,
        &drag_throttle,
        &wrap_map_cache,
        &lines_cache,
    )
});
```

- [ ] **Step 10: 修改 mod.rs 的 use_effect 闭包为薄壳**

把当前的 `hooks.use_effect({ ... 60 行 ... }, (items_len, vm_generation, is_loading));` 改为：

```rust
hooks.use_effect(
    {
        move || {
            scroll::run_auto_follow(&scroll::AutoFollowCtx {
                total_visual_rows,
                vis_height,
                scroll_state: scroll_state.clone(),
                prev_items_len: prev_items_len.clone(),
                last_scrolled_at: last_scrolled_at.clone(),
                items_len,
                is_loading,
            })
        }
    },
    (items_len, vm_generation, is_loading),
);
```

注意：`State<T>` 是 Arc 内核，`clone()` 是廉价引用拷贝；闭包 `move` 捕获 ctx 字段后，每次 effect 触发都重新构造 `AutoFollowCtx`。

deps 元组 `(items_len, vm_generation, is_loading)` 保持完全一致。

- [ ] **Step 11: 删除 mod.rs 中已搬迁的代码**

删除：
- `// ── 滚动速度控制` 段（`SCROLL_LINES`/`SCROLL_FRAME_MS` + `ScrollThrottle` + `impl Default`）
- `// ── 拖拽选中节流` 段（`DragThrottle` + `impl Default`）
- 测试中的 `proximity_check` + 7 个 `test_proximity_*`

注意：`use_event_handler` 闭包和 `use_effect` 闭包体已在 step 9–10 改写为薄壳；如果还有残留的旧闭包代码，删除。

清理 mod.rs 顶部不再使用的 imports（如 `KeyEventKind`、`MouseButton`、`MouseEventKind`、`Instant`、`Duration` 等如果只被搬迁代码使用）。

- [ ] **Step 12: 验证编译**

```bash
cargo build -p peri-tui 2>&1 | tail -20
```

**重点检查**：
- 如果出现 `cannot find function handle_event in this scope` → 检查 `use scroll::handle_event` 或调用 `scroll::handle_event(...)` 路径
- 如果出现 `expected lifetime 'static` → `AutoFollowCtx` 字段的 `State<T>` 应该是 Clone 的，无需 lifetime；检查 ctx 构造时是否漏 clone
- 如果出现 `borrowed as mutable multiple times` → parking_lot 死锁规避未正确搬迁，检查 `selection_down_pos.read()` 是否在 write 前已 drop

- [ ] **Step 13: 验证测试**

```bash
cargo test -p peri-tui --lib 2>&1 | tail -10
```

预期：与基线相同 passed 数（7 个 proximity 测试现在从 scroll.rs 运行）。

- [ ] **Step 14: Commit**

```bash
git add peri-tui/src/kit/message_area/mod.rs peri-tui/src/kit/message_area/scroll.rs
git commit -m "refactor(tui): 拆出 message_area/scroll.rs

搬迁 ScrollThrottle / DragThrottle / SCROLL_LINES / SCROLL_FRAME_MS / apply_scroll / handle_event / run_auto_follow / AutoFollowCtx 及 7 个 proximity 测试到独立子模块。use_event_handler 和 use_effect 闭包改为薄壳委托。parking_lot 死锁规避、PERI_DISABLE_DRAG_SELECT 分支保留。"
```

---

## Task 7: 最终验证 + 更新 CLAUDE.md

**Files:**
- Modify: `CLAUDE.md`（根目录，"核心文件树"段）

- [ ] **Step 1: 全量编译**

```bash
cargo build --workspace 2>&1 | tail -10
```

预期：成功，无错误。

- [ ] **Step 2: 全量测试**

```bash
cargo test --workspace --lib 2>&1 | tail -10
```

预期：基线 963 passed（2 个已存在 peri-middlewares 失败可忽略，与本次重组无关）。

- [ ] **Step 3: 检查文件行数**

```bash
wc -l peri-tui/src/kit/message_area/*.rs
```

预期总行数 ≈ 1782 + ~50（imports/mod 声明开销）。各文件大致规模：
- mod.rs: ~320 行
- render.rs: ~450 行
- selection.rs: ~270 行
- scroll.rs: ~280 行
- footer.rs: ~200 行
- props.rs: ~130 行

- [ ] **Step 4: 更新 CLAUDE.md 文件树**

修改 `CLAUDE.md` 中"核心文件树"段。找到：

```
peri-tui/src/kit/message_area.rs  # 消息区（ScrollView + 视口裁剪 + Todo）
```

替换为：

```
peri-tui/src/kit/message_area/  # 消息区（视口裁剪 + 文本选区 + Todo）
  mod.rs          # MessageArea 组件 + hook 装配 + 视口裁剪渲染骨架
  render.rs       # TuiRenderUnit → Vec<Line>
  selection.rs    # wrap_map / viewport_logical_range / 选区高亮 / 剪贴板
  scroll.rs       # ScrollThrottle / DragThrottle / 鼠标事件 / 吸底
  footer.rs       # build_footer_lines + TodoItem
  props.rs        # Props + MsgAreaTracker + ScrollbarFields/Hook
```

- [ ] **Step 5: 手动行为对比测试清单**

启动 TUI：`cargo run -p peri-tui`（或 `scripts/start-tui.sh`）。逐项验证：

- [ ] **长对话滚动**：发起多次对话（至少 30 轮），鼠标滚轮滚动顺畅，无卡顿
- [ ] **视口裁剪渲染**：长对话下消息区右侧滚动条正常显示/隐藏；滚动到不同位置时内容正确
- [ ] **鼠标拖拽选中**：鼠标在消息区按住左键拖拽，选区高亮（surface.selection 色），松开后状态栏显示"已复制 N 字符"
- [ ] **PERI_DISABLE_DRAG_SELECT=1**：`PERI_DISABLE_DRAG_SELECT=1 cargo run -p peri-tui` 启动，拖拽无选中（验证回退路径）
- [ ] **PERI_NO_HIGHLIGHT=1**：`PERI_NO_HIGHLIGHT=1 cargo run -p peri-tui` 启动，拖拽无高亮（验证回退路径）
- [ ] **历史回放**：`/history` 选择历史会话，恢复后消息区显示历史消息（注意：history-replay-scroll-too-early 是已知 Open issue，本次重组不修——保留现状即"停在中间偏上"是预期的）
- [ ] **Loading spinner**：发起对话时显示 spinner；结束后显示"Brewed for Ns"
- [ ] **Todo 列表**：触发 TodoWrite 工具时显示 ◼/✔/◻ 图标
- [ ] **Welcome 屏**：空对话显示 Welcome
- [ ] **Tool card 折叠/展开**：Bash/Read 等默认折叠，Agent/Write 等默认展开
- [ ] **消息间距**：消息块之间有空行分隔

- [ ] **Step 6: Commit CLAUDE.md 更新**

```bash
git add CLAUDE.md
git commit -m "docs(claude-md): 更新 message_area 文件树为子目录结构

message_area.rs 已重组为 message_area/{mod,render,selection,scroll,footer,props}.rs 子目录。"
```

---

## Task 8: 总结与交付

- [ ] **Step 1: 列出全部 commits**

```bash
git log --oneline main..HEAD
```

预期看到 7 个 commits（Task 1–7 各一个）。

- [ ] **Step 2: 确认外部调用路径未破坏**

```bash
grep -rn "crate::kit::message_area::" peri-tui/src/ | grep -v "peri-tui/src/kit/message_area/"
```

确认 5 处外部调用（atoms.rs / acp_events.rs / acp_notifier.rs / thread_load_consumer.rs / submit_consumer.rs）都只引用 `TodoItem` 或 `TodoStatus`——这两个通过 mod.rs `pub use` 重导出，路径不变。

- [ ] **Step 3: 完成报告**

实施完成后向用户报告：
- 6 个新文件总行数
- 测试通过数（应与基线一致）
- 手动验证清单完成情况
- CLAUDE.md 更新位置

---

## Self-Review

### Spec coverage 检查

| Spec 要求 | 实施 Task |
|----------|----------|
| §1 6 个子模块拆分 | Task 1（目录）+ Task 2–6（5 个子模块） |
| §1 详细归属表 | Task 2 (props) / Task 3 (footer) / Task 4 (render) / Task 5 (selection) / Task 6 (scroll) |
| §2 pub use 重导出 TodoItem/TodoStatus | Task 3 Step 3 |
| §2 pub use 重导出 MessageAreaProps | Task 2 Step 2（隐含——MessageAreaProps 在 mod.rs 顶部 `use props::MessageAreaProps`，但 spec 要求 `pub use`，需补） |
| §3 28 个 hook 调用顺序保持 | Task 2–6 各自搬迁后 cargo test 验证 |
| §3 Arc<Vec> 保留 | 搬迁铁律 + Task 5/6 验证 |
| §3 parking_lot 死锁规避保留 | Task 6 Step 4 搬迁约束 |
| §3 闭包改薄壳委托 | Task 6 Step 9 (event) + Step 10 (effect) |
| §3 extract_visual_range 提升为模块级 | Task 5 Step 3 |
| §4 测试归属 | Task 3 (footer 测试) + Task 6 (proximity 测试) + Task 7 (mod.rs empty 测试保留) |
| §4 验证步骤（编译/测试/手动） | Task 7 |
| §5 风险缓解 | 各 Task 的 cargo build + cargo test 步骤 |
| §6 CLAUDE.md 更新 | Task 7 Step 4 |

**遗漏修正**：Spec §2 要求 `pub use props::MessageAreaProps`。Task 2 Step 2 应改为：

```rust
mod props;
pub use props::MessageAreaProps;
use props::{MsgAreaTracker, ScrollbarFields, ScrollbarHook, mouse_in_area};
```

——已在 Task 2 Step 2 中隐含（"保持 `pub`"），但需要明确 `pub use`。实施时务必检查 mod.rs 顶部使用 `pub use props::MessageAreaProps;` 而非仅 `use`。

### Placeholder scan

无 TBD/TODO/占位符。所有步骤含具体代码或具体命令。

### Type consistency

- `WrappedLineInfo` 在 Task 5 定义为 `pub(super) struct`，Task 6 引用时签名一致
- `handle_event` 9 参数签名在 Task 6 Step 4 定义，Step 9 薄壳调用一致
- `AutoFollowCtx` 字段在 Task 6 Step 5 定义，Step 10 薄壳构造一致
- `extract_visual_range` 签名（5 参数）在 Task 5 Step 3 定义，原 mod.rs 调用一致

无类型不一致。
