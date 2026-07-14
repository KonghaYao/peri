# Markdown 行内代码颜色渲染修复

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 恢复 Markdown 行内代码（`` `code` ``）的主题颜色渲染，使其显示 `inline_code_style` 样式。

**Architecture:** `span_style.rs` 的 `span_semantic_style` 当前通过 `Modifier::DIM` 哨兵检测行内代码，但上游 `ratatui-kit-markdown 0.3.0` parser 在生成行内代码 span 时使用 `Span::raw()`，不携带任何修饰符。修复方案是增加文本特征检测作为兜底：parser 将行内代码包装为 `` `code` `` 格式，可据此识别。

**Tech Stack:** Rust / ratatui / ratatui-kit-markdown 0.3.0

---

### Task 1: 修复行内代码样式检测 + 更新测试

**Files:**
- Modify: `peri-tui/src/kit/markdown/span_style.rs:33-44`
- Modify: `peri-tui/src/kit/markdown/mod.rs:217-236`

- [ ] **Step 1: 删除 `Modifier::DIM` 哨兵分支，增加文本特征检测**

`span_semantic_style` 中 `Modifier::DIM` 分支永远不会命中（parser 不设此修饰符）。替换为文本检测：行内代码 span 内容以 `` ` `` 开头且以 `` ` `` 结尾。

```rust
/// 判断 span 语义类型（行内代码/链接/URL）并返回对应样式。
fn span_semantic_style(span: &Span<'static>, theme: &MarkdownTheme) -> Option<Style> {
    if span.style.add_modifier.contains(Modifier::REVERSED) {
        // LINK_URL_MARKER 哨兵 → link_url_style
        Some(theme.link_url_style)
    } else if span.style.add_modifier.contains(Modifier::UNDERLINED) {
        Some(theme.link_style)
    } else {
        // ratatui-kit-markdown 0.3.0 parser 对行内代码使用 Span::raw() 不带修饰符，
        // 但内容被包装为 `code` 格式。用 backtick 包裹特征做文本级检测。
        let text = span.content.as_ref();
        if text.len() >= 2 && text.starts_with('`') && text.ends_with('`') {
            Some(theme.inline_code_style)
        } else {
            None
        }
    }
}
```

同时更新 `apply_span_styles` 中的注释——`DIM` 哨兵不再使用，但保留 `remove(Modifier::DIM)` 作为无害的未来兼容：

```rust
            // 剥离 parser 内部哨兵修饰符：
            // - REVERSED（LINK_URL_MARKER）— 否则终端会反转前景/背景色
            carried_style.add_modifier.remove(Modifier::REVERSED);
            carried_style.add_modifier.remove(Modifier::DIM);
```

（`remove(Modifier::DIM)` 保留——parser 当前不设 DIM，此调用为 no-op，不影响正确性）

- [ ] **Step 2: 测试模块添加 `Color` 导入**

在 `#[cfg(test)] mod tests` 的 import 区，`use ratatui::style::Modifier;` 行后追加：

```rust
    use ratatui::style::Modifier;
    use ratatui::style::Color;
```

- [ ] **Step 3: 更新 `test_inline_code` 测试**

测试当前断言行内代码**不应**有颜色。修复后应断言**应有** `inline_code_style` 颜色。`Palette::default()` 中 `info=Blue`、`surface=Reset`，因此预期 `fg=Some(Blue), bg=Some(Reset)`。

```rust
    #[test]
    fn test_inline_code() {
        let result = flatten(&parse_markdown("use `code` here", 80, Palette::default()));
        let line = &result[0];
        let code_span = line
            .spans
            .iter()
            .find(|s| s.content.as_ref().contains("code"))
            .expect("inline code span should contain 'code'");
        // 修复后：行内代码应有 inline_code_style 颜色
        // Palette::default().info = Blue
        assert_eq!(
            code_span.style.fg,
            Some(Color::Blue),
            "inline code should have fg = palette.info (Blue)"
        );
        // Palette::default().surface = Reset
        assert_eq!(
            code_span.style.bg,
            Some(Color::Reset),
            "inline code should have bg = palette.surface (Reset)"
        );
    }
```

- [ ] **Step 4: 运行测试确认通过**

```bash
cargo test -p peri-tui --lib -- markdown::tests::test_inline_code
```

预期：PASS（行内代码有 `fg=Blue, bg=Reset`）

- [ ] **Step 5: 运行全部 markdown 测试确认无回归**

```bash
cargo test -p peri-tui --lib -- markdown
```

预期：全部 PASS（含增量缓存测试 `test_cached_*`，cache 行为不受影响因为 `ConvertState` 不涉及 span 样式）。

- [ ] **Step 6: Commit**

```bash
git add peri-tui/src/kit/markdown/span_style.rs peri-tui/src/kit/markdown/mod.rs
git commit -m "fix(markdown): 恢复行内代码 inline_code_style 颜色渲染

ratatui-kit-markdown 0.3.0 parser 用 Span::raw() 生成行内代码 span，
不携带 Modifier::DIM 哨兵，导致 span_semantic_style 的 DIM 分支永不命中。
改用文本特征检测：parser 将内容包装为 \`code\` 格式，据此匹配并应用
theme.inline_code_style。

Co-Authored-By: deepseek-v4-pro <deepseek-ai@claude-code-best.win>"
```
