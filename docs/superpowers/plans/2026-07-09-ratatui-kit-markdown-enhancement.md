# ratatui-kit-markdown 两阶段异步渲染增强

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 增强 ratatui-kit-markdown 的 `Markdown` 组件，支持两阶段异步渲染（light fallback → async heavy with syntect），使 total_height 精确（width-aware）。

**Architecture:** 利用 ratatui-kit 0.10.1 `use_previous_size` hook 检测首帧（width=0 → light 模式），CodeBlock 跳过高亮 (<1ms)。第二帧 width 精确后自动切换为完整 syntect 高亮。`use_async_state` 方案因 `AnyElement` 不实现 `Send` 而弃用，改用更简单的首帧检测方案。

**Tech Stack:** ratatui-kit 0.10.1 hooks, pulldown-cmark 0.12, syntect 5 (optional)

**Source repo:** `yexiyue/ratatui-kit-contrib`（crates/ratatui-kit-markdown）

---

## File Structure

| 文件 | 操作 | 职责 |
|------|------|------|
| `src/code_block.rs` | Modify | 加 `light` prop，true 时跳过高亮走 `build_text_plain()` |
| `src/markdown/mod.rs` | Modify | 加 width-aware 渲染 + 两阶段异步 |
| `Cargo.toml` | Modify | 加 `tokio` 为 `spawn_blocking` 依赖（已有 dev-dependency，需转为 optional dependency） |

---

### Task 1: CodeBlock 加 `light` prop

**Files:**
- Modify: `crates/ratatui-kit-markdown/src/code_block.rs`

- [ ] **Step 1: 在 CodeBlockProps 加 `light` 字段**

在 `CodeBlockProps` struct 中加 `light` 字段（line 33-58，在 `#[derive(Props)]` 下方）：

```rust
/// 轻量模式：跳过高亮，直接使用 `build_text_plain()`。
/// 用于 Markdown 组件首帧 fallback，避免 syntect 阻塞 UI 线程。
pub light: Option<bool>,
```

在 `Default` impl（line 60-82）加默认值：

```rust
light: None,
```

在 `from_props`（line 100-118）读取：

```rust
light: props.light.unwrap_or(false),
```

在 `CodeBlock` struct（line 85-97）加字段：

```rust
light: bool,
```

- [ ] **Step 2: 修改 `build_text()` 分派逻辑**

改 `build_text()`（line 214-216）为条件分派：

```rust
fn build_text(&self) -> Text<'static> {
    if self.light {
        self.build_text_plain()
    } else {
        self.build_text_highlighted()
    }
}
```

- [ ] **Step 3: Markdown mod.rs 中的 build_row 传 `light` prop**

在 `build_row` 函数（`markdown/mod.rs` line 343-353）的 `RenderRow::Code` 分支加 `light` prop：

```rust
RenderRow::Code { lang, lines } => {
    let line_count = lines.len() as u16;
    element! {
        CodeBlock(
            lines: lines,
            lang: lang,
            show_border: false,
            show_line_numbers: false,
            light: Some(light),
            height: Constraint::Length(line_count),
        )
    }
    .into_any()
}
```

需要 `build_row` 函数签名加 `light: bool` 参数。

---

### Task 2: 渲染管线加 width + light 参数

**Files:**
- Modify: `crates/ratatui-kit-markdown/src/markdown/mod.rs`

- [ ] **Step 1: `build_row` 函数签名加 `light` 参数**

Line 329:

```rust
fn build_row(row: RenderRow, theme: &MarkdownTheme, light: bool) -> AnyElement<'static> {
```

CodeBlock 分支传 `light: Some(light)`（见 Task 1 Step 3）。

- [ ] **Step 2: `render_blocks_with_theme` 加 `light` 参数**

Line 392，加上 `light` 参数并透传到 `build_row`：

