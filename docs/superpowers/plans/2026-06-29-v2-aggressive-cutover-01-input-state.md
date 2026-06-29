# Plan 1: InputState 完整化 — 替代 tui_textarea::TextArea 作为状态源

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans`. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 `state_machine::input::InputState` 从「单行 buffer 概念验证」升级为完整替代 `tui_textarea::TextArea` 的状态源 — 支持多行、选择、单词/行删除、光标跳跃、剪贴板操作。

**Architecture:** `InputState` 持有 `Vec<String>` 多行 buffer + 字符级光标 `(row, col_byte)` + `Option<Selection>` 选择区间。所有 mutation 方法返回 `Vec<Effect>`（Render / CopyToClipboard）。`tui_textarea::TextArea` **保留为渲染 widget**，但状态从 `InputState` 同步注入（`textarea.from_state(&input)`）。

**Tech Stack:** Rust 2021，CJK 安全（chars() 而非字节索引），`unicode-segmentation` 用于单词边界。

**依赖**：无。是 Plan 2/3 的前置。

**Blocked by**：无。

---

## File Structure

| 文件 | 职责 | 处置 |
|------|------|------|
| `peri-tui/src/state_machine/input/mod.rs` | `InputState` 主结构 + 公共 API | **重写**（保留 `Attachment` / `AtMentionState` / `SlashCompletionState` 不变） |
| `peri-tui/src/state_machine/input/selection.rs` | `Selection` 类型 + 区间运算 | **新建** |
| `peri-tui/src/state_machine/input/cursor.rs` | `CursorPos` (row, col) + 移动逻辑 | **新建** |
| `peri-tui/src/state_machine/input/edit.rs` | 编辑操作（插入/删除/单词/行） | **新建** |
| `peri-tui/src/state_machine/input/clipboard.rs` | cut/copy/paste 逻辑 | **新建** |
| `peri-tui/src/state_machine/input/sync.rs` | `tui_textarea::TextArea` ↔ `InputState` 双向同步 | **新建** |
| `peri-tui/src/state_machine/input_test.rs` | 全量测试 | **新建**（≥30 测试） |

`mod.rs` 末尾 `mod.rs` 内的 `tests` 模块**删除**（迁到独立 `_test.rs` 文件）。

---

## Task 1: 定义 Selection 与 CursorPos 类型

**Files:**
- Create: `peri-tui/src/state_machine/input/selection.rs`
- Create: `peri-tui/src/state_machine/input/cursor.rs`
- Modify: `peri-tui/src/state_machine/input/mod.rs:1-15`（add `mod selection; mod cursor; pub use ...`）

- [ ] **Step 1: 写 `selection.rs` 失败测试**

Create `peri-tui/src/state_machine/input/selection_test.rs`:

```rust
use super::selection::{Selection, SelectionRange};

#[test]
fn test_selection_normal_anchor_le_cursor() {
    let s = Selection::normal(0, 0, 0, 5);
    assert_eq!(s.start(), (0, 0));
    assert_eq!(s.end(), (0, 5));
}

#[test]
fn test_selection_normalizes_reversed_anchor() {
    // 用户从后往前拖选：anchor (2,5) cursor (1,1)
    let s = Selection::normal(2, 5, 1, 1);
    assert_eq!(s.start(), (1, 1));
    assert_eq!(s.end(), (2, 5));
}

#[test]
fn test_selection_empty_when_anchor_eq_cursor() {
    let s = Selection::normal(0, 3, 0, 3);
    assert!(s.is_empty());
}

#[test]
fn test_selection_line_range_across_rows() {
    let s = Selection::normal(0, 10, 2, 5);
    let range = s.range();
    assert!(range.contains_row(0));
    assert!(range.contains_row(1));
    assert!(range.contains_row(2));
    assert!(!range.contains_row(3));
}
```

- [ ] **Step 2: 运行测试验证失败**

```bash
cargo test -p peri-tui --lib state_machine::input::selection
```
Expected: FAIL — `error[E0433]: failed to resolve: mod selection`

- [ ] **Step 3: 实现 `selection.rs`**

Create `peri-tui/src/state_machine/input/selection.rs`:

```rust
//! 选择区间类型。所有坐标都是 (row, col_byte)。

/// 文本选择区间。anchor 是用户按下鼠标的位置，cursor 是当前位置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub anchor_row: usize,
    pub anchor_col: usize,
    pub cursor_row: usize,
    pub cursor_col: usize,
}

/// 已规范化的范围（start ≤ end）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionRange {
    pub start_row: usize,
    pub start_col: usize,
    pub end_row: usize,
    pub end_col: usize,
}

impl Selection {
    pub fn normal(anchor_row: usize, anchor_col: usize, cursor_row: usize, cursor_col: usize) -> Self {
        Self { anchor_row, anchor_col, cursor_row, cursor_col }
    }

    /// 返回规范化后的范围（start ≤ end）。
    pub fn range(&self) -> SelectionRange {
        let (start_row, start_col, end_row, end_col) =
            if self.anchor_row < self.cursor_row
                || (self.anchor_row == self.cursor_row && self.anchor_col <= self.cursor_col)
            {
                (self.anchor_row, self.anchor_col, self.cursor_row, self.cursor_col)
            } else {
                (self.cursor_row, self.cursor_col, self.anchor_row, self.anchor_col)
            };
        SelectionRange { start_row, start_col, end_row, end_col }
    }

    pub fn start(&self) -> (usize, usize) {
        let r = self.range();
        (r.start_row, r.start_col)
    }

    pub fn end(&self) -> (usize, usize) {
        let r = self.range();
        (r.end_row, r.end_col)
    }

    pub fn is_empty(&self) -> bool {
        self.anchor_row == self.cursor_row && self.anchor_col == self.cursor_col
    }
}

