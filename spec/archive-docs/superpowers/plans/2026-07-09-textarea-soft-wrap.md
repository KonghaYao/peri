# Textarea Soft Wrapping 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 input area textarea 组件实现浏览器式的软换行——按终端宽度自动折行、视口跟随基于视觉行、上下移动保持视觉列。

**Architecture:** 在渲染层（`render_multiline_with_cursor`）增加 `max_width` 参数做 display-width 感知的字符级折行，不存储到状态层。`TextAreaState` 新增 `desired_col` 字段记忆视觉列用于 Up/Down 移动。

**Tech Stack:** Rust 2021, ratatui, unicode-width, ratatui-kit

**关联 Issue:** `spec/issues/2026-07-09-textarea-no-soft-wrap.md`

**审查修正（2026-07-09 三轮审查后）：**
- wrap_text 移除 unused `desired_col` 参数（死参数）
- VisualLine 移除语义重复的 `char_start` 字段
- 空视觉行光标可见性修复
- undo/redo 增加 desired_col 清除
- visual_row_col_to_cursor 增加 debug_assert bounds check
- 鼠标点击处理适配视觉行
- Task 2+3 合并（避免 bisect 断点）

---

## 文件结构

```
peri-widgets/src/textarea/
├── render.rs        ← 新增 wrap_text() + VisualLine/WrapResult；
│                       render_multiline_with_cursor 增加 max_width 参数
├── state.rs          ← TextAreaState 新增 desired_col；
│                       cursor_visual_up/down 新增方法
├── widget.rs         ← render_ref 中 area.width 作为 max_width 传入
├── mod.rs            ← 导出新增公开类型
├── state_test.rs     ← 更新所有 render_multiline_with_cursor 调用 + 新增视觉移动测试
└── render_test.rs    ← 新增 wrap_text 测试 + 更新所有 render_multiline_with_cursor 调用
peri-tui/src/kit/
└── input_area.rs     ← 计算 text_width 传入渲染；Up/Down 事件 + 鼠标点击适配视觉行
```

---

### Task 1: 新增 `wrap_text()` 纯函数及数据结构

**Files:**
- Modify: `peri-widgets/src/textarea/render.rs`

- [ ] **Step 1: 在 render.rs 顶部新增数据结构**

在 `display_width_before` 函数之后、`render_multiline_with_cursor` 之前，插入以下代码：

```rust
/// 一个视觉行——逻辑行折行后的一行。选中映射和光标定位依赖 char_range。
#[derive(Debug, Clone)]
pub struct VisualLine {
    /// 来源逻辑行索引（text.split('\n') 中的行号）
    pub source_line: usize,
    /// 本视觉行的文本内容
    pub text: String,
    /// 本视觉行内字符范围（[start, end) 半开区间，全局坐标）
    /// 同时充当第一个字符的全局索引（等价于旧 char_start 字段）
    pub char_range: (usize, usize),
}

/// wrap_text 的返回值
#[derive(Debug, Clone)]
pub struct WrapResult {
    /// 折行后的视觉行列表
    pub visual_lines: Vec<VisualLine>,
    /// 光标所在的视觉行索引
    pub cursor_visual_row: usize,
    /// 光标在视觉行内的视觉列偏移（display width）
    pub cursor_visual_col: usize,
    /// 总视觉行数（等于 visual_lines.len()）
    pub total_visual_rows: usize,
}

/// 将文本按 max_width 做 display-width 感知的折行，返回视觉行列表和光标映射。
///
/// 折行策略：任意字符处断行（overflow-wrap: break-word），保证零宽字符
/// 不单独成行，CJK 双宽字符不打散。max_width 最小 1。
///
/// 光标位置 cursor 为字符索引。返回的 cursor_visual_row/col 是视觉坐标。
pub fn wrap_text(
    text: &str,
    cursor: usize,
    max_width: usize,
) -> WrapResult {
    let max_width = max_width.max(1);
    let cursor = cursor.min(text.chars().count());

    let mut visual_lines: Vec<VisualLine> = Vec::new();
    let mut cursor_visual_row = 0usize;
    let mut cursor_visual_col = 0usize;
    let mut global_char = 0usize;

    for (source_line, logical_line) in text.split('\n').enumerate() {
        let logical_start = global_char;
        let mut line_char_offset = 0usize; // 本逻辑行内字符偏移

        // 空逻辑行仍然产生一个空视觉行
        if logical_line.is_empty() {
            // 只在 global_char 精确匹配时设置光标（不在 +1 处——避免覆盖依赖）
            if cursor == global_char {
                cursor_visual_row = visual_lines.len();
                cursor_visual_col = 0;
            }
            visual_lines.push(VisualLine {
                source_line,
                text: String::new(),
                char_range: (global_char, global_char),
            });
            global_char += 1; // for \n
            continue;
        }

        let mut current_text = String::new();
        let mut current_width = 0usize;
        let mut segment_char_start = line_char_offset;

        for ch in logical_line.chars() {
            let ch_w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);

            // 当前行已有内容，加下一个字会超出 → 断行
            if current_width > 0 && current_width + ch_w > max_width {
                visual_lines.push(VisualLine {
                    source_line,
                    text: std::mem::take(&mut current_text),
                    char_range: (
                        logical_start + segment_char_start,
                        logical_start + line_char_offset,
                    ),
                });
                current_width = 0;
                segment_char_start = line_char_offset;
            }

            current_text.push(ch);
            current_width += ch_w;

            // 光标检查：在视觉行构建过程中匹配全局坐标
            let char_global = logical_start + line_char_offset;
            if cursor == char_global {
                cursor_visual_row = visual_lines.len();
                cursor_visual_col = current_width - ch_w; // 此字符之前
            } else if cursor == char_global + 1 {
                // 光标在这个字符之后
                cursor_visual_row = visual_lines.len();
                cursor_visual_col = current_width;
            }

            line_char_offset += 1;
        }

        // 该行剩余部分
        if !current_text.is_empty() || logical_line.is_empty() {
            // 再次检查光标（可能在行尾）
            let char_global = logical_start + line_char_offset;
            if cursor == char_global {
                cursor_visual_row = visual_lines.len();
                cursor_visual_col = current_width;
            }

            visual_lines.push(VisualLine {
                source_line,
                text: current_text,
                char_range: (logical_start + segment_char_start, logical_start + line_char_offset),
            });
        }

        global_char += logical_line.chars().count() + 1; // +1 for \n
    }

    // 光标可能在文本末尾（超出所有字符）
    if cursor >= text.chars().count() {
        cursor_visual_row = visual_lines.len().saturating_sub(1);
        cursor_visual_col = visual_lines
            .last()
            .map(|vl| {
                vl.text
                    .chars()
                    .map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(0))
                    .sum()
            })
            .unwrap_or(0);
    }

    let total_visual_rows = visual_lines.len();

    WrapResult {
        visual_lines,
        cursor_visual_row,
        cursor_visual_col,
        total_visual_rows,
    }
}
```