```rust
pub fn render_blocks_with_theme(
    blocks: &[ParsedBlock],
    theme: &MarkdownTheme,
    light: bool,
) -> RenderedMarkdown {
    let rows = render_rows_with_theme(blocks, theme);
    let mut total_height: u16 = 0;
    let mut elements = Vec::with_capacity(rows.len());
    for row in rows {
        total_height = total_height.saturating_add(row.height());
        elements.push(build_row(row, theme, light));
    }
    RenderedMarkdown {
        elements,
        total_height,
    }
}
```

- [ ] **Step 3: 更新 `render_blocks` 公开函数签名**

Line 388：

```rust
pub fn render_blocks(blocks: &[ParsedBlock], light: bool) -> RenderedMarkdown {
    render_blocks_with_theme(blocks, &MarkdownTheme::default(), light)
}
```

- [ ] **Step 4: 更新测试调用点**

`mod.rs` 的 tests 模块中所有 `render_blocks_with_theme` 和 `render_blocks` 调用加 `light: false` 参数：

```rust
// Line 432
assert_eq!(render_blocks(&parsed.blocks, false).total_height, 3);

// Line 544-545
let rows_first = render_rows_with_theme(&parsed.blocks, &first);
let rows_second = render_rows_with_theme(&parsed.blocks, &second);
// (render_rows_with_theme doesn't need light - it doesn't call build_row)
```

---

### Task 3: Markdown 组件两阶段渲染改造

**Files:**
- Modify: `crates/ratatui-kit-markdown/src/markdown/mod.rs`

> **实际实现与原始计划差异**：`use_async_state` 要求 `T: Send`，但 `AnyElement` 不实现 `Send`（内部 `DropRaw` trait object 不是 Send），因此无法将 `RenderedMarkdown` 放入 `use_async_state`。改为简化方案：用 `use_previous_size` 检测首帧（width=0 → light=true），后续帧自动切换为完整高亮。效果：首帧无 syntect 阻塞（<1ms），第二帧起恢复高亮（与当前行为一致，无退化）。

- [x] **Step 0: RenderedMarkdown 不加 Clone/Debug derive**

`AnyElement` 不实现 `Clone`/`Debug`，因此 `RenderedMarkdown` 无法 derive 这些 trait。简化方案不需要 Clone。

- [x] **Step 1: 替换 Markdown 组件函数体**

完整替换 `#[component] pub fn Markdown(...)` 函数体：

```rust
#[component]
pub fn Markdown(mut hooks: Hooks, props: &MarkdownProps) -> impl Into<AnyElement<'static>> {
    // 获取上一帧的 width（第一帧为 0）
    let prev = hooks.use_previous_size();

    // markdown 解析（纯 pulldown-cmark 事件流 → ParsedBlock，<1ms）
    let parsed = hooks.use_memo(|| parse_markdown(&props.content), props.content.clone());

    let theme = hooks.use_component_theme::<MarkdownTheme>();

    // 两阶段渲染：
    //   第一帧 use_previous_size 返回 width=0 → light=true，CodeBlock 跳过高亮，<1ms 立即显示
    //   第二帧起 width 精确 → light=false，syntect 完整高亮
    let light = prev.width == 0;
    let rendered = render_blocks_with_theme(&parsed.blocks, &theme, light);

    element! {
        View(
            flex_direction: Direction::Vertical,
            height: Constraint::Length(rendered.total_height),
        ) {
            { rendered.elements.into_iter() }
        }
    }
}
```

`use_previous_size` 已通过 `ratatui_kit::prelude::*` 导入，无需额外 import。

---

### Task 4: Cargo.toml 依赖（无需调整）

> **实际**：简化方案不使用 `spawn_blocking`/`tokio`，Cargo.toml 无需修改。现有 dev-dependency `tokio` 保持不变。

---

### Task 5: 运行现有测试确保不退化

**Files:**
- Test: 全部现有测试

- [ ] **Step 1: 运行测试**

```bash
cd /path/to/ratatui-kit-contrib
cargo test -p ratatui-kit-markdown
```

预期：全部 pass（现有 13 个测试）