impl SelectionRange {
    pub fn contains_row(&self, row: usize) -> bool {
        row >= self.start_row && row <= self.end_row
    }
}
```

- [ ] **Step 4: 运行测试验证通过**

```bash
cargo test -p peri-tui --lib state_machine::input::selection
```
Expected: PASS — 4 tests

- [ ] **Step 5: 写 `cursor.rs` 失败测试**

Create `peri-tui/src/state_machine/input/cursor_test.rs`:

```rust
use super::cursor::CursorPos;

#[test]
fn test_cursor_default_is_origin() {
    let c = CursorPos::default();
    assert_eq!(c.row, 0);
    assert_eq!(c.col_byte, 0);
}

#[test]
fn test_cursor_from_byte_offset_single_line() {
    let lines = vec!["hello".to_string()];
    let c = CursorPos::from_byte_offset(&lines, 3);
    assert_eq!(c.row, 0);
    assert_eq!(c.col_byte, 3);
}

#[test]
fn test_cursor_from_byte_offset_multiline() {
    let lines = vec!["ab".to_string(), "cde".to_string()];
    // byte offset 3 = 跨过 "ab\n" = (1, 0)
    let c = CursorPos::from_byte_offset(&lines, 3);
    assert_eq!(c.row, 1);
    assert_eq!(c.col_byte, 0);
}

#[test]
fn test_cursor_to_byte_offset_multiline() {
    let lines = vec!["ab".to_string(), "cde".to_string()];
    let c = CursorPos { row: 1, col_byte: 2 };
    assert_eq!(c.to_byte_offset(&lines), 3 + 2); // "ab\n" + "cd"[..2]
}

#[test]
fn test_cursor_clamp_to_line_end() {
    let lines = vec!["hi".to_string()];
    let c = CursorPos { row: 0, col_byte: 100 };
    let clamped = c.clamped(&lines);
    assert_eq!(clamped.col_byte, 2);
}
```

- [ ] **Step 6: 运行测试验证失败**

```bash
cargo test -p peri-tui --lib state_machine::input::cursor
```
Expected: FAIL

- [ ] **Step 7: 实现 `cursor.rs`**

Create `peri-tui/src/state_machine/input/cursor.rs`:

```rust
//! 光标位置 (row, col_byte)。col_byte 是字节偏移（CJK 安全由调用方保证）。

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CursorPos {
    pub row: usize,
    pub col_byte: usize,
}

impl CursorPos {
    pub fn new(row: usize, col_byte: usize) -> Self {
        Self { row, col_byte }
    }

    /// 从全 buffer 的字节偏移推导 (row, col)。
    pub fn from_byte_offset(lines: &[String], byte_offset: usize) -> Self {
        let mut remaining = byte_offset;
        for (row, line) in lines.iter().enumerate() {
            let line_len_with_newline = line.len() + 1; // +1 for '\n'
            if remaining <= line.len() {
                return Self { row, col_byte: remaining };
            }
            // 跨过这一行 + 换行符
            remaining = remaining.saturating_sub(line_len_with_newline);
        }
        // 越界：定位到最后一行末尾
        let last_row = lines.len().saturating_sub(1);
        let last_col = lines.last().map(|s| s.len()).unwrap_or(0);
        Self { row: last_row, col_byte: last_col }
    }

    /// 反向：把 (row, col) 转为全 buffer 字节偏移。
    pub fn to_byte_offset(&self, lines: &[String]) -> usize {
        let mut offset = 0;
        for (i, line) in lines.iter().enumerate() {
            if i == self.row {
                return offset + self.col_byte.min(line.len());
            }
            offset += line.len() + 1; // +1 for '\n'
        }
        offset
    }

    /// 钳位到 lines 的合法范围内。
    pub fn clamped(&self, lines: &[String]) -> Self {
        let row = self.row.min(lines.len().saturating_sub(1));
        let col_byte = self.col_byte.min(lines.get(row).map(|s| s.len()).unwrap_or(0));
        Self { row, col_byte }
    }
}
```

- [ ] **Step 8: 运行测试验证通过**

```bash
cargo test -p peri-tui --lib state_machine::input::cursor
```
Expected: PASS — 5 tests

- [ ] **Step 9: 更新 `mod.rs` 暴露新类型**

Modify `peri-tui/src/state_machine/input/mod.rs:1-10`:

```rust
//! Aggregated input state -- textarea buffer + cursor + selection + at-mention popup +
//! slash-completion popup + attachments + prediction.

pub mod cursor;
pub mod selection;

pub use cursor::CursorPos;
pub use selection::{Selection, SelectionRange};

// ... 保留原有 Attachment / AtMentionState / SlashCompletionState ...
```

- [ ] **Step 10: 运行全 input 模块测试 + Commit**

```bash
cargo test -p peri-tui --lib state_machine::input
cargo clippy -p peri-tui --lib -- -D warnings
```
Expected: 9 tests pass (4 selection + 5 cursor), 0 warnings

```bash
git add peri-tui/src/state_machine/input/
git commit -m "feat(v2): InputState 基础类型 — Selection + CursorPos

- selection.rs: Selection / SelectionRange + 区间规范化
- cursor.rs: CursorPos + 字节偏移双向转换 + clamp
- mod.rs: pub use 暴露新类型
- 9 个测试覆盖核心语义

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>"
```

---

## Task 2: 重构 InputState 为多行 + 集成 Cursor/Selection

**Files:**
- Modify: `peri-tui/src/state_machine/input/mod.rs:15-67`（重写 InputState struct + 方法）
- Test: `peri-tui/src/state_machine/input_test.rs`（新建）

- [ ] **Step 1: 写 `InputState` 多行能力失败测试**

Create `peri-tui/src/state_machine/input_test.rs`:

```rust
use super::input::{CursorPos, InputState, Selection};

#[test]
fn test_input_state_default_is_empty_single_line() {
    let s = InputState::default();
    assert_eq!(s.lines.len(), 1);
    assert!(s.lines[0].is_empty());
    assert_eq!(s.cursor, CursorPos::default());
    assert!(s.selection.is_none());
}