- [ ] **Step 2: 编译检查**

```bash
cargo build -p peri-widgets 2>&1 | head -20
```

预期：编译通过（`wrap_text` 尚未被引用，只新增不破坏现有代码）

- [ ] **Step 3: 提交**

```bash
git add peri-widgets/src/textarea/render.rs
git commit -m "feat(textarea): add wrap_text() and WrapResult/VisualLine for soft wrapping"
```

---

### Task 2: 重写 `render_multiline_with_cursor` + 更新所有调用方（合并提交）

> **CR 修正**：与原 Plan 的 Task 2+3 合并——原设计拆成两次提交会导致 Task 2 commit 无法编译，阻碍 git bisect。

**Files:**
- Modify: `peri-widgets/src/textarea/render.rs`
- Modify: `peri-widgets/src/textarea/widget.rs`
- Modify: `peri-widgets/src/textarea/mod.rs`
- Modify: `peri-widgets/src/textarea/render_test.rs`
- Modify: `peri-widgets/src/textarea/state_test.rs`

- [ ] **Step 1: 修改 `render_multiline_with_cursor` 签名，增加 `max_width` 参数**

将函数签名从：

```rust
pub fn render_multiline_with_cursor(
    text: &str,
    cursor: usize,
    cursor_style: Style,
    selection_range: Option<(usize, usize)>,
    selection_style: Style,
    placeholder: Option<&str>,
    placeholder_style: Style,
    viewport_height: usize,
    loading: bool,
    show_cursor: bool,
) -> Vec<Line<'static>> {
```

改为：

```rust
pub fn render_multiline_with_cursor(
    text: &str,
    cursor: usize,
    cursor_style: Style,
    selection_range: Option<(usize, usize)>,
    selection_style: Style,
    placeholder: Option<&str>,
    placeholder_style: Style,
    max_width: usize,          // 新增
    viewport_height: usize,
    loading: bool,
    show_cursor: bool,
) -> Vec<Line<'static>> {
```

- [ ] **Step 2: 重写函数体**

将整个函数体替换为以下代码：

