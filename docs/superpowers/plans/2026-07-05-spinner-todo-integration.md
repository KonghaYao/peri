# SpinnerWidget + Todo 列表集成实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 用 `peri-widgets::spinner::SpinnerWidget` 替换 message_area 中硬编码的 spinner 行，并添加 Todo 列表渲染 stub。

**Architecture:** SpinnerWidget 底层就是 `Paragraph::new(Line::from(spans))`——与现有 `all_lines: Vec<Line>` 渲染路径同构。方案是在 spinner 层提取 `render_to_lines()` 辅助函数直接产出 `Vec<Line>`，在 message_area 中替换硬编码字符串。Todo 列表同理：独立 `render_todo_lines()` 产出 `Vec<Line>`，追加到 `all_lines`。数据通道（Todo 数据的 ACP 事件 → atom 链路）作为后续议题，本次仅做渲染 stub。

**Tech Stack:** Rust 2021 + ratatui + ratatui-kit

**文件总览：**

| 文件 | 职责 | 变更类型 |
|------|------|---------|
| `peri-widgets/src/spinner/mod.rs` | 新增 `render_to_lines()` 方法 | 扩展 |
| `peri-tui/src/kit/message_area.rs` | 替换硬编码 spinner，添加 Todo 渲染 stub | 重构/扩展 |

---

### Task 1: SpinnerState 新增 `render_to_lines()` 方法

**Files:**
- Modify: `peri-widgets/src/spinner/mod.rs`

> 此任务在 `peri-widgets` crate 中操作，完成后不影响 TUI 层。

- [ ] **Step 1: 读取 SpinnerWidget::render_ref 的渲染逻辑**

已有代码（`spinner/mod.rs:173-209`）：
```rust
impl WidgetRef for SpinnerWidget<'_> {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        let frame = animation::tick_to_frame(self.state.tick());
        let orange = Style::default().fg(self.primary_color);
        let gray = Style::default().fg(self.secondary_color);

        let mut spans: Vec<Span<'_>> = vec![];
        spans.push(Span::styled(format!("{} ", frame), orange));
        spans.push(Span::styled(self.state.verb().to_string(), orange));

        let elapsed = self.state.elapsed_ms();
        let displayed_tokens = self.state.displayed_tokens();
        let mut suffix_parts = Vec::new();
        if self.show_elapsed {
            suffix_parts.push(animation::format_elapsed(elapsed));
        }
        if self.show_tokens && displayed_tokens > 0 {
            suffix_parts.push(format!("↓ {} tokens", animation::format_tokens(displayed_tokens)));
        }
        if !suffix_parts.is_empty() {
            spans.push(Span::styled(format!(" ({}", suffix_parts.join(" · ")), gray));
            spans.push(Span::styled(")", gray));
        }

        Paragraph::new(Line::from(spans)).render(area, buf);
    }
}
```

- [ ] **Step 2: 从渲染逻辑中提取 `render_to_lines()` 加到 `SpinnerState`**

```rust
// 在 impl SpinnerState 块中添加:

/// 将 spinner 渲染为 Vec<Line>，供 TUI 消息区直接追加到 all_lines 中
pub fn render_to_lines(&self, primary: Color, secondary: Color, show_elapsed: bool, show_tokens: bool) -> Vec<Line<'static>> {
    let frame = animation::tick_to_frame(self.tick());  // getter, 非 pub 字段
    let mut spans: Vec<Span<'static>> = vec![];

    spans.push(Span::styled(
        format!("{} ", frame),
        Style::default().fg(primary),
    ));
    spans.push(Span::styled(
        self.verb().to_string(),  // getter
        Style::default().fg(primary),
    ));

    let mut suffix_parts = Vec::new();
    if show_elapsed {
        suffix_parts.push(animation::format_elapsed(self.elapsed_ms()));
    }
    if show_tokens && self.displayed_tokens() > 0 {  // getter
        suffix_parts.push(format!(
            "↓ {} tokens",
            animation::format_tokens(self.displayed_tokens())  // getter
        ));
    }
    if !suffix_parts.is_empty() {
        spans.push(Span::styled(
            format!(" ({}", suffix_parts.join(" · ")),
            Style::default().fg(secondary),
        ));
        spans.push(Span::styled(")", Style::default().fg(secondary)));
    }

    vec![Line::from(spans)]
}
```

- [ ] **Step 3: 在 SpinnerWidget::render_ref 中复用 `render_to_lines()`**

修改 `SpinnerWidget::render_ref` 以调用 `self.state.render_to_lines()` 避免代码重复：

```rust
impl WidgetRef for SpinnerWidget<'_> {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        let lines = self.state.render_to_lines(
            self.primary_color,
            self.secondary_color,
            self.show_elapsed,
            self.show_tokens,
        );
        Paragraph::new(lines[0].clone()).render(area, buf);
    }
}
```