#[test]
fn test_input_state_insert_char_at_cursor() {
    let mut s = InputState::default();
    s.insert_str("hi");
    assert_eq!(s.lines, vec!["hi".to_string()]);
    assert_eq!(s.cursor, CursorPos::new(0, 2));
}

#[test]
fn test_input_state_insert_newline_splits_line() {
    let mut s = InputState::default();
    s.insert_str("ab\ncd");
    assert_eq!(s.lines, vec!["ab".to_string(), "cd".to_string()]);
    assert_eq!(s.cursor, CursorPos::new(1, 2));
}

#[test]
fn test_input_state_insert_at_middle_pushes_rest_right() {
    let mut s = InputState::default();
    s.insert_str("hello");
    s.cursor = CursorPos::new(0, 2);
    s.insert_str("XX");
    assert_eq!(s.lines, vec!["heXXllo".to_string()]);
    assert_eq!(s.cursor, CursorPos::new(0, 4));
}

#[test]
fn test_input_state_backspace_at_line_start_merges_with_prev() {
    let mut s = InputState::default();
    s.insert_str("ab\ncd");
    s.cursor = CursorPos::new(1, 0);
    s.backspace();
    assert_eq!(s.lines, vec!["abcd".to_string()]);
    assert_eq!(s.cursor, CursorPos::new(0, 2));
}

#[test]
fn test_input_state_backspace_in_middle_deletes_char() {
    let mut s = InputState::default();
    s.insert_str("hello");
    s.cursor = CursorPos::new(0, 3);
    s.backspace();
    assert_eq!(s.lines, vec!["helo".to_string()]);
    assert_eq!(s.cursor, CursorPos::new(0, 2));
}

#[test]
fn test_input_state_clear_resets_to_empty_single_line() {
    let mut s = InputState::default();
    s.insert_str("multi\nline\ntext");
    s.clear_buffer();
    assert_eq!(s.lines, vec![String::new()]);
    assert_eq!(s.cursor, CursorPos::default());
    assert!(s.selection.is_none());
}
```

- [ ] **Step 2: 运行测试验证失败**

```bash
cargo test -p peri-tui --lib state_machine::input_test
```
Expected: FAIL — `no field 'lines' on type 'InputState'`

- [ ] **Step 3: 重写 `InputState` struct + 核心方法**

Modify `peri-tui/src/state_machine/input/mod.rs:15-67`:

```rust
/// Aggregated input-box state.
///
/// 持有多行 buffer + 光标位置 + 选择区间 + 历史导航 + prediction +
/// at-mention / slash-completion popup + 附件列表。
#[derive(Debug, Clone, Default)]
pub struct InputState {
    /// 多行文本 buffer（至少 1 行，空 buffer 为 `vec![String::new()]`）。
    pub lines: Vec<String>,

    /// 光标位置 (row, col_byte)。
    pub cursor: CursorPos,

    /// 当前选择区间（如有）。鼠标拖拽 / Shift+方向键 / Ctrl+A 触发。
    pub selection: Option<Selection>,

    /// 历史提交记录（最新在尾部）。
    pub history: Vec<String>,

    /// 历史导航当前位置（None = 编辑 live buffer）。
    pub history_index: Option<usize>,

    /// 灰色 prediction 文本（来自 `"prediction"` 事件）。
    pub prediction: Option<String>,

    /// 活跃 `@mention` popup（如有）。
    pub at_mention: Option<AtMentionState>,

    /// 活跃 `/slash` completion popup（如有）。
    pub slash_completion: Option<SlashCompletionState>,

    /// 待提交附件（图片/文件）。
    pub attachments: Vec<Attachment>,
}

impl InputState {
    pub fn new() -> Self {
        Self {
            lines: vec![String::new()],
            ..Default::default()
        }
    }

    /// 全 buffer 文本（行用 '\n' 连接）。
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// 在光标位置插入字符串（支持 '\n' 多行）。
    pub fn insert_str(&mut self, s: &str) {
        let parts: Vec<&str> = s.split('\n').collect();
        let CursorPos { row, col_byte } = self.cursor;

        let right_part: String = self.lines[row].drain(col_byte..).collect();
        self.lines[row].push_str(parts[0]);

        for (i, part) in parts.iter().enumerate().skip(1) {
            let mut new_line = String::new();
            new_line.push_str(part);
            if i == parts.len() - 1 {
                new_line.push_str(&right_part);
            }
            self.lines.insert(row + i, new_line);
        }

        // 更新光标
        let new_row = row + parts.len() - 1;
        let new_col = if parts.len() == 1 {
            col_byte + parts[0].len()
        } else {
            parts.last().unwrap().len()
        };
        self.cursor = CursorPos::new(new_row, new_col);
    }

    /// 退格。在行首时合并到上一行。
    pub fn backspace(&mut self) {
        let CursorPos { row, col_byte } = self.cursor;
        if col_byte == 0 {
            if row > 0 {
                let prev_len = self.lines[row - 1].len();
                let current = self.lines.remove(row);
                self.lines[row - 1].push_str(&current);
                self.cursor = CursorPos::new(row - 1, prev_len);
            }
            return;
        }
        // CJK 安全删除：找到前一个 char 边界
        let line = &mut self.lines[row];
        let chars_before: Vec<(usize, char)> = line[..col_byte].char_indices().collect();
        if let Some(&(prev_byte, _)) = chars_before.last() {
            line.replace_range(prev_byte..col_byte, "");
            self.cursor = CursorPos::new(row, prev_byte);
        }
    }