```rust
    let viewport_height = viewport_height.max(1);

    if text.is_empty() {
        return if loading {
            vec![Line::from("")]
        } else if !show_cursor {
            if let Some(ph) = placeholder.filter(|s| !s.is_empty()) {
                vec![Line::from(vec![Span::styled(
                    ph.to_string(),
                    placeholder_style,
                )])]
            } else {
                vec![Line::from("")]
            }
        } else if let Some(ph) = placeholder.filter(|s| !s.is_empty()) {
            vec![Line::from(vec![
                Span::styled(" ", cursor_style),
                Span::styled(ph.to_string(), placeholder_style),
            ])]
        } else {
            vec![Line::from(vec![Span::styled(" ", cursor_style)])]
        };
    }

    // 使用 wrap_text 做软换行
    let wrap = wrap_text(text, cursor, max_width);

    // 视口裁剪基于视觉行
    let (start, end) = if wrap.total_visual_rows <= viewport_height {
        (0, wrap.total_visual_rows)
    } else {
        let half_window = viewport_height / 2;
        let center_start = wrap.cursor_visual_row.saturating_sub(half_window);
        let end = (center_start + viewport_height).min(wrap.total_visual_rows);
        let start = end.saturating_sub(viewport_height);
        (start, end)
    };

    let mut result: Vec<Line<'static>> = Vec::with_capacity(end - start);

    for vi in start..end {
        let vl = &wrap.visual_lines[vi];
        let line = &vl.text;
        let line_chars = line.chars().count();
        let is_cursor_line = vi == wrap.cursor_visual_row;

        // 计算选区与本视觉行的重叠区间（全局坐标 → 视觉行内坐标）
        let sel_in_line: Option<(usize, usize)> =
            selection_range.and_then(|(sel_start, sel_end)| {
                let (v_start, v_end) = vl.char_range;
                let overlap_start = sel_start.saturating_sub(v_start);
                let overlap_end = (sel_end.saturating_sub(v_start)).min(line_chars);
                if overlap_start < overlap_end {
                    Some((overlap_start, overlap_end))
                } else {
                    None
                }
            });

        if is_cursor_line && show_cursor {
            // ── 光标行：光标 + 选区合并渲染 ──
            let target_col = wrap.cursor_visual_col;

            // 将 visual_col 映射到字符位置和字节
            let mut col = 0usize;
            let mut cut_byte = 0usize;
            for (i, ch) in line.char_indices() {
                let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                if col + cw > target_col {
                    break;
                }
                col += cw;
                cut_byte = i + ch.len_utf8();
            }
            let cursor_end_byte = if cut_byte < line.len() {
                line[cut_byte..]
                    .chars()
                    .next()
                    .map(|c| cut_byte + c.len_utf8())
                    .unwrap_or(line.len())
            } else {
                line.len()
            };

            // 分段构建 spans
            let mut split_points: Vec<usize> = vec![0, line.len(), cut_byte, cursor_end_byte];
            if let Some((s_start, s_end)) = sel_in_line {
                split_points.push(char_index_to_byte(line, s_start));
                split_points.push(char_index_to_byte(line, s_end));
            }
            split_points.sort();
            split_points.dedup();

            let mut spans: Vec<Span<'static>> = Vec::new();
            for i in 0..split_points.len() - 1 {
                let seg_start = split_points[i];
                let seg_end = split_points[i + 1];
                if seg_start >= seg_end || seg_start >= line.len() {
                    continue;
                }
                let seg = &line[seg_start..seg_end.min(line.len())];
                if seg.is_empty() {
                    continue;
                }
                let style = if seg_start >= cut_byte && seg_end <= cursor_end_byte {
                    cursor_style
                } else if let Some((s_start, s_end)) = sel_in_line {
                    let s_s_byte = char_index_to_byte(line, s_start);
                    let s_e_byte = char_index_to_byte(line, s_end);
                    if seg_start >= s_s_byte && seg_end <= s_e_byte {
                        selection_style
                    } else {
                        Style::default()
                    }
                } else {
                    Style::default()
                };
                spans.push(Span::styled(seg.to_string(), style));
            }

            // 光标在视觉行尾时追加 styled space。
            // CR 修正：移除 !spans.is_empty() 条件——空视觉行（如空逻辑行）
            // 上 spans 为空但光标仍需可见。
            let line_display_w: usize = line
                .chars()
                .map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(0))
                .sum();
            if target_col >= line_display_w {
                spans.push(Span::styled(" ", cursor_style));
            }
            result.push(Line::from(spans));
        } else if let Some((s_start, s_end)) = sel_in_line {
            // ── 非光标行：仅选区高亮 ──
            let s_s_byte = char_index_to_byte(line, s_start);
            let s_e_byte = char_index_to_byte(line, s_end);
            let mut spans: Vec<Span<'static>> = Vec::new();
            if s_s_byte > 0 {
                spans.push(Span::raw(line[..s_s_byte].to_string()));
            }
            spans.push(Span::styled(
                line[s_s_byte..s_e_byte].to_string(),
                selection_style,
            ));
            if s_e_byte < line.len() {
                spans.push(Span::raw(line[s_e_byte..].to_string()));
            }
            result.push(Line::from(spans));
        } else {
            // ── 纯文本行 ──
            result.push(Line::from(line.to_string()));
        }
    }
    result
```