- [ ] **Step 4: 添加 compact_mode 支持（通过 getter）**

`SpinnerState` 字段为 private——添加 setter：

```rust
// SpinnerState 中添加:
pub fn set_compact_mode(&mut self, compact: bool) {
    self.compact_mode = compact;
}
```

调用方在 compact 时传 `thinking` 色给 `render_to_lines()` 的 `primary` 参数即可，无需在 spinner 层内置颜色切换。

- [ ] **Step 5: 构建和测试**

```bash
cargo build -p peri-widgets 2>&1 | tail -20
cargo test -p peri-widgets --lib -- spinner 2>&1 | tail -20
```

期望：编译通过，全部测试 PASS。

- [ ] **Step 6: Commit**

```bash
git add peri-widgets/src/spinner/mod.rs
git commit -m "feat(spinner): add SpinnerState::render_to_lines() for TUI message area integration

Extracts Vec<Line> generation from WidgetRef::render_ref so message_area
can append spinner lines directly to all_lines without mixing Widget/Buffer
rendering paths. Adds compact_mode flag for color switching."
```

---

### Task 2: message_area 用 SpinnerState 替换硬编码 spinner

**Files:**
- Modify: `peri-tui/src/kit/message_area.rs`

- [ ] **Step 1: 定位当前硬编码 spinner 位置**

读取 `message_area.rs:152-163`：

```rust
// 当前代码（约）:
if is_loading {
    all_lines.push(Line::from(Span::styled(
        "◜ 思考中…",
        Style::default().fg(semantic().status.running),
    )));
}
```

- [ ] **Step 2: 添加 SpinnerState hooks**

在 `MessageArea` 函数组件中（`hooks` 参数附近），添加：

```rust
use peri_widgets::spinner::{SpinnerState, SpinnerMode};
use std::time::Instant;

let spinner_state = hooks.use_state(|| SpinnerState::new(SpinnerMode::Thinking));

// 每帧推进 tick
let tick_advanced = hooks.use_state(|| false);
if !*tick_advanced.read() {
    spinner_state.write().advance_tick();
    *tick_advanced.write() = true;
}
```

**注意**：如果 `SpinnerState` 字段不是 `pub`，需要先读取源码确认字段可见性；若不可见则用 `SpinnerState::new(SpinnerMode::Thinking)` 替代。

- [ ] **Step 3: 替换 spinner 渲染行**

```rust
// 修改后:
if is_loading {
    let theme = crate::kit::theme::current();
    let sem = theme.semantic();
    let spinner_lines = spinner_state.read().render_to_lines(
        sem.status.running,          // primary (accent/loading)
        sem.text.muted,              // secondary
        true,                         // show_elapsed
        true,                         // show_tokens
    );
    for line in spinner_lines {
        all_lines.push(line);
    }
}
```

- [ ] **Step 4: 处理 wrap_map 同步**

在 `wrap_map` 构建逻辑中（约 line 308-318），spinner 行始终为 1 行高（`visual_height: 1`），无需修改：

```rust
// 现有逻辑已正确处理 — spinner 追加到 all_lines 末尾后,
// total_visual_rows 和 cumulative_heights 自动包含了它
```

- [ ] **Step 5: 构建和测试**

```bash
cargo build -p peri-tui -p peri-widgets 2>&1 | tail -20
cargo test -p peri-tui --lib -- message_area 2>&1 | tail -20
```

期望：编译通过，全部测试 PASS。

- [ ] **Step 6: Commit**

```bash
git add peri-tui/src/kit/message_area.rs
git commit -m "refactor(tui): replace hardcoded spinner with SpinnerState::render_to_lines()

Uses SpinnerState from peri-widgets to generate spinner Vec<Line> with
animation frames, verb, elapsed time, and token count. Eliminates raw
'◜ 思考中…' string."
```

---

### Task 3: Todo 列表渲染 stub

**Files:**
- Modify: `peri-tui/src/kit/message_area.rs`

> **说明**：Todo 列表的数据通道（ACP SessionUpdate::Plan → TUI atom）尚未实现。本 task 创建渲染 stub——函数签名接受静态 `Vec<TodoItem>`，供后续数据通道接入后直接使用。

- [ ] **Step 1: 定义 TodoItem 和渲染函数**

在 `message_area.rs` 中（组件函数外部）添加：

