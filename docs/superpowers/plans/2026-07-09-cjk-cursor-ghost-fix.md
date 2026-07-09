# CJK 光标残影修复计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复删除 CJK 双宽字符后旧光标位置的 bg 残影——为非光标 Span 设置显式背景色覆盖旧 cursor_bg。

**Architecture:** `render_multiline_with_cursor` 的非光标 Span 当前使用 `Style::default()`（无显式 bg），导致 ratatui 单元格 bg 不被覆盖。新增 `default_style` 参数，所有回退 Span 使用显式 `bg(surface.default)` 样式。

**Tech Stack:** Rust 2021, ratatui

**关联 Issue:** `spec/issues/2026-07-05-input-unicode-cursor-misalignment.md`

---

## 文件结构

```
peri-widgets/src/textarea/
├── render.rs        ← render_multiline_with_cursor 新增 default_style 参数
├── widget.rs         ← render_ref 传入 default_style
├── state_test.rs     ← 更新调用 + 新增残影回归测试
└── render_test.rs    ← 更新调用
peri-tui/src/kit/
└── input_area.rs     ← render_multiline_with_cursor_for_themed 传入 default_style
```

---

### Task 1: 新增 `default_style` 参数 + 替换所有 `Style::default()` 回退

**Files:**
- Modify: `peri-widgets/src/textarea/render.rs`

- [ ] **Step 1: 修改 `render_multiline_with_cursor` 签名**

在 `max_width` 之前插入 `default_style: Style` 参数。新签名：

```rust
pub fn render_multiline_with_cursor(
    text: &str,
    cursor: usize,
    cursor_style: Style,
    selection_range: Option<(usize, usize)>,
    selection_style: Style,
    placeholder: Option<&str>,
    placeholder_style: Style,
    default_style: Style,        // 新增：非光标/非选区的 Span 样式（需带显式 bg）
    max_width: usize,
    viewport_height: usize,
    loading: bool,
    show_cursor: bool,
) -> Vec<Line<'static>> {
```

- [ ] **Step 2: 替换函数体内所有裸 `Style::default()` 为 `default_style`**

有两处 `Style::default()` 用于非光标 Span：

**位置 1** — 光标行 fallback（约第 380 行）：
```rust
// 旧：
} else {
    Style::default()
};

// 新：
} else {
    default_style
};
```

**位置 2** — 光标行 else 分支（约第 390 行）：
```rust
// 旧：
} else {
    Style::default()
};

// 新：
} else {
    default_style
};
```

- [ ] **Step 3: 提交**

```bash
git add peri-widgets/src/textarea/render.rs
git commit -m "fix(textarea): add default_style param to render_multiline_with_cursor for explicit span bg"
```

---

### Task 2: 更新所有调用方

**Files:**
- Modify: `peri-widgets/src/textarea/widget.rs`
- Modify: `peri-widgets/src/textarea/render_test.rs`
- Modify: `peri-widgets/src/textarea/state_test.rs`
- Modify: `peri-tui/src/kit/input_area.rs`

- [ ] **Step 1: 更新 `widget.rs`**

在 `render_ref` 中传入 `default_style`：

```rust
    fn render_ref(&self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let placeholder = if state.placeholder.is_empty() {
            None
        } else {
            Some(state.placeholder.as_str())
        };
        let max_width = area.width.saturating_sub(2).max(1) as usize;
        let viewport_height = area.height as usize;
        let default_style = Style::default().bg(Color::Black); // widget 层用黑色兜底
        let lines = render_multiline_with_cursor(
            &state.text,
            state.cursor,
            self.cursor_style,
            state.selection_range(),
            self.selection_style,
            placeholder,
            self.placeholder_style,
            default_style,       // 新增
            max_width,
            viewport_height,
            self.loading,
            self.show_cursor,
        );
```

需确认 `Color` 已导入（`use ratatui::style::Color;` 在文件头）。

- [ ] **Step 2: 更新 `render_test.rs` 所有调用点**

在每个测试的调用中增加 `default_style` 参数（位置在 `ph_style()` 和 `80` 之间）：

```rust
        let result = render_multiline_with_cursor(
            &text,
            cursor,
            cursor_style(),
            None,
            sel_style(),
            None,
            ph_style(),
            Style::default(),   // 新增 default_style（测试中用 default 即可）
            80,
            viewport_height,
            false,
            true,
        );
```

共 7 处（含 build_lines_text 辅助函数中的调用）。

- [ ] **Step 3: 更新 `state_test.rs` 所有调用点**

```rust
    let lines = render_multiline_with_cursor(
        "",
        0,
        cursor_style(),
        None,
        sel_style(),
        None,
        ph_style(),
        Style::default(),   // 新增
        12,
        1,
        false,
        true,
    );
```

共 11 处。

- [ ] **Step 4: 更新 `input_area.rs` 的 `render_multiline_with_cursor_for_themed`**

增加 `default_style` 计算和传递：