- [ ] **Step 3: 更新 `widget.rs`**

在 `widget.rs` 的 `render_ref` 方法中修改调用，传入 `area.width`：

```rust
    fn render_ref(&self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let placeholder = if state.placeholder.is_empty() {
            None
        } else {
            Some(state.placeholder.as_str())
        };
        let max_width = area.width.saturating_sub(2).max(1) as usize;
        let viewport_height = area.height as usize;
        let lines = render_multiline_with_cursor(
            &state.text,
            state.cursor,
            self.cursor_style,
            state.selection_range(),
            self.selection_style,
            placeholder,
            self.placeholder_style,
            max_width,
            viewport_height,
            self.loading,
            self.show_cursor,
        );
```

- [ ] **Step 4: 更新 `mod.rs` 导出**

```rust
pub use render::{display_width_before, render_multiline_with_cursor, wrap_text, VisualLine, WrapResult};
```

- [ ] **Step 5: 更新 `render_test.rs` 所有调用点**

所有 `render_multiline_with_cursor` 调用增加 `max_width: 80`（足够宽，不触发折行）。约 6 处。

```rust
        let result = render_multiline_with_cursor(
            &text,
            cursor,
            cursor_style(),
            None,
            sel_style(),
            None,
            ph_style(),
            80,              // 新增 max_width
            viewport_height,
            false,
            true,
        );
```

- [ ] **Step 6: 更新 `state_test.rs` 所有调用点**

所有 `render_multiline_with_cursor` 调用增加 `max_width: 12`。约 12 处。

```rust
    let lines = render_multiline_with_cursor(
        "",
        0,
        cursor_style(),
        None,
        sel_style(),
        None,
        ph_style(),
        12,
        1,
        false,
        true,
    );
```

- [ ] **Step 7: 运行测试验证**

```bash
cargo test -p peri-widgets --lib -- textarea 2>&1
```

预期：所有现有测试通过（max_width 足够大，不触发折行）。

- [ ] **Step 8: 提交（单次）**

```bash
git add peri-widgets/src/textarea/render.rs peri-widgets/src/textarea/widget.rs \
        peri-widgets/src/textarea/mod.rs peri-widgets/src/textarea/render_test.rs \
        peri-widgets/src/textarea/state_test.rs
git commit -m "feat(textarea): add soft wrapping — rewrite render_multiline_with_cursor + update all callers"
```

---

### Task 3: 新增 `wrap_text` 单元测试

**Files:**
- Modify: `peri-widgets/src/textarea/render_test.rs`

- [ ] **Step 1: 追加测试函数到 render_test.rs**

在文件末尾追加（注意 `wrap_text` 已移除 `desired_col` 参数，所有调用改为 3 参数）：