```rust
/// Todo 列表项状态
#[derive(Debug, Clone, PartialEq, Eq)]
enum TodoStatus {
    InProgress,
    Completed,
    Pending,
}

/// Todo 列表项（后续从 ACP Plan 事件映射而来）
#[derive(Debug, Clone)]
struct TodoItem {
    status: TodoStatus,
    content: String,
}

/// 渲染 Todo 列表为 Vec<Line>
fn render_todo_lines(items: &[TodoItem]) -> Vec<Line<'static>> {
    use ratatui::style::Modifier;
    let sem = crate::kit::theme::semantic();
    let mut lines = Vec::new();

    for item in items {
        let (icon, icon_color, text_color, crossed) = match item.status {
            TodoStatus::InProgress => ("◼", sem.accent, sem.text.primary, false),
            TodoStatus::Completed => ("✔", sem.status.success, sem.text.muted, true),
            TodoStatus::Pending => ("◻", sem.text.muted, sem.text.muted, false),
        };

        let mut prefix_style = Style::default().fg(icon_color).add_modifier(Modifier::BOLD);
        let mut text_style = Style::default().fg(text_color);
        if crossed {
            text_style = text_style.add_modifier(Modifier::CROSSED_OUT);
        }

        let prefix = Span::styled(format!("  {}  ", icon), prefix_style);

        let mut content = item.content.clone();
        if item.status == TodoStatus::Pending {
            content.push_str(" (可开始)");
        }
        let text = Span::styled(content, text_style);

        lines.push(Line::from(vec![prefix, text]));
    }

    // 尾部 3 行空行
    for _ in 0..3 {
        lines.push(Line::from(""));
    }

    lines
}
```

- [ ] **Step 2: 集成到 all_lines**

在 spinner 渲染之后、消息流末尾，添加 Todo 渲染：

```rust
// 在 spinner 追加之后（约 line 163）:
// ── Todo 列表（数据通道接入后替换 empty vec） ──
{
    let todo_items: &[TodoItem] = &[]; // TODO: 从 plan atom 读取
    if !todo_items.is_empty() {
        for line in render_todo_lines(todo_items) {
            all_lines.push(line);
        }
    }
}
```

- [ ] **Step 3: 编写渲染测试**

在 `message_area.rs` 所在 crate 的测试文件中添加：

```rust
#[test]
fn test_render_todo_lines_inprogress() {
    use super::*;
    let items = vec![TodoItem { status: TodoStatus::InProgress, content: "整理设计".into() }];
    let lines = render_todo_lines(&items);
    assert_eq!(lines.len(), 4); // 1 item + 3 trailing blanks
    let first = &lines[0].spans;
    assert!(first[0].content.contains("◼")); // icon
    assert!(first[1].content.contains("整理设计"));
}

#[test]
fn test_render_todo_lines_empty() {
    let lines = render_todo_lines(&[]);
    assert_eq!(lines.len(), 3); // only 3 trailing blanks
}
```

- [ ] **Step 4: 运行测试**

```bash
cargo test -p peri-tui --lib -- todo 2>&1 | tail -20
```

期望：FAIL（函数尚未定义于测试可见范围，需确认模块可见性）。确认后可调整导入。

**注意**：如果 `render_todo_lines` 放在组件函数内部（非 `pub`），测试需放在同一文件内或提取为独立模块。若测试放置复杂，skip 此步骤——渲染函数可在 Task 2 提测时通过集成测试验证。

- [ ] **Step 5: 构建和测试**

```bash
cargo build -p peri-tui 2>&1 | tail -20
```

- [ ] **Step 6: Commit**

```bash
git add peri-tui/src/kit/message_area.rs
git commit -m "feat(tui): add Todo list render_todo_lines() stub per §2.4.5

◼ InProgress (accent+BOLD), ✔ Completed (success+CROSSED_OUT),
◻ Pending (muted) with (可开始) hint. 3 trailing blank lines.
Data channel (ACP Plan→atom) deferred to separate issue."
```

---

## 完成标准

- [ ] `cargo build -p peri-widgets -p peri-tui` 零错误零 warning
- [ ] `cargo test -p peri-widgets --lib -- spinner` 全部 PASS
- [ ] `cargo test -p peri-tui --lib` 全部 PASS
- [ ] spinner 动画帧、elapsed time、token count 在 TUI 中正确显示
- [ ] Todo stub 编译通过，空列表不插入额外行

## 后续议题

- **Todo 数据通道**：需要将 ACP `SessionUpdate::Plan` 事件映射到 TUI atom，供 `render_todo_lines()` 消费。涉及 `view_mapper.rs` → `render_bridge.rs` → `message_area.rs` 三点链路。
- **SpinnerState 生命周期管理**：当前方案在每帧 `render` 中 `advance_tick()`，但这会与 `use_state` 的持久化语义冲突——同一帧可能多次调用 render。需要将 tick 推进移到 `use_effect` 或 TUI 主循环中（与现有 spinner 相似的推进位置）。