- [ ] **Step 2: 修复因签名变化导致的编译错误**

Task 2 修改了 `render_blocks` / `render_blocks_with_theme` / `build_row` 签名——需要确保所有调用点更新：
- `mod.rs` tests 模块（~6 处）
- `build_row` 所有 match arm 透传 `light`
- 首次编译如有遗漏调用点，逐个补充 `light` 参数

---

### Task 6: 本地验证——peri-tui 中集成测试

- [ ] **Step 1: 将增强版 ratatui-kit-markdown 指向本地路径**

在 `peri-tui/Cargo.toml` 中临时改：

```toml
# 先注释掉官方依赖
# ratatui-kit-markdown = { version = "0.2", features = ["markdown-highlight"] }

# 指向本地增强版
ratatui-kit-markdown = { path = "/path/to/ratatui-kit-contrib/crates/ratatui-kit-markdown", features = ["markdown-highlight"] }
```

同时确保 `ratatui-kit` 切到官方 0.10.1：

```toml
ratatui-kit = { version = "0.10.1", features = ["full"] }
```

- [ ] **Step 2: 编译 peri-tui**

```bash
cargo build -p peri-tui 2>&1
```

修复所有编译错误（类型签名变化、import path 变化等）。

- [ ] **Step 3: 运行时验证**

```bash
cargo run -p peri-tui
```

重点验证：
1. 首帧消息立即渲染（无闪烁/空白）
2. 代码块第二帧自动切换为高亮（颜色出现）
3. 长对话滚动流畅（total_height 精确）
4. 终端 resize 后重新计算正常

- [ ] **Step 4: 性能测量**

用含大代码块（>500 行）的 markdown 消息测试：
- 首帧：light fallback 渲染时间 <5ms（`use_memo` 命中后 <1ms）
- 第二帧：heavy 高亮渲染在 `spawn_blocking` 后 ~100-500ms 完成

---

### Task 7: 提交 PR 给 ratatui-kit-contrib

- [ ] **Step 1: Commit 增强代码**

```bash
cd /path/to/ratatui-kit-contrib
git add -A
git commit -m "feat(markdown): two-phase async rendering with light/heavy fallback

- CodeBlock: add `light` prop to skip syntect highlighting
- Markdown: use `use_previous_size` + `use_async_state` for
  width-aware rendering + async heavy highlighting
- First frame renders light (no highlight, <1ms)
- Subsequent frames upgrade to heavy (full syntect) via spawn_blocking

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>"
```

- [ ] **Step 2: Push 并创建 PR**

向 `yexiyue/ratatui-kit-contrib` 提 PR。

---

## 风险/注意事项

1. **`AnyElement` 不实现 `Send`**：`use_async_state` 要求 `T: Send`，但 `AnyElement<'static>` 内部的 `DropRaw` trait object 不是 Send。尝试用 `use_async_state` + `spawn_blocking` 会在编译期报错。**实际方案**：简化为 `use_previous_size` 首帧检测——首帧 width=0 → light=true（无高亮），第二帧起 width 精确 → light=false（完整 syntect 高亮）。首帧不阻塞 UI，后续帧 syntect 行为与当前一致（无退化）。

2. **`MarkdownTheme` 已是 `Clone + Copy`**（`#[derive(Debug, Clone, Copy, PartialEq, Eq)]`），closure 自动按 copy 捕获，无需手动 clone 字段。

3. **第一帧 width=0**：`use_previous_size` 第一帧返回 `Rect::default()`（width=0）。退到默认 80 字符宽度——对新打开的终端窗口通常准确，但对极窄终端（<80）有一帧的 light+default-width 渲染偏差。

4. **tokio 依赖**：`markdown` feature 加 `dep:tokio` 作为可选依赖。用户如果不启用 `markdown` feature 则不受影响。

5. **tests 中 `light` 默认值**：所有现有测试的 `render_blocks` / `render_blocks_with_theme` 调用改为 `light: false`（保持原行为——默认高亮）。