```rust
// ── wrap_text 折行测试 ──────────────────────────────

/// ASCII 文本 6 字符，max_width=3，应折成 2 行 "abc" / "def"
#[test]
fn test_wrap_text_ascii_splits_at_width() {
    let result = wrap_text("abcdef", 4, 3); // cursor at 'e' (index 4)
    assert_eq!(result.total_visual_rows, 2);
    assert_eq!(result.visual_lines[0].text, "abc");
    assert_eq!(result.visual_lines[1].text, "def");
    assert_eq!(result.cursor_visual_row, 1);
    assert_eq!(result.cursor_visual_col, 1);
}

/// CJK 文本 "你好世界" (4 chars, 8 cols)，max_width=4，应折成 "你好"/"世界"
#[test]
fn test_wrap_text_cjk_splits_at_half() {
    let result = wrap_text("你好世界", 2, 4); // cursor at '世' (index 2)
    assert_eq!(result.total_visual_rows, 2);
    assert_eq!(result.visual_lines[0].text, "你好");
    assert_eq!(result.visual_lines[1].text, "世界");
    assert_eq!(result.cursor_visual_row, 1);
    assert_eq!(result.cursor_visual_col, 0);
}

/// 短文本不折行
#[test]
fn test_wrap_text_short_no_wrap() {
    let result = wrap_text("abc", 1, 10);
    assert_eq!(result.total_visual_rows, 1);
    assert_eq!(result.visual_lines[0].text, "abc");
    assert_eq!(result.cursor_visual_row, 0);
    assert_eq!(result.cursor_visual_col, 1);
}

/// max_width=1 时只折行不截断（CJK char 宽 2 也容纳）
#[test]
fn test_wrap_text_min_width_does_not_truncate() {
    let result = wrap_text("你", 1, 1);
    assert_eq!(result.total_visual_rows, 1);
    assert_eq!(result.visual_lines[0].text, "你");
}

/// 空文本返回 1 个空视觉行
#[test]
fn test_wrap_text_empty_returns_one_empty_line() {
    let result = wrap_text("", 0, 10);
    assert_eq!(result.total_visual_rows, 1);
    assert_eq!(result.visual_lines[0].text, "");
    assert_eq!(result.cursor_visual_row, 0);
    assert_eq!(result.cursor_visual_col, 0);
}

/// 多逻辑行 + 折行混合
#[test]
fn test_wrap_text_multi_logical_with_wrap() {
    let text = "abc\ndefgh";
    let result = wrap_text(text, 5, 3);
    assert_eq!(result.total_visual_rows, 3);
    assert_eq!(result.visual_lines[0].text, "abc");
    assert_eq!(result.visual_lines[1].text, "def");
    assert_eq!(result.visual_lines[2].text, "gh");
    assert_eq!(result.cursor_visual_row, 1);
    assert_eq!(result.cursor_visual_col, 1);
}

/// 光标在文本末尾
#[test]
fn test_wrap_text_cursor_at_text_end() {
    let result = wrap_text("你好", 2, 4);
    assert_eq!(result.cursor_visual_row, 0);
    assert_eq!(result.cursor_visual_col, 4);
}

/// 空行 + 非空行混合
#[test]
fn test_wrap_text_empty_lines_preserved() {
    let text = "a\n\nb";
    let result = wrap_text(text, 2, 10); // cursor = 2 (在空行)
    assert_eq!(result.total_visual_rows, 3);
    assert_eq!(result.visual_lines[1].text, "");
    assert_eq!(result.cursor_visual_row, 1); // 光标在空行
    assert_eq!(result.cursor_visual_col, 0);
}
```

> **CR 修正**：移除 `test_wrap_text_desired_col_accepted` 测试（`desired_col` 已从 `wrap_text` 签名移除）。新增空行混合测试覆盖空视觉行光标场景。

- [ ] **Step 2: 运行测试**

```bash
cargo test -p peri-widgets --lib -- wrap_text
```

预期：8 个新测试全部通过。

- [ ] **Step 3: 提交**

```bash
git add peri-widgets/src/textarea/render_test.rs
git commit -m "test(textarea): add wrap_text unit tests"
```

---

### Task 4: `TextAreaState` 增加 `desired_col` 和视觉移动方法

**Files:**
- Modify: `peri-widgets/src/textarea/state.rs`

> **CR 修正**：undo/redo 加入清除列表；visual_row_col_to_cursor 加 debug_assert。

- [ ] **Step 1: 新增 `desired_col` 字段 + 更新 Default**

在第 34 行 `pub placeholder: String,` 之后增加：

```rust
    /// 上下移动时记忆的视觉列（display-width 列）。
    /// 水平移动或编辑时清除。
    pub desired_col: Option<usize>,
```

Default 实现增加 `desired_col: None,`。

- [ ] **Step 2: 新增 `cursor_visual_up` 和 `cursor_visual_down`**

```rust
    /// 上移一个视觉行（使用软换行折行信息），返回是否真的移动了。
    pub fn cursor_visual_up(&mut self, max_width: usize) -> bool {
        self.cancel_selection();
        if self.cursor == 0 {
            self.desired_col = None;
            return false;
        }
        let wrap = crate::textarea::render::wrap_text(
            &self.text, self.cursor, max_width.max(1),
        );
        if wrap.cursor_visual_row == 0 {
            self.desired_col = None;
            return false;
        }
        let target_row = wrap.cursor_visual_row - 1;
        let desired = self.desired_col.unwrap_or(wrap.cursor_visual_col);
        self.desired_col = Some(desired);
        self.cursor = Self::visual_row_col_to_cursor(
            &wrap.visual_lines, target_row, desired,
        );
        true
    }

    /// 下移一个视觉行（使用软换行折行信息），返回是否真的移动了。
    pub fn cursor_visual_down(&mut self, max_width: usize) -> bool {
        self.cancel_selection();
        let wrap = crate::textarea::render::wrap_text(
            &self.text, self.cursor, max_width.max(1),
        );
        if wrap.cursor_visual_row >= wrap.total_visual_rows.saturating_sub(1) {
            self.desired_col = None;
            return false;
        }
        let target_row = wrap.cursor_visual_row + 1;
        let desired = self.desired_col.unwrap_or(wrap.cursor_visual_col);
        self.desired_col = Some(desired);
        self.cursor = Self::visual_row_col_to_cursor(
            &wrap.visual_lines, target_row, desired,
        );
        true
    }
```