    /// 清空 buffer 到空单行状态。
    pub fn clear_buffer(&mut self) {
        self.lines = vec![String::new()];
        self.cursor = CursorPos::default();
        self.selection = None;
        self.prediction = None;
        self.at_mention = None;
        self.slash_completion = None;
    }
}
```

- [ ] **Step 4: 运行测试验证通过**

```bash
cargo test -p peri-tui --lib state_machine::input_test
```
Expected: PASS — 7 tests

- [ ] **Step 5: 修复 InputState::default() 编译错误**

如果 `InputState::default()` 现在依赖 `lines: Vec<String>` 但 derive(Default) 给空 vec，需要修正：

Modify `peri-tui/src/state_machine/input/mod.rs` 顶部：

```rust
impl Default for InputState {
    fn default() -> Self {
        Self {
            lines: vec![String::new()],
            cursor: CursorPos::default(),
            selection: None,
            history: Vec::new(),
            history_index: None,
            prediction: None,
            at_mention: None,
            slash_completion: None,
            attachments: Vec::new(),
        }
    }
}
```

删除 `#[derive(Debug, Clone, Default)]` 中的 `Default`。

- [ ] **Step 6: 运行全 workspace 编译验证**

```bash
cargo build -p peri-tui 2>&1 | tail -10
```
Expected: 可能有多处编译错误（旧字段引用）— 在 Step 7-9 修复。

- [ ] **Step 7: 修复旧字段引用 `buffer` / `cursor: usize`**

```bash
grep -rn "\.buffer\b\|input\.cursor\b" peri-tui/src/state_machine/
```

对每个引用点：
- `.buffer` → `.text()` 或 `.lines[row]`
- `cursor: usize` → `cursor: CursorPos`

主要修复点：
- `state_machine/transitions/idle.rs` — 如果引用 `input.buffer` / `input.cursor`
- `state_machine/transitions/streaming.rs` — buffered input 处理

- [ ] **Step 8: 运行全 workspace 测试**

```bash
cargo test -p peri-tui --lib 2>&1 | tail -5
```
Expected: 1010+ tests pass（原有 + 新增 16）

- [ ] **Step 9: Commit**

```bash
git add peri-tui/src/state_machine/input/ peri-tui/src/state_machine/input_test.rs
git commit -m "feat(v2): InputState 多行 buffer + insert/backspace API

- lines: Vec<String> 替代 buffer: String
- cursor: CursorPos (row, col_byte) 替代 usize
- insert_str 支持多行分割
- backspace 在行首时合并到上一行
- clear_buffer 重置到空单行
- 16 个测试覆盖核心编辑语义

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>"
```

---

## Task 3: 选择 + 单词/行删除 + 光标跳跃

**Files:**
- Create: `peri-tui/src/state_machine/input/edit.rs`
- Create: `peri-tui/src/state_machine/input/clipboard.rs`
- Modify: `peri-tui/src/state_machine/input/mod.rs`（添加 `pub use`）

- [ ] **Step 1: 写 edit.rs 失败测试**

Create `peri-tui/src/state_machine/input/edit_test.rs`:

```rust
use super::edit::InputEdit;
use super::{CursorPos, InputState};

#[test]
fn test_start_selection_sets_anchor() {
    let mut s = InputState::default();
    s.insert_str("hello");
    s.cursor = CursorPos::new(0, 2);
    s.start_selection();
    assert!(s.selection.is_some());
    let sel = s.selection.as_ref().unwrap();
    assert_eq!(sel.anchor_row, 0);
    assert_eq!(sel.anchor_col, 2);
}

#[test]
fn test_move_cursor_left_clears_selection_when_no_shift() {
    let mut s = InputState::default();
    s.insert_str("hello");
    s.cursor = CursorPos::new(0, 3);
    s.start_selection();
    s.move_cursor_left(false); // shift=false
    assert!(s.selection.is_none());
    assert_eq!(s.cursor, CursorPos::new(0, 2));
}

#[test]
fn test_move_cursor_left_extends_selection_when_shift() {
    let mut s = InputState::default();
    s.insert_str("hello");
    s.cursor = CursorPos::new(0, 3);
    s.start_selection();
    s.move_cursor_left(true); // shift=true
    assert!(s.selection.is_some());
    assert_eq!(s.cursor, CursorPos::new(0, 2));
}

#[test]
fn test_select_all_selects_entire_buffer() {
    let mut s = InputState::default();
    s.insert_str("ab\ncd");
    s.select_all();
    let sel = s.selection.as_ref().unwrap();
    assert_eq!(sel.start(), (0, 0));
    assert_eq!(sel.end(), (1, 2));
}

#[test]
fn test_delete_word_backspace_removes_last_word() {
    let mut s = InputState::default();
    s.insert_str("hello world");
    s.cursor = CursorPos::new(0, 11);
    s.delete_word_backspace();
    assert_eq!(s.lines, vec!["hello ".to_string()]);
    assert_eq!(s.cursor, CursorPos::new(0, 6));
}

#[test]
fn test_delete_line_to_head() {
    let mut s = InputState::default();
    s.insert_str("hello");
    s.cursor = CursorPos::new(0, 3);
    s.delete_line_by_head();
    assert_eq!(s.lines, vec!["lo".to_string()]);
    assert_eq!(s.cursor, CursorPos::new(0, 0));
}

#[test]
fn test_move_cursor_jump_to_position() {
    let mut s = InputState::default();
    s.insert_str("ab\ncd");
    s.move_cursor_jump(1, 1);
    assert_eq!(s.cursor, CursorPos::new(1, 1));
}

#[test]
fn test_move_cursor_word_forward() {
    let mut s = InputState::default();
    s.insert_str("hello world foo");
    s.cursor = CursorPos::new(0, 0);
    s.move_cursor_word_forward();
    assert_eq!(s.cursor.col_byte, 6); // 跳到 "world" 之后
}
```

- [ ] **Step 2: 运行测试验证失败**

```bash
cargo test -p peri-tui --lib state_machine::input::edit
```
Expected: FAIL

- [ ] **Step 3: 实现 `edit.rs`**

Create `peri-tui/src/state_machine/input/edit.rs`:

```rust
//! 高级编辑操作 trait，扩展 InputState。

use super::cursor::CursorPos;
use super::InputState;

pub trait InputEdit {
    fn start_selection(&mut self);
    fn select_all(&mut self);
    fn move_cursor_left(&mut self, extend_selection: bool);
    fn move_cursor_right(&mut self, extend_selection: bool);
    fn move_cursor_up(&mut self, extend_selection: bool);
    fn move_cursor_down(&mut self, extend_selection: bool);
    fn move_cursor_jump(&mut self, row: usize, col: usize);
    fn move_cursor_word_forward(&mut self);
    fn move_cursor_word_back(&mut self);
    fn delete_word_backspace(&mut self);
    fn delete_line_by_head(&mut self);
}

impl InputEdit for InputState {
    fn start_selection(&mut self) {
        self.selection = Some(super::Selection::normal(
            self.cursor.row,
            self.cursor.col_byte,
            self.cursor.row,
            self.cursor.col_byte,
        ));
    }

    fn select_all(&mut self) {
        let last_row = self.lines.len() - 1;
        let last_col = self.lines[last_row].len();
        self.cursor = CursorPos::new(last_row, last_col);
        self.selection = Some(super::Selection::normal(0, 0, last_row, last_col));
    }

    fn move_cursor_left(&mut self, extend_selection: bool) {
        let old = self.cursor;
        let new = if old.col_byte > 0 {
            // CJK 安全：找前一个 char 边界
            let line = &self.lines[old.row];
            let prev_byte = line[..old.col_byte]
                .char_indices()
                .last()
                .map(|(b, _)| b)
                .unwrap_or(0);
            CursorPos::new(old.row, prev_byte)
        } else if old.row > 0 {
            CursorPos::new(old.row - 1, self.lines[old.row - 1].len())
        } else {
            old
        };

        self.update_selection_and_cursor(old, new, extend_selection);
    }

    fn move_cursor_right(&mut self, extend_selection: bool) {
        let old = self.cursor;
        let line_len = self.lines[old.row].len();
        let new = if old.col_byte < line_len {
            let line = &self.lines[old.row];
            let next_byte = line[old.col_byte..]
                .char_indices()
                .nth(1)
                .map(|(b, _)| old.col_byte + b)
                .unwrap_or(line_len);
            CursorPos::new(old.row, next_byte)
        } else if old.row < self.lines.len() - 1 {
            CursorPos::new(old.row + 1, 0)
        } else {
            old
        };

        self.update_selection_and_cursor(old, new, extend_selection);
    }

    fn move_cursor_up(&mut self, extend_selection: bool) {
        let old = self.cursor;
        let new = if old.row > 0 {
            CursorPos::new(old.row - 1, old.col_byte.min(self.lines[old.row - 1].len()))
        } else {
            old
        };
        self.update_selection_and_cursor(old, new, extend_selection);
    }

    fn move_cursor_down(&mut self, extend_selection: bool) {
        let old = self.cursor;
        let new = if old.row < self.lines.len() - 1 {
            CursorPos::new(old.row + 1, old.col_byte.min(self.lines[old.row + 1].len()))
        } else {
            old
        };
        self.update_selection_and_cursor(old, new, extend_selection);
    }

    fn move_cursor_jump(&mut self, row: usize, col: usize) {
        let clamped = CursorPos::new(row, col).clamped(&self.lines);
        self.cursor = clamped;
        self.selection = None;
    }

    fn move_cursor_word_forward(&mut self) {
        let line = &self.lines[self.cursor.row];
        let mut chars = line[self.cursor.col_byte..].char_indices().peekable();
        let mut passed_non_space = false;
        while let Some((rel_off, c)) = chars.next() {
            if c.is_whitespace() {
                if passed_non_space {
                    self.cursor = CursorPos::new(self.cursor.row, self.cursor.col_byte + rel_off);
                    return;
                }
            } else {
                passed_non_space = true;
            }
        }
        // 到行尾
        self.cursor = CursorPos::new(self.cursor.row, line.len());
    }

    fn move_cursor_word_back(&mut self) {
        let line = &self.lines[self.cursor.row];
        let before: Vec<(usize, char)> = line[..self.cursor.col_byte].char_indices().collect();
        let mut iter = before.iter().rev().peekable();
        let mut passed_non_space = false;
        while let Some(&(byte_off, c)) = iter.peek() {
            if c.is_whitespace() {
                if passed_non_space {
                    self.cursor = CursorPos::new(self.cursor.row, byte_off + c.len_utf8());
                    return;
                }
            } else {
                passed_non_space = true;
            }
            iter.next();
        }
        self.cursor = CursorPos::new(self.cursor.row, 0);
    }

    fn delete_word_backspace(&mut self) {
        let start_cursor = self.cursor;
        self.move_cursor_word_back();
        let end_cursor = self.cursor;
        if start_cursor != end_cursor {
            self.delete_range(end_cursor, start_cursor);
            self.cursor = end_cursor;
        }
    }

    fn delete_line_by_head(&mut self) {
        let CursorPos { row, col_byte } = self.cursor;
        let removed: String = self.lines[row].drain(..col_byte).collect();
        let _ = removed; // 不放进剪贴板，delete_line_by_head 仅删除
        self.cursor = CursorPos::new(row, 0);
    }
}

impl InputState {
    /// 内部辅助：更新 selection + cursor。
    fn update_selection_and_cursor(&mut self, old: CursorPos, new: CursorPos, extend: bool) {
        if extend {
            if self.selection.is_none() {
                self.selection = Some(super::Selection::normal(
                    old.row, old.col_byte, new.row, new.col_byte,
                ));
            } else if let Some(sel) = self.selection.as_mut() {
                sel.cursor_row = new.row;
                sel.cursor_col = new.col_byte;
            }
        } else {
            self.selection = None;
        }
        self.cursor = new;
    }

    /// 删除 [start, end) 范围内的所有字符（跨行）。
    pub fn delete_range(&mut self, start: CursorPos, end: CursorPos) {
        let start = start.clamped(&self.lines);
        let end = end.clamped(&self.lines);
        if start.row == end.row {
            self.lines[start.row].replace_range(start.col_byte..end.col_byte, "");
        } else {
            // 合并 start 行前半 + end 行后半
            let tail: String = self.lines[end.row][end.col_byte..].to_string();
            self.lines[start.row].truncate(start.col_byte);
            self.lines[start.row].push_str(&tail);
            // 删除中间整行
            self.lines.drain((start.row + 1)..=end.row);
        }
    }
}
```