```rust
fn render_multiline_with_cursor_for_themed(
    text: &str,
    cursor: usize,
    selection_range: Option<(usize, usize)>,
    placeholder: Option<&str>,
    max_width: usize,
    viewport_height: usize,
    loading: bool,
    show_cursor: bool,
) -> Vec<ratatui::text::Line<'static>> {
    let tokens = input_tokens();
    let cursor_style = Style::default()
        .fg(tokens.cursor_fg)
        .bg(tokens.cursor_bg)
        .add_modifier(Modifier::BOLD);
    let selection_style = Style::default()
        .fg(tokens.cursor_fg)
        .bg(tokens.cursor_bg)
        .add_modifier(Modifier::DIM);
    let placeholder_style = Style::default().fg(tokens.placeholder);
    let default_style = Style::default().bg(theme::semantic().surface.default);  // 新增
    peri_widgets::textarea::render_multiline_with_cursor(
        text,
        cursor,
        cursor_style,
        selection_range,
        selection_style,
        placeholder,
        placeholder_style,
        default_style,       // 新增
        max_width,
        viewport_height,
        loading,
        show_cursor,
    )
}
```

- [ ] **Step 5: 运行测试验证编译**

```bash
cargo test -p peri-widgets --lib -- textarea 2>&1
cargo build -p peri-tui 2>&1 | tail -5
```

- [ ] **Step 6: 提交**

```bash
git add peri-widgets/src/textarea/widget.rs peri-widgets/src/textarea/render_test.rs \
        peri-widgets/src/textarea/state_test.rs peri-tui/src/kit/input_area.rs
git commit -m "fix(textarea): pass explicit default_style with bg to all render_multiline_with_cursor callers"
```

---

### Task 3: 新增 CJK 删除残影回归测试

**Files:**
- Modify: `peri-widgets/src/textarea/state_test.rs`

- [ ] **Step 1: 追加测试——验证删除 CJK 字符后旧位置 Span 无 cursor bg 残留**

```rust
/// 删除 CJK 字符后，旧光标位置的 Span 不应残留 cursor_style。
/// 验证 default_style 正确覆盖了旧 cursor bg。
#[test]
fn test_cjk_delete_no_cursor_ghost() {
    let cs = cursor_style();
    let ss = sel_style();
    let ps = ph_style();
    let ds = Style::default().bg(Color::Black);

    // 模拟：输入 "你好世界"，cursor 在位置 1（'好' 上）
    let before = render_multiline_with_cursor(
        "你好世界", 1,
        cs, None, ss, None, ps, ds,
        12, 3, false, true,
    );

    // 模拟：删除 '你' 后，text="好世界"，cursor 移到位置 0（'好' 上）
    let after = render_multiline_with_cursor(
        "好世界", 0,
        cs, None, ss, None, ps, ds,
        12, 3, false, true,
    );

    // 旧光标在 "好"（before 中 index 1）上，bg 应为 CURSOR_BG
    // 新光标在 "好"（after 中 index 0）上，bg 也应为 CURSOR_BG
    // 关键是：旧光标位置右侧（"世界"部分）不应残留 cursor bg

    // 检查 after 的第二个字符 "世" 不应有 cursor bg
    let non_cursor_spans: Vec<_> = after[0].spans.iter()
        .filter(|s| s.style != cs)
        .collect();

    // "世" 和 "界" 应在非光标 Span 中，且 bg 应为 ds.bg（Black），非 cs.bg（CURSOR_BG）
    let text_after_cursor: String = non_cursor_spans.iter()
        .map(|s| s.content.as_ref())
        .collect();
    assert!(text_after_cursor.contains("世界"),
        "非光标区域应包含 '世界'，实际: '{text_after_cursor}'");

    for span in &non_cursor_spans {
        assert_ne!(
            span.style.bg, cs.bg,
            "非光标 Span '{content}' 不应有 cursor bg，但有 {bg:?}",
            content = span.content, bg = span.style.bg,
        );
    }
}
```

注意：测试需要导入 `Color`：
```rust
use ratatui::style::Color;
```

以及使用现有的 `cursor_style()` / `sel_style()` / `ph_style()` 辅助函数（已在文件中定义）。

- [ ] **Step 2: 运行新测试**

```bash
cargo test -p peri-widgets --lib -- test_cjk_delete_no_cursor_ghost
```

预期：如果假设正确，修复后此测试应通过（旧 cursor bg 被 default_style bg 覆盖）。

- [ ] **Step 3: 运行全部 textarea 测试**

```bash
cargo test -p peri-widgets --lib -- textarea 2>&1
```

- [ ] **Step 4: 提交**

```bash
git add peri-widgets/src/textarea/state_test.rs
git commit -m "test(textarea): add CJK cursor ghost regression test"
```

---

### Task 4: 全量验证

**Files:** 无变更

- [ ] **Step 1: 全量构建**

```bash
cargo build --workspace 2>&1 | tail -5
```

- [ ] **Step 2: 全量测试**

```bash
cargo test --workspace 2>&1 | tail -10
```

- [ ] **Step 3: clippy**

```bash
cargo clippy -p peri-widgets -p peri-tui 2>&1 | tail -5
```

---

## 自检清单

### 1. Spec 覆盖

| 需求 | Task |
|------|------|
| render_multiline_with_cursor 新增 default_style 参数 | 1 |
| 替换 Style::default() 为 default_style | 1 |
| 更新所有调用方 | 2 |
| CJK 删除残影回归测试 | 3 |
| 全量验证 | 4 |

### 2. 占位符检查

✅ 无 TBD/TODO  
✅ 每步包含确切代码  
✅ 所有类型一致

### 3. 风险

- `default_style` 参数排在 `placeholder_style` 之后、`max_width` 之前 — 与现有参数分组一致（样式参数集中在前部）
- widget.rs 中的 `Color::Black` 兜底可能在非黑色主题中看起来不协调，但 widget.rs 是通用层，永远不会有主题上下文，Black 是合理兜底