- [ ] **Step 3: 新增辅助函数 `visual_row_col_to_cursor`**（在 impl 块内，`char_to_byte` 之后）

```rust
    /// 给定视觉行列表、目标视觉行索引、视觉列，返回对应的全局字符索引。
    /// 若目标行比 desired_col 短，光标放在目标行尾。
    fn visual_row_col_to_cursor(
        visual_lines: &[crate::textarea::render::VisualLine],
        target_row: usize,
        desired_col: usize,
    ) -> usize {
        debug_assert!(
            target_row < visual_lines.len(),
            "visual_row_col_to_cursor: target_row out of bounds"
        );
        let vl = &visual_lines[target_row];
        let mut col = 0usize;
        for (i, ch) in vl.text.char_indices() {
            let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if col + cw > desired_col {
                return vl.char_range.0 + vl.text[..i].chars().count();
            }
            col += cw;
        }
        // desired_col 超出或等于行宽 → 行尾
        vl.char_range.0 + vl.text.chars().count()
    }
```

- [ ] **Step 4: 保留旧的 `cursor_line_up`/`cursor_line_down`（加 desired_col 清除）**

```rust
    /// 上移一行（逻辑行，无软换行信息时的回退方法）。
    pub fn cursor_line_up(&mut self) -> bool {
        self.cancel_selection();
        let (line, col) = self.cursor_line_col();
        if line == 0 {
            return false;
        }
        self.desired_col = None;
        self.cursor = Self::line_col_to_cursor(&self.text, line - 1, col);
        true
    }

    /// 下移一行（逻辑行，无软换行信息时的回退方法）。
    pub fn cursor_line_down(&mut self) -> bool {
        self.cancel_selection();
        let (line, col) = self.cursor_line_col();
        let last_line = self.text.matches('\n').count();
        if line >= last_line {
            return false;
        }
        self.desired_col = None;
        self.cursor = Self::line_col_to_cursor(&self.text, line + 1, col);
        true
    }
```

- [ ] **Step 5: 在所有移动和编辑操作中清除 `desired_col`**

在以下每个方法的 `self.cancel_selection();` 之后增加 `self.desired_col = None;`：

- `cursor_left`、`cursor_right`、`cursor_word_left`、`cursor_word_right`
- `cursor_home`、`cursor_end`、`cursor_line_home`、`cursor_line_end`

在以下每个方法的开头/`delete_selection` 之前增加 `self.desired_col = None;`：

- `insert_char`、`insert_str`、`backspace`、`delete_forward`
- `delete_word_backward`、`delete_word_forward`、`paste_yank`
- `clear`、`take_text`、`replace_all`、`replace_all_no_undo`、`replace_char_range`

> **CR 修正**：新增 `undo()` 和 `redo()` —— 在调用 `history.undo()`/`history.redo()` 后增加 `self.desired_col = None;`。

```rust
    pub fn undo(&mut self) -> bool {
        let result = self.history.undo(&mut self.text, &mut self.cursor, &mut self.selection_start);
        if result {
            self.desired_col = None;
        }
        result
    }

    pub fn redo(&mut self) -> bool {
        let result = self.history.redo(&mut self.text, &mut self.cursor, &mut self.selection_start);
        if result {
            self.desired_col = None;
        }
        result
    }
```

**注意**：当前 `undo`/`redo` 实现直接操作字段（`let Self { text, cursor, ..., history, .. } = self; history.undo(text, ...)`），需要适配为能在此之后设置 `desired_col`。建议将解构改为 `history.undo(&mut self.text, &mut self.cursor, &mut self.selection_start)` 模式，或保持原结构但在成功时显式设置。

- [ ] **Step 6: 编译并运行测试**

```bash
cargo test -p peri-widgets --lib -- state_test 2>&1
```

- [ ] **Step 7: 提交**

```bash
git add peri-widgets/src/textarea/state.rs
git commit -m "feat(textarea): add desired_col + cursor_visual_up/down for visual movement"
```

---

### Task 5: 在 `input_area.rs` 中连接软换行 + 鼠标点击适配

**Files:**
- Modify: `peri-tui/src/kit/input_area.rs`

> **CR 修正**：提取 magic number 为常量；新增鼠标点击 handler 的视觉行适配。

- [ ] **Step 1: 在文件顶部定义常量**

在 `use` 语句区域之后、第一个函数之前增加：

```rust
/// 输入区域 prompt + border 占用的列宽常量。
/// border 左右各 1 列，" ❯ " prompt 前缀占 3 列 → 共 5 列。
const PROMPT_AND_BORDER_WIDTH: u16 = 5;
```