- [ ] **Step 4: 运行测试验证通过**

```bash
cargo test -p peri-tui --lib state_machine::input::edit
```
Expected: PASS — 9 tests

- [ ] **Step 5: 写 clipboard.rs 失败测试**

Create `peri-tui/src/state_machine/input/clipboard_test.rs`:

```rust
use super::clipboard::InputClipboard;
use super::{CursorPos, InputState};

#[test]
fn test_copy_selection_returns_selected_text() {
    let mut s = InputState::default();
    s.insert_str("hello world");
    s.selection = Some(super::Selection::normal(0, 0, 0, 5));
    let copied = s.copy_selection();
    assert_eq!(copied, Some("hello".to_string()));
}

#[test]
fn test_copy_no_selection_returns_none() {
    let mut s = InputState::default();
    s.insert_str("hello");
    assert_eq!(s.copy_selection(), None);
}

#[test]
fn test_cut_removes_selection_and_returns_text() {
    let mut s = InputState::default();
    s.insert_str("hello world");
    s.selection = Some(super::Selection::normal(0, 0, 0, 5));
    let cut = s.cut_selection();
    assert_eq!(cut, Some("hello".to_string()));
    assert_eq!(s.lines, vec![" world".to_string()]);
    assert_eq!(s.cursor, CursorPos::new(0, 0));
    assert!(s.selection.is_none());
}

#[test]
fn test_paste_inserts_at_cursor() {
    let mut s = InputState::default();
    s.insert_str("ab");
    s.paste("XX");
    assert_eq!(s.lines, vec!["abXX".to_string()]);
    assert_eq!(s.cursor, CursorPos::new(0, 4));
}
```

- [ ] **Step 6: 运行测试验证失败**

```bash
cargo test -p peri-tui --lib state_machine::input::clipboard
```
Expected: FAIL

- [ ] **Step 7: 实现 `clipboard.rs`**

Create `peri-tui/src/state_machine/input/clipboard.rs`:

```rust
//! 剪贴板操作 trait。

use super::cursor::CursorPos;
use super::InputState;

pub trait InputClipboard {
    fn copy_selection(&self) -> Option<String>;
    fn cut_selection(&mut self) -> Option<String>;
    fn paste(&mut self, text: &str);
}

impl InputClipboard for InputState {
    fn copy_selection(&self) -> Option<String> {
        let sel = self.selection.as_ref()?;
        if sel.is_empty() {
            return None;
        }
        let (start_row, start_col) = sel.start();
        let (end_row, end_col) = sel.end();

        if start_row == end_row {
            Some(self.lines[start_row][start_col..end_col].to_string())
        } else {
            let mut result = self.lines[start_row][start_col..].to_string();
            result.push('\n');
            for row in (start_row + 1)..end_row {
                result.push_str(&self.lines[row]);
                result.push('\n');
            }
            result.push_str(&self.lines[end_row][..end_col]);
            Some(result)
        }
    }

    fn cut_selection(&mut self) -> Option<String> {
        let text = self.copy_selection()?;
        let sel = self.selection.take()?;
        let (start_row, start_col) = sel.start();
        let (end_row, end_col) = sel.end();
        self.delete_range(
            CursorPos::new(start_row, start_col),
            CursorPos::new(end_row, end_col),
        );
        self.cursor = CursorPos::new(start_row, start_col).clamped(&self.lines);
        Some(text)
    }

    fn paste(&mut self, text: &str) {
        // 先删除当前 selection（如有）
        if self.selection.is_some() {
            self.cut_selection();
        }
        self.insert_str(text);
    }
}
```

- [ ] **Step 8: 运行测试验证通过**

```bash
cargo test -p peri-tui --lib state_machine::input::clipboard
```
Expected: PASS — 4 tests

- [ ] **Step 9: 更新 mod.rs 暴露新模块 + Commit**

Modify `peri-tui/src/state_machine/input/mod.rs`:

```rust
pub mod clipboard;
pub mod cursor;
pub mod edit;
pub mod selection;

pub use clipboard::InputClipboard;
pub use cursor::CursorPos;
pub use edit::InputEdit;
pub use selection::{Selection, SelectionRange};
```

```bash
cargo test -p peri-tui --lib state_machine::input
cargo clippy -p peri-tui --lib -- -D warnings
git add peri-tui/src/state_machine/input/
git commit -m "feat(v2): InputState 选择 + 单词/行删除 + 剪贴板

- edit.rs: InputEdit trait (start_selection, select_all, move_cursor_*, delete_word, delete_line_by_head)
- clipboard.rs: InputClipboard trait (copy, cut, paste)
- delete_range 跨行删除
- 13 个测试覆盖选择/删除/剪贴板语义

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>"
```

---

## Task 4: tui_textarea::TextArea ↔ InputState 同步

**Files:**
- Create: `peri-tui/src/state_machine/input/sync.rs`
- Test: `peri-tui/src/state_machine/input/sync_test.rs`

`tui_textarea::TextArea` 保留为渲染 widget，状态从 InputState 同步注入。

- [ ] **Step 1: 写 sync 失败测试**

Create `peri-tui/src/state_machine/input/sync_test.rs`:

```rust
use super::sync::{from_textarea, to_textarea};
use super::{CursorPos, InputState};
use tui_textarea::TextArea;

#[test]
fn test_from_textarea_copies_lines_and_cursor() {
    let mut ta = TextArea::default();
    ta.insert_str("hello");
    ta.move_cursor(tui_textarea::CursorMove::Forward);
    let state = from_textarea(&ta);
    assert_eq!(state.lines, vec!["hello".to_string()]);
    assert_eq!(state.cursor, CursorPos::new(0, 1));
}

#[test]
fn test_to_textarea_restores_state() {
    let mut state = InputState::default();
    state.insert_str("ab\ncd");
    state.cursor = CursorPos::new(1, 1);
    let mut ta = TextArea::default();
    to_textarea(&state, &mut ta);
    assert_eq!(ta.lines(), &["ab".to_string(), "cd".to_string()]);
    let (row, col) = ta.cursor();
    assert_eq!((row, col), (1, 1));
}

#[test]
fn test_roundtrip_preserves_state() {
    let mut original = InputState::default();
    original.insert_str("multi\nline\ntext");
    original.cursor = CursorPos::new(2, 2);

    let mut ta = TextArea::default();
    to_textarea(&original, &mut ta);
    let restored = from_textarea(&ta);

    assert_eq!(restored.lines, original.lines);
    assert_eq!(restored.cursor, original.cursor);
}
```

- [ ] **Step 2: 运行测试验证失败**

```bash
cargo test -p peri-tui --lib state_machine::input::sync
```
Expected: FAIL

- [ ] **Step 3: 实现 `sync.rs`**

Create `peri-tui/src/state_machine/input/sync.rs`:

```rust
//! tui_textarea::TextArea ↔ InputState 双向同步。
//!
//! TextArea 仅作为渲染 widget；状态源始终是 InputState。

use super::cursor::CursorPos;
use super::InputState;
use tui_textarea::TextArea;

/// 从 TextArea 读取当前状态到 InputState。
pub fn from_textarea(ta: &TextArea) -> InputState {
    let lines: Vec<String> = ta.lines().to_vec();
    let (row, col) = ta.cursor();
    let mut state = InputState::new();
    if lines.is_empty() {
        state.lines = vec![String::new()];
    } else {
        state.lines = lines;
    }
    state.cursor = CursorPos::new(row, col);
    state
}

/// 把 InputState 状态写回 TextArea（渲染前调用）。
pub fn to_textarea(state: &InputState, ta: &mut TextArea) {
    ta.set_text(state.lines.join("\n"));
    let CursorPos { row, col_byte } = state.cursor.clamped(&state.lines);
    ta.move_cursor(tui_textarea::CursorMove::Jump(row as u16, col_byte as u16));
}
```

- [ ] **Step 4: 运行测试验证通过**

```bash
cargo test -p peri-tui --lib state_machine::input::sync
```
Expected: PASS — 3 tests

- [ ] **Step 5: 更新 mod.rs 暴露 sync + Commit**

Modify `peri-tui/src/state_machine/input/mod.rs`:

```rust
pub mod sync;
pub use sync::{from_textarea, to_textarea};
```

```bash
cargo test -p peri-tui --lib state_machine::input
git add peri-tui/src/state_machine/input/
git commit -m "feat(v2): InputState ↔ TextArea 同步桥

- sync.rs: from_textarea / to_textarea 双向同步
- TextArea 保留为渲染 widget，状态源始终是 InputState
- roundtrip 测试保证状态不丢失

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>"
```

---

## Task 5: 移除 `cursor: usize` 字段残余引用

之前 InputState 用 `cursor: usize`（字节偏移），现在改为 `CursorPos`。需要扫描并修复所有引用。

**Files:**
- Modify: 所有引用 `input.cursor` 的文件

- [ ] **Step 1: 扫描所有引用点**

```bash
grep -rn "input\.cursor\|\.cursor\b" peri-tui/src/state_machine/ | grep -v "_test\.rs" | grep -v "cursor\.rs"
```

预期找到的引用点（调研结果）：
- `state_machine/transitions/idle.rs` — 输入字符处理
- `state_machine/transitions/streaming.rs` — buffered input

- [ ] **Step 2: 修复 transitions/idle.rs**

读取 `peri-tui/src/state_machine/transitions/idle.rs`，找到所有 `input.cursor` 引用：

- 旧：`input.cursor`（usize）→ 新：`input.cursor.col_byte`（如果在单行上下文）
- 旧：`input.cursor = N` → 新：`input.cursor = CursorPos::new(0, N)` 或 `CursorPos { row: 0, col_byte: N }`

对每个修改点：
- [ ] 修改后立即运行 `cargo test -p peri-tui --lib state_machine::transitions::idle`
- [ ] 验证所有 idle 测试通过

- [ ] **Step 3: 修复 transitions/streaming.rs**

同样的模式修复 streaming.rs 中的 `input.cursor` 引用。

- [ ] **Step 4: 全 workspace 编译 + 测试**

```bash
cargo build --workspace 2>&1 | tail -5
cargo test --workspace 2>&1 | grep -E "test result" | awk '{print $4, $6, $8}' | awk '{p+=$1; f+=$2; i+=$3} END {print "passed=" p " failed=" f " ignored=" i}'
```
Expected: 编译 0 错误，测试通过数 ≥ 3304（之前基线）+ 新增 input 测试

- [ ] **Step 5: Commit**

```bash
git add peri-tui/src/state_machine/
git commit -m "refactor(v2): InputState.cursor 改为 CursorPos 类型

- 所有 input.cursor 引用迁移到 CursorPos
- idle.rs / streaming.rs 适配
- 全 workspace 编译通过

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>"
```

---

## Task 6: 历史记录 + prediction 集成

之前 InputState 用 `history: Vec<String>` + `history_index` + `prediction`。验证它们与新多行 buffer 兼容。

**Files:**
- Modify: `peri-tui/src/state_machine/input/mod.rs`（添加 history 方法）
- Test: `peri-tui/src/state_machine/input_test.rs`

- [ ] **Step 1: 写 history 方法失败测试**

Append to `peri-tui/src/state_machine/input_test.rs`:

```rust
#[test]
fn test_history_push_stores_text() {
    let mut s = InputState::default();
    s.insert_str("first");
    s.history_push();
    assert_eq!(s.history, vec!["first".to_string()]);
    assert!(s.lines[0].is_empty());
}

#[test]
fn test_history_navigate_prev_loads_previous() {
    let mut s = InputState::default();
    s.history = vec!["old1".into(), "old2".into()];
    s.history_prev();
    assert_eq!(s.history_index, Some(1));
    assert_eq!(s.text(), "old2");
}

#[test]
fn test_history_navigate_next_returns_to_live() {
    let mut s = InputState::default();
    s.history = vec!["old".into()];
    s.history_prev();
    s.history_next();
    assert_eq!(s.history_index, None);
    assert!(s.lines[0].is_empty());
}

#[test]
fn test_prediction_set_shown_until_overwritten() {
    let mut s = InputState::default();
    s.set_prediction("world");
    assert_eq!(s.prediction, Some("world".to_string()));
    s.insert_str("x");
    assert_eq!(s.prediction, None);
}
```

- [ ] **Step 2: 运行验证失败**

```bash
cargo test -p peri-tui --lib state_machine::input_test
```
Expected: FAIL — `method 'history_push' not found`

- [ ] **Step 3: 实现 history + prediction 方法**

Append to `impl InputState` in `peri-tui/src/state_machine/input/mod.rs`:

```rust
    /// 把当前 buffer 文本推入历史，并清空 buffer。
    pub fn history_push(&mut self) {
        let text = self.text();
        if !text.is_empty() {
            self.history.push(text);
        }
        self.clear_buffer();
    }

    /// 导航到上一条历史。更新 buffer 与 cursor。
    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let new_idx = match self.history_index {
            None => self.history.len() - 1,
            Some(0) => return,
            Some(i) => i - 1,
        };
        self.history_index = Some(new_idx);
        self.load_from_history(new_idx);
    }

    /// 导航到下一条历史。None 表示回到 live buffer。
    pub fn history_next(&mut self) {
        let current = match self.history_index {
            None => return,
            Some(i) => i,
        };
        if current + 1 >= self.history.len() {
            self.history_index = None;
            self.clear_buffer();
        } else {
            self.history_index = Some(current + 1);
            self.load_from_history(current + 1);
        }
    }

    fn load_from_history(&mut self, idx: usize) {
        let text = self.history[idx].clone();
        self.lines = text.split('\n').map(String::from).collect();
        let last_row = self.lines.len() - 1;
        self.cursor = CursorPos::new(last_row, self.lines[last_row].len());
    }

    /// 设置 prediction 文本。
    pub fn set_prediction(&mut self, text: &str) {
        self.prediction = if text.is_empty() {
            None
        } else {
            Some(text.to_string())
        };
    }
```

- [ ] **Step 4: 运行测试验证通过**

```bash
cargo test -p peri-tui --lib state_machine::input_test
```
Expected: PASS — 4 new tests + previous 7 = 11 total

- [ ] **Step 5: 全 workspace 测试 + Commit**

```bash
cargo test -p peri-tui --lib 2>&1 | tail -3
git add peri-tui/src/state_machine/input/
git commit -m "feat(v2): InputState history 导航 + prediction

- history_push / history_prev / history_next 完整导航
- load_from_history 恢复多行 + 光标到末尾
- set_prediction 设置/清除 prediction
- 4 个新测试

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>"
```

---

## Task 7: 完整测试套件 + clippy 清理

**Files:**
- Verify: `peri-tui/src/state_machine/input_test.rs` 完整覆盖

- [ ] **Step 1: 运行完整 input 测试套件**

```bash
cargo test -p peri-tui --lib state_machine::input 2>&1 | tail -10
```
Expected: 30+ tests pass

- [ ] **Step 2: clippy 0 警告**

```bash
cargo clippy -p peri-tui --lib -- -D warnings 2>&1 | tail -10
```
Expected: 0 warnings

- [ ] **Step 3: 全 workspace 验证**

```bash
cargo build --workspace 2>&1 | tail -3
cargo test --workspace 2>&1 | grep -E "test result" | awk '{print $4, $6, $8}' | awk '{p+=$1; f+=$2; i+=$3} END {print "passed=" p " failed=" f " ignored=" i}'
```
Expected: 通过数 ≥ 3304 + 新增（约 30）= 3334+，failed=0

- [ ] **Step 4: 如果有失败测试，逐一修复**

常见失败：
- 旧测试依赖 `buffer: String` 字段 → 改用 `text()` 方法
- 旧测试依赖 `cursor: usize` → 改用 `cursor.col_byte`

- [ ] **Step 5: Plan 1 完成标志**

```bash
grep -rn "input\.buffer\b" peri-tui/src/state_machine/ 2>/dev/null
grep -rn "cursor: usize" peri-tui/src/state_machine/input/ 2>/dev/null
```
Expected: 0 results（无旧字段残余）

```bash
git log --oneline -7
```
Expected: 6 个新 commit（Task 1-6）

---

## Plan 1 完成定义（Definition of Done）

1. ✅ `InputState` 持有 `lines: Vec<String>` + `cursor: CursorPos` + `selection: Option<Selection>`
2. ✅ 完整编辑 API：insert_str / backspace / delete_word_backspace / delete_line_by_head
3. ✅ 完整选择 API：start_selection / select_all / move_cursor_* (with shift)
4. ✅ 完整剪贴板 API：copy_selection / cut_selection / paste
5. ✅ 完整光标 API：move_cursor_jump / move_cursor_word_forward/back
6. ✅ History 导航：history_push / history_prev / history_next
7. ✅ Prediction：set_prediction
8. ✅ TextArea 同步桥：from_textarea / to_textarea
9. ✅ 30+ 测试覆盖
10. ✅ 0 clippy 警告
11. ✅ 全 workspace 编译 + 测试通过

**Plan 2 依赖项已就绪**：`InputState` 可作为 `State::Idle.input` 的完整实现。