- [ ] **Step 2: 更新渲染段——传入 max_width + 视觉行视口**

将第 507-508 行：

```rust
    let line_count = text.matches('\n').count() + 1;
    let editor_rows = (line_count as u16).clamp(1, 10);
```

替换为：

```rust
    let text_width = composer_area
        .map(|a| a.width.saturating_sub(PROMPT_AND_BORDER_WIDTH).max(1) as usize)
        .unwrap_or(80);
    let wrap = peri_widgets::textarea::wrap_text(&text, cursor, text_width);
    let editor_rows = (wrap.total_visual_rows as u16).clamp(1, 10);
```

然后在渲染调用处传入 `text_width`：

```rust
    let lines = render_multiline_with_cursor_for_themed(
        &text,
        cursor,
        selection_range,
        placeholder_str,
        text_width,          // 新增
        editor_rows as usize,
        loading,
        show_cursor,
    );
```

- [ ] **Step 3: 更新 `render_multiline_with_cursor_for_themed` 签名**

增加 `max_width: usize` 参数（第 4 个位置），透传给 `peri_widgets::textarea::render_multiline_with_cursor`。

- [ ] **Step 4: 更新 Up/Down 事件处理使用视觉移动**

在键盘事件闭包内（`KeyCode::Up` 和 `KeyCode::Down` 分支），将 `cursor_line_up()`/`cursor_line_down()` 替换为 `cursor_visual_up(tw)`/`cursor_visual_down(tw)`。`tw` 从 `composer_area` 即时计算：

```rust
                    KeyCode::Up if !is_ctrl && !mention_active && !slash_active => {
                        let tw = composer_area
                            .map(|a| a.width.saturating_sub(PROMPT_AND_BORDER_WIDTH).max(1) as usize)
                            .unwrap_or(80);
                        let moved = state.write().cursor_visual_up(tw);
                        if !moved {
                            let current = state.read().all_text();
                            if let Some(historical) = history_up(Some(&current)) {
                                state.write().replace_all_no_undo(historical);
                            }
                        }
                        EventResult::Consumed
                    }
                    KeyCode::Down if !is_ctrl && !mention_active && !slash_active => {
                        let tw = composer_area
                            .map(|a| a.width.saturating_sub(PROMPT_AND_BORDER_WIDTH).max(1) as usize)
                            .unwrap_or(80);
                        let moved = state.write().cursor_visual_down(tw);
                        if !moved && let Some(historical) = history_down() {
                            state.write().replace_all_no_undo(historical);
                        }
                        EventResult::Consumed
                    }
```

- [ ] **Step 5: 更新鼠标点击 handler（视觉行适配）**

> **CR 修正**：原鼠标点击 handler 使用 `text.split('\n')` 做逻辑行映射，软换行后需改用 `wrap_text` 做视觉行 → 光标映射。

在鼠标点击 handler（`display_col_to_char_idx` 调用附近，约第 434-449 行）中，将 `text.split('\n')` 的逻辑替换为基于 `wrap_text` 的视觉行映射：

```rust
                    if let Some(outer) = composer_area {
                        let ov_h = *overlay_height_cl.lock();
                        let composer_top = outer.y.saturating_add(ov_h).saturating_add(1);
                        let text_x = outer.x.saturating_add(3);
                        if mouse.row >= composer_top && mouse.column >= text_x {
                            let click_visual_row = mouse.row.saturating_sub(composer_top) as usize;
                            let click_display_col = mouse.column.saturating_sub(text_x) as usize;
                            let s = state_cl.read();
                            if !s.text.is_empty() {
                                let tw = outer.width.saturating_sub(PROMPT_AND_BORDER_WIDTH).max(1) as usize;
                                let wr = wrap_text(&s.text, s.cursor, tw);
                                if click_visual_row < wr.total_visual_rows {
                                    let vl = &wr.visual_lines[click_visual_row];
                                    let mut col = 0usize;
                                    let mut target_char = vl.char_range.0;
                                    for (i, ch) in vl.text.char_indices() {
                                        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                                        if col + cw > click_display_col {
                                            break;
                                        }
                                        col += cw;
                                        target_char = vl.char_range.0 + vl.text[..i + ch.len_utf8()].chars().count();
                                    }
                                    drop(s);
                                    state_cl.write().desired_col = None;
                                    state_cl.write().cursor = target_char;
                                }
                            }
                        }
                    }
```

- [ ] **Step 6: 编译检查**

```bash
cargo build -p peri-tui 2>&1 | head -30
```

- [ ] **Step 7: 提交**

```bash
git add peri-tui/src/kit/input_area.rs
git commit -m "feat(textarea): wire soft wrapping in input_area — max_width + visual movement + mouse click"
```

---

### Task 6: 新增视觉移动测试

**Files:**
- Modify: `peri-widgets/src/textarea/state_test.rs`

- [ ] **Step 1: 追加测试**

```rust
// ── 视觉行移动测试（soft wrapping）──────────────────

#[test]
fn test_visual_down_cjk_wrapped() {
    let mut s = TextAreaState::default();
    s.insert_str("你好世界");
    s.cursor = 0;
    assert!(s.cursor_visual_down(4));
    assert_eq!(s.cursor, 2);
}

#[test]
fn test_visual_down_at_last_visual_row_returns_false() {
    let mut s = TextAreaState::default();
    s.insert_str("你好世界");
    s.cursor = 4;
    assert!(!s.cursor_visual_down(4));
    assert_eq!(s.cursor, 4);
}

#[test]
fn test_visual_up_at_first_visual_row_returns_false() {
    let mut s = TextAreaState::default();
    s.insert_str("你好世界");
    s.cursor = 2;
    assert!(s.cursor_visual_up(4));
    assert_eq!(s.cursor, 0);
    assert!(!s.cursor_visual_up(4));
}

#[test]
fn test_desired_col_cleared_on_edit() {
    let mut s = TextAreaState::default();
    s.insert_str("你好世界");
    s.cursor_visual_down(4);
    assert!(s.desired_col.is_some());
    s.insert_char('x');
    assert!(s.desired_col.is_none());
}

#[test]
fn test_desired_col_cleared_on_horizontal_move() {
    let mut s = TextAreaState::default();
    s.insert_str("你好世界");
    s.cursor_visual_down(4);
    assert!(s.desired_col.is_some());
    s.cursor_left();
    assert!(s.desired_col.is_none());
}

/// undo 清除 desired_col
#[test]
fn test_desired_col_cleared_on_undo() {
    let mut s = TextAreaState::default();
    s.insert_str("你好世界");
    s.cursor_visual_down(4);
    assert!(s.desired_col.is_some());
    s.undo();
    assert!(s.desired_col.is_none());
}
```

- [ ] **Step 2: 运行测试**

```bash
cargo test -p peri-widgets --lib -- visual
```

预期：6 个新测试通过。

- [ ] **Step 3: 提交**

```bash
git add peri-widgets/src/textarea/state_test.rs
git commit -m "test(textarea): add visual movement tests"
```

---

### Task 7: 集成构建 + 全量测试

**Files:** 无变更

- [ ] **Step 1: 全量构建**

```bash
cargo build --workspace 2>&1 | tail -20
```

- [ ] **Step 2: 运行全部 textarea 测试**

```bash
cargo test -p peri-widgets --lib -- textarea 2>&1
```

- [ ] **Step 3: 运行全部项目测试**

```bash
cargo test --workspace 2>&1 | tail -30
```

- [ ] **Step 4: clippy**

```bash
cargo clippy -p peri-widgets -p peri-tui 2>&1 | tail -20
```

- [ ] **Step 5: 修复 lint 问题后提交（如有）**

---

## 自检清单

### 1. Spec 覆盖

| 需求 | Task |
|------|------|
| wrap_text 折行函数 | 1 |
| render_multiline_with_cursor + max_width + 调用方更新 | 2 |
| wrap_text 单元测试 | 3 |
| desired_col + 视觉移动 | 4 |
| input_area 连接 + mouse click | 5 |
| 视觉移动测试 | 6 |
| 集成构建 | 7 |

### 2. CR 修正确认

| 审查问题 | 状态 |
|----------|------|
| wrap_text 移除 unused desired_col | Task 1 ✅ |
| VisualLine 移除 char_start | Task 1 ✅ |
| 空行光标可见性 | Task 2 Step 2 ✅ |
| undo/redo 清除 desired_col | Task 4 Step 5 ✅ |
| visual_row_col_to_cursor debug_assert | Task 4 Step 3 ✅ |
| 鼠标点击视觉行适配 | Task 5 Step 5 ✅ |
| Task 2+3 合并提交 | Task 2 ✅ |
| magic number 提取常量 | Task 5 Step 1 ✅ |
| 空视觉行 + 非空行混合测试 | Task 3 ✅ |
| undo 清除 desired_col 测试 | Task 6 ✅ |

### 3. 已知限制（不在本 Plan 范围）

- Tab 字符 display width=0 —— 输入区 TAB 不常见，后期单独处理
- max_width=1 极窄终端 CJK 字符溢出视觉宽度但不截断（用户体验受限但不会崩溃）
- composer_area 有 1 帧延迟（resize 时折行可能滞后 1 帧，ratatui-kit 框架正常行为）
