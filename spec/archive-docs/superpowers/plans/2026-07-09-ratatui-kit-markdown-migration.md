# ratatui-kit-markdown 迁移实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 peri-tui 从自研 markdown 渲染管道（~2700 行 render_bridge + peri-widgets/markdown）迁移到官方 `ratatui-kit 0.10.1` + `ratatui-kit-markdown 0.2.0`（含 Track A 增强），净减 ~2700 行。

**Architecture:** 删除 RENDER_CACHE atom + render_bridge 异步 task + wrap_map + peri-widgets/markdown。VIEW_MODELS 成为消息流唯一数据源。MessageArea 重写为 ratatui-kit #[component] 组件树，每个 ViewModel 变体一个组件。AssistantBubble 使用增强版 `Markdown` 组件（两阶段 light/heavy 渲染）。text_selection.rs 保留代码但功能失效。

**Tech Stack:** ratatui-kit 0.10.1, ratatui-kit-markdown 0.2.0 (本地增强版), pulldown-cmark 0.12

**前置依赖:** Track A（ratatui-kit-markdown 增强）已完成，位于 `/Users/konghayao/code/ai/ratatui-kit-contrib`。

---

## File Structure

### 删除（19 文件）

| 文件 | 行数 | 说明 |
|------|------|------|
| `peri-tui/src/kit/render_bridge.rs` | ~432 | 异步预计算 task + RenderCache + WrappedLineInfo |
| `peri-tui/src/kit/markdown/mod.rs` | ~22 | 薄 facade（→ peri_widgets::markdown） |
| `peri-widgets/src/markdown/mod.rs` | ~157 | MarkdownTheme + parse_markdown 入口 |
| `peri-widgets/src/markdown/cache.rs` | ~109 | MarkdownCache（LRU 256 + content_hash 缓存） |
| `peri-widgets/src/markdown/highlight.rs` | ~38 | syntect 代码高亮 |
| `peri-widgets/src/markdown/cache_test.rs` | ~53 | 缓存单元测试 |
| `peri-widgets/src/markdown/highlight_test.rs` | ~35 | 高亮测试 |
| `peri-widgets/src/markdown/mod_test.rs` | ~653 | markdown 解析测试 |
| `peri-widgets/src/markdown/render_state.rs` | ~ | facade → coordinator |
| `peri-widgets/src/markdown/render_state/coordinator.rs` | ~692 | RenderState 协调器 |
| `peri-widgets/src/markdown/render_state/table/` | ~ | 表格构建器 |

### 修改（12 文件）

| 文件 | 操作 | 说明 |
|------|------|------|
| `peri-tui/Cargo.toml` | Modify | ratatui-kit 切官方 0.10.1，新增 ratatui-kit-markdown path dep |
| `peri-widgets/Cargo.toml` | Modify | 移除 markdown/markdown-highlight features + pulldown-cmark/syntect deps |
| `peri-widgets/src/lib.rs` | Modify | 移除 `pub mod markdown` + re-exports |
| `peri-tui/src/kit/mod.rs` | Modify | 移除 render_bridge/markdown 模块声明，新增 bubbles/ 声明 |
| `peri-tui/src/kit/atoms.rs` | Modify | 删除 RENDER_CACHE atom + RESIZE_TX channel |
| `peri-tui/src/kit/entry.rs` | Modify | 删除 render_bridge spawn + RESIZE_TX channel + mini bridge task |
| `peri-tui/src/kit/acp_notifier.rs` | Modify | 移除 render_bridge_tx 参数及所有 send 调用点 |
| `peri-tui/src/kit/submit_consumer.rs` | Modify | 移除 RENDER_CACHE 重置 |
| `peri-tui/src/kit/app_shell.rs` | Modify | 包裹 PaletteProvider |
| `peri-tui/src/kit/message_area.rs` | Rewrite | 重写为 #[component] 组件树 |
| `peri-tui/src/kit/view_render.rs` | Rewrite | 拆分纯函数到 bubbles/ 各组件 |
| `peri-tui/src/kit/text_selection.rs` | Modify | 加 `#[allow(dead_code)]` |

### 新增（9 文件）

| 文件 | 说明 |
|------|------|
| `peri-tui/src/kit/bubbles/mod.rs` | 组件注册 + Re-export |
| `peri-tui/src/kit/bubbles/user_bubble.rs` | 纯 Span 拼接 |
| `peri-tui/src/kit/bubbles/assistant_bubble.rs` | Markdown(content) + ReasoningBlock |
| `peri-tui/src/kit/bubbles/tool_card.rs` | format_tool_name + 折叠/展开 |
| `peri-tui/src/kit/bubbles/system_note.rs` | Info/Warning/Error 三级 |
| `peri-tui/src/kit/bubbles/subagent_group.rs` | 子 Agent 消息递归 |
| `peri-tui/src/kit/bubbles/collapsed_group.rs` | 折叠/展开组 |
| `peri-tui/src/kit/bubbles/reasoning_block.rs` | "Thought for N chars" |
| `peri-tui/src/kit/theme/markdown_palette.rs` | PaletteProvider 色值映射 |

### 保留但失效

| 文件 | 说明 |
|------|------|
| `peri-tui/src/kit/text_selection.rs` | 加 `#[allow(dead_code)]`，功能失效（RENDER_CACHE 已删除） |

---

### Task 1: Cargo.toml 依赖切换

**Files:**
- Modify: `peri-tui/Cargo.toml`
- Modify: `peri-widgets/Cargo.toml`

- [ ] **Step 1: peri-tui/Cargo.toml — 切 ratatui-kit 到官方 0.10.1 + 新增 ratatui-kit-markdown**

当前 `peri-tui/Cargo.toml` 的 `ratatui-kit` 指向 fork：
```toml
ratatui-kit = { git = "https://github.com/KonghaYao/ratatui-kit", branch = "peri/deps", features = ["full"] }
```

替换为官方版本 + markdown 依赖：
```toml
ratatui-kit = { version = "0.10.1", features = ["full"] }
ratatui-kit-markdown = { path = "/Users/konghayao/code/ai/ratatui-kit-contrib/crates/ratatui-kit-markdown", features = ["markdown-highlight"] }
```

- [ ] **Step 2: peri-widgets/Cargo.toml — 移除 markdown 相关依赖**

移除 `[dependencies]` 中的：
```toml
pulldown-cmark = { version = "0.13", optional = true }
syntect = { version = "5", optional = true, default-features = false, features = ["parsing", "default-syntaxes", "default-themes", "regex-fancy"] }
```

移除 `[features]` 中的：
```toml
markdown = ["dep:pulldown-cmark"]
markdown-highlight = ["markdown", "dep:syntect"]
```

确认 `unicode-width = "0.2"` 由其他模块使用，保留。

- [ ] **Step 3: 构建验证**

```bash
cd /Users/konghayao/code/ai/perihelion && cargo build -p peri-widgets -p peri-tui 2>&1
```

预期：编译失败（peri-widgets 的 markdown 模块已被引用但即将删除）。这是预期行为——逐 task 修复。

- [ ] **Step 4: Commit**

```bash
cd /Users/konghayao/code/ai/perihelion
git add peri-tui/Cargo.toml peri-widgets/Cargo.toml
git commit -m "feat: switch ratatui-kit to 0.10.1, add ratatui-kit-markdown path dep, remove markdown deps from peri-widgets"
```

---

### Task 2: 删除 peri-widgets/markdown 模块

**Files:**
- Delete: `peri-widgets/src/markdown/` (整个目录)
- Modify: `peri-widgets/src/lib.rs`

- [ ] **Step 1: 删除 markdown 目录**

```bash
cd /Users/konghayao/code/ai/perihelion && rm -rf peri-widgets/src/markdown/
```

- [ ] **Step 2: 从 lib.rs 移除 markdown 模块声明及相关 re-export**

移除以下行（`peri-widgets/src/lib.rs`）：

1. 移除 `#[cfg(feature = "markdown")] pub mod markdown;`
2. 移除 re-export 中的 `MarkdownTheme`, `ThemeMarkdownAdapter`

查找确切的 re-export 行：
```rust
// 移除类似以下的行：
pub use markdown::{MarkdownTheme, ThemeMarkdownAdapter};
```

- [ ] **Step 3: Commit**

```bash
cd /Users/konghayao/code/ai/perihelion
git add peri-widgets/src/
git commit -m "feat: delete peri-widgets/markdown module (~2000 lines)"
```

---

### Task 3: 删除 render_bridge + kit/markdown

**Files:**
- Delete: `peri-tui/src/kit/render_bridge.rs`
- Delete: `peri-tui/src/kit/markdown/` (整个目录)
- Modify: `peri-tui/src/kit/mod.rs`

- [ ] **Step 1: 删除 render_bridge.rs**

```bash
rm /Users/konghayao/code/ai/perihelion/peri-tui/src/kit/render_bridge.rs
```

- [ ] **Step 2: 删除 kit/markdown 目录**

```bash
rm -rf /Users/konghayao/code/ai/perihelion/peri-tui/src/kit/markdown/
```

- [ ] **Step 3: 修改 mod.rs — 移除模块声明 + 新增 bubbles/ 声明**

在 `peri-tui/src/kit/mod.rs` 中：
1. 移除 `pub mod render_bridge;` 和 `pub mod markdown;` 行
2. 移除 render_bridge 类型的重导出（VmKey, RenderedEntry, RenderCache, WrappedLineInfo 等——这些通过 `use crate::kit::render_bridge::*` 引入的）
3. 新增 `pub mod bubbles;`

- [ ] **Step 4: Commit**

```bash
cd /Users/konghayao/code/ai/perihelion
git add peri-tui/src/kit/
git commit -m "feat: delete render_bridge.rs and kit/markdown (~460 lines)"
```

---

### Task 4: 清理 atoms.rs — 删除 RENDER_CACHE + RESIZE_TX

**Files:**
- Modify: `peri-tui/src/kit/atoms.rs:185,193`

- [ ] **Step 1: 删除 RENDER_CACHE atom**

删除 `peri-tui/src/kit/atoms.rs` 第 185 行：
```rust
pub static RENDER_CACHE: AtomStatic<RenderCache> = AtomStatic::new(|| RenderCache::default());
```

同时删除文件顶部的 `use crate::kit::render_bridge::RenderCache;` import。

- [ ] **Step 2: 删除 RESIZE_TX channel**

删除 `peri-tui/src/kit/atoms.rs` 第 193 行：
```rust
pub static RESIZE_TX: OnceLock<UnboundedSender<u16>> = OnceLock::new();
```

- [ ] **Step 3: 删除 RENDER_CACHE 相关的注释**

文件顶部注释中提及 RENDER_CACHE / RESIZE_TX 的部分改为当前状态描述。

- [ ] **Step 4: Commit**

```bash
cd /Users/konghayao/code/ai/perihelion
git add peri-tui/src/kit/atoms.rs
git commit -m "feat: remove RENDER_CACHE atom and RESIZE_TX channel"
```

---

### Task 5: 创建 markdown_palette.rs（PaletteProvider 色值映射）

**Files:**
- Create: `peri-tui/src/kit/theme/markdown_palette.rs`

- [ ] **Step 1: 创建 palette 文件和函数**

```rust
//! Markdown 主题色值 → ratatui-kit Palette 映射。
//!
//! 将 peri-tui 原有的 9 种 hardcoded 色值映射到 ratatui-kit PaletteProvider，
//! Markdown / CodeBlock 组件通过 use_component_theme 自动派生色值。

use ratatui_kit::prelude::{Palette, PaletteColor};

/// 构建 peri-tui 专用的 markdown 色板。
///
/// 映射关系（对应原 DefaultMarkdownTheme）：
///
/// | 色值（#hex）     | 当前用途          | Palette 槽位   |
/// |------------------|-------------------|----------------|
/// | #FFC107          | heading           | Palette::text  |
/// | #FFFFFF          | text / list_bullet| Palette::text  |
/// | #999999          | muted / quote / sep| Palette::muted |
/// | #A2A9E4          | code              | Palette::info  |
/// | #4EBA65          | link / code_prefix| Palette::success|
pub fn peri_markdown_palette() -> Palette {
    Palette {
        text: PaletteColor::hex("#FFFFFF"),
        muted: PaletteColor::hex("#999999"),
        success: PaletteColor::hex("#4EBA65"),
        warning: PaletteColor::hex("#FFC107"),
        info: PaletteColor::hex("#A2A9E4"),
        ..Default::default()
    }
}
```

- [ ] **Step 2: 确认 theme 模块已存在 `mod.rs`**

```bash
ls /Users/konghayao/code/ai/perihelion/peri-tui/src/kit/theme/
```

如果 `theme/` 是 `theme.rs` 而非 `theme/` 目录，则改为创建 `theme/markdown_palette.rs` 并确保有 `pub mod markdown_palette;`。

- [ ] **Step 3: Commit**

```bash
cd /Users/konghayao/code/ai/perihelion
git add peri-tui/src/kit/theme/
git commit -m "feat: add markdown PaletteProvider color mapping"
```

---

### Task 6: 创建 bubbles/ 目录 + mod.rs

**Files:**
- Create: `peri-tui/src/kit/bubbles/mod.rs`
- Create: 占位文件（后续 task 填充）

- [ ] **Step 1: 创建 bubbles/mod.rs**

```rust
//! ViewModel 变体组件。
//!
//! 每个 ViewModel 变体一个 #[component]，由 MessageArea 父组件 match 分发。
//! ViewModel 类型定义见 `peri-tui/src/kit/tui_render_unit.rs`。

pub mod user_bubble;
pub mod assistant_bubble;
pub mod tool_card;
pub mod system_note;
pub mod subagent_group;
pub mod collapsed_group;
pub mod reasoning_block;
```

- [ ] **Step 2: 创建空占位文件**

为每个模块创建最小框架文件，包含 `use ratatui_kit::prelude::*;` 和一个 panic 占位函数（后续 task 逐步填充）：

`peri-tui/src/kit/bubbles/user_bubble.rs`:
```rust
use ratatui_kit::prelude::*;
// TODO: Task 7
```

其余 6 个文件类似创建（assistant_bubble.rs, tool_card.rs, system_note.rs, subagent_group.rs, collapsed_group.rs, reasoning_block.rs）。

- [ ] **Step 3: Commit**

```bash
cd /Users/konghayao/code/ai/perihelion
git add peri-tui/src/kit/bubbles/
git commit -m "feat: scaffold bubbles/ directory with module stubs"
```

---

### Task 7: UserBubble 组件

**Files:**
- Write: `peri-tui/src/kit/bubbles/user_bubble.rs`

UserBubble 不解析 markdown，用纯 Span 拼接 `❯ ` 前缀 + 用户文本。

- [ ] **Step 1: 实现 UserBubble 组件**

```rust
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::{
    layout::{Constraint, Direction},
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::kit::theme;

/// 用户消息气泡——纯 Span 拼接，不解析 markdown。
#[with_layout_style]
#[derive(Props, Default)]
pub struct UserBubbleProps<'a> {
    pub content: Arc<str>,
    pub children: Vec<AnyElement<'a>>,
}

pub struct UserBubble;

impl Component for UserBubble {
    type Props<'a> = UserBubbleProps<'a>;

    fn new(_props: &Self::Props<'_>) -> Self {
        Self
    }

    fn update(
        &mut self,
        props: &mut Self::Props<'_>,
        _hooks: Hooks,
        updater: &mut ComponentUpdater,
    ) {
        updater.set_layout_style(props.layout_style());
    }

    fn draw(&mut self, drawer: &mut ComponentDrawer<'_, '_>) {
        let area = drawer.area;
        // 前缀 ❯ + 用户文本（单行，超出截断）
        let prefix = Span::styled("❯ ", Style::default().fg(theme::COLOR_USER_PREFIX));
        let text = Span::raw(props.content.as_ref());
        let line = Line::from(vec![prefix, text]);
        let paragraph = Paragraph::new(line);
        paragraph.render(area, drawer.buffer_mut());
    }
}
```

> **注意**：需确认 `theme::COLOR_USER_PREFIX` 的存在性。如果不存在，在 `theme.rs` 中新增：
> ```rust
> pub const COLOR_USER_PREFIX: Color = Color::Rgb(103, 152, 255); // #6798FF
> ```

- [ ] **Step 2: 从 view_render.rs 提取 `render_user_bubble` 逻辑作为参考**

查看 `view_render.rs:116-154` 的 `render_user_bubble` 函数，确认前缀颜色和样式。但不要删除 view_render.rs 中的函数——后续 task 统一清理。

- [ ] **Step 3: Commit**

```bash
git add peri-tui/src/kit/bubbles/user_bubble.rs
git commit -m "feat: add UserBubble component (pure Span, no markdown)"
```

---

### Task 8: AssistantBubble 组件

**Files:**
- Write: `peri-tui/src/kit/bubbles/assistant_bubble.rs`

AssistantBubble 是唯一使用 `Markdown` 官方组件的地方。需包含 reasoning block（如果存在）。

- [ ] **Step 1: 实现 AssistantBubble 组件**

```rust
use std::sync::Arc;

use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::layout::{Constraint, Direction};
use ratatui_kit_markdown::Markdown;

use crate::kit::bubbles::reasoning_block::ReasoningBlock;

/// AI 助手消息气泡——使用官方 Markdown 组件渲染。
#[with_layout_style]
#[derive(Props, Default)]
pub struct AssistantBubbleProps<'a> {
    pub content: Arc<str>,
    /// 可选的推理块（"Thought for N chars"）。
    pub reasoning: Option<Arc<str>>,
    pub children: Vec<AnyElement<'a>>,
}

#[component]
pub fn AssistantBubble(props: &AssistantBubbleProps) -> impl Into<AnyElement<'static>> {
    let has_reasoning = props.reasoning.is_some();

    element! {
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
        ) {
            if has_reasoning {
                ReasoningBlock(text: props.reasoning.clone().unwrap_or_default())
            }
            Markdown(content: props.content.as_ref().to_string())
        }
    }
}
```

`ReasoningBlock` 组件见 Task 12。

- [ ] **Step 2: Commit**

```bash
git add peri-tui/src/kit/bubbles/assistant_bubble.rs
git commit -m "feat: add AssistantBubble component (uses official Markdown)"
```

---

### Task 9: ToolCard 组件

**Files:**
- Write: `peri-tui/src/kit/bubbles/tool_card.rs`

从 `view_render.rs:235-355` 提取 `render_tool_card` 逻辑。

- [ ] **Step 1: 实现 ToolCard 组件**

```rust
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::{
    layout::Constraint,
    style::{Color, Style},
    text::{Line, Span},
};

/// 工具卡片组件——显示工具名、参数摘要和输出。
#[with_layout_style]
#[derive(Props, Default)]
pub struct ToolCardProps<'a> {
    /// 工具名称（已格式化）。
    pub tool_name: String,
    /// 工具参数摘要。
    pub args_summary: Option<String>,
    /// 工具输出行列表。
    pub output_lines: Vec<String>,
    /// 是否默认展开。
    pub expanded: Option<bool>,
    pub children: Vec<AnyElement<'a>>,
}

pub struct ToolCard {
    tool_name: String,
    args_summary: Option<String>,
    output_lines: Vec<String>,
    expanded: bool,
}

impl Component for ToolCard {
    type Props<'a> = ToolCardProps<'a>;

    fn new(props: &Self::Props<'_>) -> Self {
        Self {
            tool_name: props.tool_name.clone(),
            args_summary: props.args_summary.clone(),
            output_lines: props.output_lines.clone(),
            expanded: props.expanded.unwrap_or(false),
        }
    }

    fn update(
        &mut self,
        props: &mut Self::Props<'_>,
        _hooks: Hooks,
        updater: &mut ComponentUpdater,
    ) {
        self.tool_name = props.tool_name.clone();
        self.args_summary = props.args_summary.clone();
        self.output_lines = props.output_lines.clone();
        self.expanded = props.expanded.unwrap_or(false);
        updater.set_layout_style(props.layout_style());
    }

    fn draw(&mut self, drawer: &mut ComponentDrawer<'_, '_>) {
        let area = drawer.area;

        // Header 行：⏳/✓ 图标 + 工具名 [+ 参数摘要]
        let header = {
            let icon = if self.output_lines.is_empty() {
                Span::styled("⏳ ", Style::default().fg(Color::Yellow))
            } else {
                Span::styled("✓ ", Style::default().fg(Color::Green))
            };
            let name = Span::styled(
                &self.tool_name,
                Style::default().fg(Color::Rgb(162, 169, 228)),
            );
            let mut spans = vec![icon, name];
            if let Some(ref args) = self.args_summary {
                if !args.is_empty() {
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(args, Style::default().fg(Color::Gray)));
                }
            }
            Line::from(spans)
        };

        // 输出行（折叠时只显示最后一行的摘要）
        // ... 完整的 draw 逻辑
    }
}
```

> **注意**：上述代码框架简化了完整的折叠/展开 UI 逻辑。实际实现需参考 `view_render.rs:235-355` 的完整折叠逻辑（COLLAPSED_BY_DEFAULT / AUTO_EXPAND / FORCE_EXPAND_ON_COMPLETE 三态），以及 `compact_output_lines` 的截断逻辑。

- [ ] **Step 2: Commit**

```bash
git add peri-tui/src/kit/bubbles/tool_card.rs
git commit -m "feat: add ToolCard component (fold/expand, tool output)"
```

---

### Task 10: SystemNote 组件

**Files:**
- Write: `peri-tui/src/kit/bubbles/system_note.rs`

从 `view_render.rs:469-536` 提取 `render_system_note` 逻辑。

- [ ] **Step 1: 实现 SystemNote 组件**

```rust
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::{
    layout::Constraint,
    style::{Color, Style},
    text::{Line, Span},
};

/// 系统注释级别。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum NoteLevel {
    Info,
    Warning,
    Error,
}

/// 系统注释组件——Info/Warning/Error 三级显示。
#[with_layout_style]
#[derive(Props, Default)]
pub struct SystemNoteProps<'a> {
    pub text: String,
    pub level: NoteLevel,
    pub children: Vec<AnyElement<'a>>,
}

impl Component for SystemNote {
    type Props<'a> = SystemNoteProps<'a>;

    fn new(props: &Self::Props<'_>) -> Self { /* ... */ }
    fn update(&mut self, props: &mut Self::Props<'_>, _hooks: Hooks, updater: &mut ComponentUpdater) { /* ... */ }
    fn draw(&mut self, drawer: &mut ComponentDrawer<'_, '_>) {
        let area = drawer.area;
        let prefix = match self.level {
            NoteLevel::Info => Span::styled("✻ ", Style::default().fg(Color::Rgb(162, 169, 228))),
            NoteLevel::Warning => Span::styled("⚠ ", Style::default().fg(Color::Rgb(255, 193, 7))),
            NoteLevel::Error => Span::styled("✗ ", Style::default().fg(Color::Red)),
        };
        let text = Span::styled(&self.text, Style::default().fg(Color::Rgb(153, 153, 153)));
        let line = Line::from(vec![prefix, text]);
        // render to drawer.buffer_mut()
    }
}
```

- [ ] **Step 2: Commit**

```bash
git add peri-tui/src/kit/bubbles/system_note.rs
git commit -m "feat: add SystemNote component (Info/Warning/Error levels)"
```

---

### Task 11: SubAgentGroup 组件

**Files:**
- Write: `peri-tui/src/kit/bubbles/subagent_group.rs`

从 `view_render.rs:569-683` 提取 `render_subagent_group` 逻辑。SubAgentGroup 内部递归渲染各变体 ViewModel，需持有 ViewModel 列表。

- [ ] **Step 1: 实现 SubAgentGroup 组件**

SubAgentGroup 是最复杂的变体——需递归渲染子 ViewModel。实现方式：

```rust
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::layout::{Constraint, Direction};
use crate::kit::tui_render_unit::TuiRenderUnit;

#[with_layout_style]
#[derive(Props, Default)]
pub struct SubAgentGroupProps<'a> {
    /// SubAgent 名称。
    pub agent_name: String,
    /// 是否正在运行。
    pub is_running: bool,
    /// 是否以 error 结束。
    pub is_error: bool,
    /// 子 ViewModel 列表。
    pub view_models: Vec<TuiRenderUnit>,
    /// 最终结果文本（如果完成）。
    pub final_result: Option<String>,
    pub children: Vec<AnyElement<'a>>,
}

#[component]
pub fn SubAgentGroup(props: &SubAgentGroupProps) -> impl Into<AnyElement<'static>> {
    // Header: ◆/❯ agent_name [running/error icon]
    // 折叠/展开按钮
    // 展开时遍历 view_models，递归调用对应变体组件
    // 末尾 final_result 行（⎿ prefix）
    element! {
        View(flex_direction: Direction::Vertical) {
            /* header + body */
        }
    }
}
```

> **注意**：SubAgentGroup 内部对 child ViewModel 的渲染需要调用变体分发逻辑。可通过在 bubbles/mod.rs 中暴露一个 `fn render_view_model(vm: &TuiRenderUnit) -> AnyElement<'static>` 函数实现。

- [ ] **Step 2: Commit**

```bash
git add peri-tui/src/kit/bubbles/subagent_group.rs
git commit -m "feat: add SubAgentGroup component (recursive VM rendering)"
```

---

### Task 12: ReasoningBlock + CollapsedGroup 组件

**Files:**
- Write: `peri-tui/src/kit/bubbles/reasoning_block.rs`
- Write: `peri-tui/src/kit/bubbles/collapsed_group.rs`

- [ ] **Step 1: ReasoningBlock 组件**

从 `view_render.rs:199-226` 提取 `render_reasoning_block`：

```rust
use std::sync::Arc;
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::{
    layout::Constraint,
    style::{Color, Style},
    text::{Line, Span},
};

#[with_layout_style]
#[derive(Props, Default)]
pub struct ReasoningBlockProps<'a> {
    pub text: Arc<str>,
    /// 是否展开。默认 false（折叠状态）。
    pub expanded: Option<bool>,
    pub children: Vec<AnyElement<'a>>,
}

pub struct ReasoningBlock {
    text: Arc<str>,
    expanded: bool,
}

impl Component for ReasoningBlock {
    type Props<'a> = ReasoningBlockProps<'a>;

    fn new(props: &Self::Props<'_>) -> Self {
        Self {
            text: props.text.clone(),
            expanded: props.expanded.unwrap_or(false),
        }
    }

    fn update(&mut self, props: &mut Self::Props<'_>, _hooks: Hooks, updater: &mut ComponentUpdater) {
        self.text = props.text.clone();
        self.expanded = props.expanded.unwrap_or(false);
        updater.set_layout_style(props.layout_style());
    }

    fn draw(&mut self, drawer: &mut ComponentDrawer<'_, '_>) {
        let area = drawer.area;
        let char_count = self.text.chars().count();
        let header = if self.expanded {
            format!("▼ Thought for {} chars", char_count)
        } else {
            // 折叠状态：显示 "▶ Thought for N chars" + 最后 3 行预览
            let preview = self.text.lines().rev().take(3).collect::<Vec<_>>();
            // ...
            format!("▶ Thought for {} chars", char_count)
        };
        let line = Line::styled(header, Style::default().fg(Color::Rgb(153, 153, 153)));
        // render to drawer
    }
}
```

- [ ] **Step 2: CollapsedGroup 组件**

从 `view_render.rs:685-694` 提取：

```rust
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::{
    layout::Constraint,
    style::{Color, Style},
    text::{Line, Span},
};

#[with_layout_style]
#[derive(Props, Default)]
pub struct CollapsedGroupProps<'a> {
    /// 折叠项的摘要文本。
    pub summary: String,
    /// 是否展开。
    pub expanded: Option<bool>,
    /// 折叠的条目数量。
    pub count: usize,
    pub children: Vec<AnyElement<'a>>,
}
// ... Component impl
```

- [ ] **Step 3: Commit**

```bash
git add peri-tui/src/kit/bubbles/reasoning_block.rs peri-tui/src/kit/bubbles/collapsed_group.rs
git commit -m "feat: add ReasoningBlock and CollapsedGroup components"
```

---

### Task 13: MessageArea 重写为组件树

**Files:**
- Rewrite: `peri-tui/src/kit/message_area.rs`

这是最关键的任务。MessageArea 从直接消费 `RENDER_CACHE` atom 改为遍历 `VIEW_MODELS` atom 的 committed + current_turn，match 变体分发到对应气泡组件。

- [ ] **Step 1: 理解当前 MessageArea 的结构**

当前 `message_area.rs` (~1182 行) 的功能：
1. 订阅 `RENDER_CACHE` + `ACP_STATE` + `TODO_ITEMS` + `LANG_VERSION`
2. `LineCache` 缓存 wrap_map + highlighted_lines
3. `viewport_clip` 二分查找视口裁剪
4. `build_footer_lines` 构建 spinner + todo + 状态行
5. ScrollView 包裹 Paragraph
6. 鼠标 Drag 文本选区
7. 智能跟随：loading 吸底，用户上滚不抢夺

新 MessageArea 需要保留的功能：
- ScrollView 视口裁剪（ratatui-kit 原生）
- Todo 渲染
- 智能跟随
- Sticky Header + 滚动按钮

新增功能：
- 遍历 VIEW_MODELS → match variant → 分发到子组件

- [ ] **Step 2: 实现新 MessageArea 组件**

```rust
use ratatui_kit::prelude::*;
use ratatui_kit::ratatui::layout::{Constraint, Direction};
use crate::kit::atoms::{VIEW_MODELS, ACP_STATE, TODO_ITEMS, LANG_VERSION};
use crate::kit::bubbles::*;
use crate::kit::tui_render_unit::{TuiRenderUnit, ViewModel};

/// 消息区——遍历 VIEW_MODELS 的 committed + current_turn，
/// match ViewModel 变体分发到对应 #[component]。
#[component]
pub fn MessageArea(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let vm_snapshot = hooks.use_atom(&VIEW_MODELS);
    let acp_state = hooks.use_atom(&ACP_STATE);
    let todo_items = hooks.use_atom(&TODO_ITEMS);

    let snapshot = vm_snapshot.read();
    let is_loading = acp_state.read().is_loading;
    let todos = todo_items.read().clone();

    // 构建所有 ViewModel 的行高近似值：在非 scrolling 帧才精确计算。
    // 暂时每个变体按内容行数估算高度。
    let all_items: Vec<&TuiRenderUnit> = snapshot
        .committed
        .iter()
        .chain(snapshot.current_turn.iter())
        .collect();

    // 渲染每个 ViewModel
    let rendered_items: Vec<AnyElement<'static>> = all_items
        .iter()
        .map(|vm| render_view_model(vm))
        .collect();

    // 估算总高度
    let total_height: u16 = snapshot.committed.len() as u16
        + snapshot.current_turn.len() as u16;

    // footer: spinner + todo
    let footer = build_message_footer(is_loading, &todos);

    element! {
        ScrollView(
            height: Constraint::Fill(1),
        ) {
            View(
                flex_direction: Direction::Vertical,
                height: Constraint::Length(total_height),
            ) {
                { rendered_items.into_iter() }
                { footer }
            }
        }
    }
}

/// 将单个 ViewModel 渲染为对应组件。
fn render_view_model(vm: &TuiRenderUnit) -> AnyElement<'static> {
    match &vm.variant {
        ViewModelVariant::UserBubble { content, reminder } => {
            element! {
                UserBubble(content: content.clone())
            }.into_any()
        }
        ViewModelVariant::AssistantBubble { content, reasoning } => {
            let reasoning_arc = reasoning.clone().map(Arc::from);
            element! {
                AssistantBubble(
                    content: content.clone(),
                    reasoning: reasoning_arc,
                )
            }.into_any()
        }
        ViewModelVariant::ToolCard { data } => {
            // format_tool_name / format_tool_args -> ToolCard props
            element! { ToolCard(/* ... */) }.into_any()
        }
        ViewModelVariant::SystemNote { data } => {
            element! { SystemNote(/* ... */) }.into_any()
        }
        ViewModelVariant::SubAgentGroup { data } => {
            element! { SubAgentGroup(/* ... */) }.into_any()
        }
        ViewModelVariant::CollapsedGroup { data } => {
            element! { CollapsedGroup(/* ... */) }.into_any()
        }
        ViewModelVariant::DividerData => {
            AnyElement::from(View::default())  // 占位
        }
        ViewModelVariant::AskUserBlock { .. } => {
            AnyElement::from(View::default())  // 占位
        }
    }
}

/// 构建底部状态行（spinner + todo + 总结）。
fn build_message_footer(
    is_loading: bool,
    todo_items: &[TodoItem],
) -> impl Into<AnyElement<'static>> {
    // ... footer 渲染逻辑
}
```

> **注意**：上述代码需要对齐 `TuiRenderUnit` 的实际变体定义（在 `peri-tui/src/kit/tui_render_unit.rs` 中）。需要先读取该文件确认确切的变体名和字段。

- [ ] **Step 3: 读取 tui_render_unit.rs 确认变体定义**

```bash
# 在实现前先读取变体定义
```

TuiRenderUnit 的变体名（根据 view_render.rs 中的 match arm）：
- `TuiUserBubble { content, reminder }`
- `TuiAssistantBubble { content, reasoning }`
- `TuiToolCard { data: TuiToolCard }`
- `TuiSystemNote { data: TuiSystemNote }`
- `TuiSubAgentGroup { data: TuiSubAgentGroup }`
- `TuiCollapsedGroup { data: TuiCollapsedGroup }`
- `TuiDivider`
- `TuiAskUserBlock { data: TuiAskUserBlock }`

- [ ] **Step 4: Commit**

```bash
git add peri-tui/src/kit/message_area.rs
git commit -m "feat: rewrite MessageArea as ViewModel variant component tree"
```

---

### Task 14: 清理 view_render.rs

**Files:**
- Modify: `peri-tui/src/kit/view_render.rs`

- [ ] **Step 1: 删除不再需要的函数**

删除以下函数（逻辑已迁移到 bubbles/ 组件）：
- `render_v2_vm` (已被 `render_view_model` 替代)
- `render_user_bubble` (→ UserBubble 组件)
- `render_assistant_bubble` (→ AssistantBubble 组件)
- `render_tool_card` (→ ToolCard 组件)
- `render_system_note` (→ SystemNote 组件)
- `render_subagent_group` (→ SubAgentGroup 组件)
- `render_collapsed_group` (→ CollapsedGroup 组件)
- `render_reasoning_block` (→ ReasoningBlock 组件)
- `render_ask_user_block` (保留，但清空为 TODO 占位)
- `render_divider` (→ Divider 组件，或直接内联到 MessageArea)

保留的工具函数（被多个组件引用）：
- `compact_summary` — 保留
- `compact_output_lines` — 保留
- `with_message_spacing` — 保留
- `format_tool_name` / `format_tool_args` 等 — 保留到 `tool_display.rs`

- [ ] **Step 2: Commit**

```bash
git add peri-tui/src/kit/view_render.rs
git commit -m "feat: delete migrated render functions from view_render.rs"
```

---

### Task 15: 清理 entry.rs — 删除 render_bridge spawn

**Files:**
- Modify: `peri-tui/src/kit/entry.rs:142-188`

- [ ] **Step 1: 删除 render_bridge_tx/rx channel 创建**

删除第 144 行：
```rust
let (render_bridge_tx, render_bridge_rx) = mpsc::unbounded_channel();
```

- [ ] **Step 2: 删除 LOCAL_EVENT_TX → render_bridge 的 mini bridge task**

删除第 148-158 行（LOCAL_EVENT_TX + mini bridge task 的 render_bridge 转发部分）：
```rust
// 删除 localStorage_tx 转发到 render_bridge_tx 的逻辑
// 保留 bridge_tx 转发
```

修改 mini bridge task 为只转发到 bridge_tx：
```rust
let bridge_tx_clone = bridge_tx.clone();
tokio::spawn(async move {
    while let Some(ev) = local_event_rx.recv().await {
        let _ = bridge_tx_clone.send(ev);
    }
});
```

- [ ] **Step 3: 删除 RESIZE_TX channel**

删除第 159-160 行：
```rust
let (resize_tx, resize_rx) = mpsc::unbounded_channel::<u16>();
let _ = atoms::RESIZE_TX.set(resize_tx);
```

- [ ] **Step 4: 修改 spawn_kit_notifier 调用——移除 render_bridge_tx 参数**

第 163-168 行，从：
```rust
let _notifier_handle = spawn_kit_notifier(
    notification_rx,
    bridge_tx,
    render_bridge_tx,
    shutdown.clone(),
);
```
改为：
```rust
let _notifier_handle = spawn_kit_notifier(
    notification_rx,
    bridge_tx,
    shutdown.clone(),
);
```

- [ ] **Step 5: 删除 render_bridge spawn + supervisor task**

删除第 170-188 行（`spawn_render_bridge` 调用 + supervisor task）：
```rust
let render_handle = spawn_render_bridge(render_bridge_rx, resize_rx, shutdown.clone());
tokio::spawn(async move {
    match render_handle.await {
        Ok(()) => { tracing::info!("render_bridge task exited cleanly"); }
        Err(e) => { tracing::error!(...); }
    }
});
```

- [ ] **Step 6: 删除 `use crate::kit::render_bridge;` import**

- [ ] **Step 7: Commit**

```bash
git add peri-tui/src/kit/entry.rs
git commit -m "feat: remove render_bridge spawn and RESIZE_TX from entry.rs"
```

---

### Task 16: 清理 acp_notifier.rs — 移除 render_bridge_tx

**Files:**
- Modify: `peri-tui/src/kit/acp_notifier.rs:39-44,173-277`

- [ ] **Step 1: 修改 spawn_kit_notifier 签名**

从：
```rust
pub fn spawn_kit_notifier(
    mut notification_rx: mpsc::UnboundedReceiver<AcpNotification>,
    bridge_tx: mpsc::UnboundedSender<AcpEventWithEpoch>,
    render_bridge_tx: mpsc::UnboundedSender<AcpEventWithEpoch>,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
```
改为：
```rust
pub fn spawn_kit_notifier(
    mut notification_rx: mpsc::UnboundedReceiver<AcpNotification>,
    bridge_tx: mpsc::UnboundedSender<AcpEventWithEpoch>,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
```

- [ ] **Step 2: 修改 forward_notification 签名**

从：
```rust
fn forward_notification(
    bridge_tx: &mpsc::UnboundedSender<AcpEventWithEpoch>,
    render_bridge_tx: &mpsc::UnboundedSender<AcpEventWithEpoch>,
    n: AcpNotification,
) {
```
改为：
```rust
fn forward_notification(
    bridge_tx: &mpsc::UnboundedSender<AcpEventWithEpoch>,
    n: AcpNotification,
) {
```

- [ ] **Step 3: 删除所有 render_bridge_tx.send() 调用点**

在 `forward_notification` 函数体内，每个事件处理分支都有以下模式：
```rust
if let Err(e) = render_bridge_tx.send(wrapped.clone()) { /* ... */ }
if let Err(e) = bridge_tx.send(wrapped) { /* ... */ }
```

删除 `render_bridge_tx.send()` 行，保留 `bridge_tx.send()` 行。共有 ~7 处：

1. `AcpNotification::UnstableEvent` 分支（第 198-200 行）
2. `AcpNotification::SessionUpdate` 分支（第 211-212 行）
3. `AcpNotification::AgentDone` 分支
4. `AcpNotification::TurnInterrupted` 分支
5. `handle_elicitation` 函数内（~第 548 行）
6. `AcpNotification::AgentEvent` 分支（~第 247 行）
7. `AcpNotification::PredictionReady` 分支（~第 263 行）
8. `handle_request_permission` 函数内（~第 607 行）

- [ ] **Step 4: 更新主循环调用**

第 53 行，从：
```rust
Some(notif) => forward_notification(&bridge_tx, &render_bridge_tx, notif),
```
改为：
```rust
Some(notif) => forward_notification(&bridge_tx, notif),
```

- [ ] **Step 5: 更新 handle_elicitation 和 handle_request_permission 签名**

移除它们的 `render_bridge_tx` 参数。

- [ ] **Step 6: Commit**

```bash
git add peri-tui/src/kit/acp_notifier.rs
git commit -m "feat: remove render_bridge_tx from acp_notifier"
```

---

### Task 17: 清理 submit_consumer.rs — 移除 RENDER_CACHE 重置

**Files:**
- Modify: `peri-tui/src/kit/submit_consumer.rs:110-145`

- [ ] **Step 1: 删除 RENDER_CACHE 重置**

在 `handle_clear_submit` 函数中，删除两处：
```rust
*RENDER_CACHE.state().write() = RenderCache::default();
```
（第 119 行和第 138 行各一处）

- [ ] **Step 2: 删除 RENDER_CACHE 相关 import**

删除：
```rust
use crate::kit::render_bridge::RenderCache;
use crate::kit::atoms::RENDER_CACHE;
```

- [ ] **Step 3: Commit**

```bash
git add peri-tui/src/kit/submit_consumer.rs
git commit -m "feat: remove RENDER_CACHE reset from submit_consumer"
```

---

### Task 18: 添加 PaletteProvider 到 AppShell

**Files:**
- Modify: `peri-tui/src/kit/app_shell.rs`

- [ ] **Step 1: 包裹 PaletteProvider**

```rust
use crate::kit::theme::markdown_palette::peri_markdown_palette;

#[component]
pub fn AppShell(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    // ... 现有代码 ...

    // 注入 markdown 色板
    let palette = peri_markdown_palette();

    if wizard_active {
        element! {
            PaletteProvider(palette: palette) {
                View(/* SetupWizard */)
            }
        }
    } else {
        element! {
            PaletteProvider(palette: palette) {
                View(/* SessionColumn + StatusBar + PopupOverlay */)
            }
        }
    }
}
```

- [ ] **Step 2: Commit**

```bash
git add peri-tui/src/kit/app_shell.rs
git commit -m "feat: wrap AppShell with PaletteProvider for markdown theming"
```

---

### Task 19: 禁用 text_selection.rs

**Files:**
- Modify: `peri-tui/src/kit/text_selection.rs`

- [ ] **Step 1: 加 #[allow(dead_code)]**

在 `text_selection.rs` 顶部模块级添加：
```rust
#![allow(dead_code)]
```

- [ ] **Step 2: Commit**

```bash
git add peri-tui/src/kit/text_selection.rs
git commit -m "feat: disable text_selection.rs (RENDER_CACHE removed, to be replaced)"
```

---

### Task 20: 全局编译修复 + 集成测试

**Files:**
- All modified files

- [ ] **Step 1: 全量构建**

```bash
cd /Users/konghayao/code/ai/perihelion && cargo build -p peri-tui 2>&1
```

修复所有编译错误（预期类型：import 丢失、模块引用失效、use 路径错误）。

常见错误模式：
- `use crate::kit::render_bridge::*` → 删除（render_bridge 已删除）
- `RENDER_CACHE.state()` → 替换为 `VIEW_MODELS.state()`
- `render_bridge_tx` → 删除
- `peri_widgets::markdown` → 替换为 `ratatui_kit_markdown`

- [ ] **Step 2: 搜索残留引用**

```bash
cd /Users/konghayao/code/ai/perihelion
rg "render_bridge|RENDER_CACHE|RESIZE_TX" peri-tui/src/ --type rust
rg "peri_widgets::markdown" peri-tui/src/ --type rust
```

确保所有残留引用已清理。

- [ ] **Step 3: 运行时验证**

```bash
cargo run -p peri-tui
```

验证：
1. 启动无 panic
2. 用户输入后气泡正常渲染
3. AI 回复的 Markdown 正确渲染（代码块有高亮吗？——第二帧应有）
4. ToolCard / SystemNote / SubAgentGroup 正常显示
5. /clear 无残留
6. 消息滚动流畅

- [ ] **Step 4: 运行现有测试**

```bash
cargo test -p peri-tui --lib 2>&1
```

预期：部分测试因 RENDER_CACHE 移除而失败——需要更新测试中的 RENDER_CACHE 断言。

- [ ] **Step 5: 运行 peri-widgets 测试**

```bash
cargo test -p peri-widgets --lib 2>&1
```

预期：markdown 相关测试需删除（模块已删除），其余 pass。

- [ ] **Step 6: Commit**

```bash
cd /Users/konghayao/code/ai/perihelion
git add -A
git commit -m "feat: fix all compile errors and complete migration integration"
```

---

### Task 21: 切换依赖为 crates.io 版本（最终步骤）

**Files:**
- Modify: `peri-tui/Cargo.toml`

待 ratatui-kit-markdown 增强版发布到 crates.io 后：

- [ ] **Step 1: 改 path dep 为 crates.io**

```toml
# 从：
ratatui-kit-markdown = { path = "/Users/konghayao/code/ai/ratatui-kit-contrib/crates/ratatui-kit-markdown", features = ["markdown-highlight"] }
# 改为：
ratatui-kit-markdown = { version = "0.3", features = ["markdown-highlight"] }
```

- [ ] **Step 2: 确认 ratatui-kit auto_quit_on_ctrl_c 上游已合并**

如果上游 PR 未合并，保持 fork 引用。否则切官方 0.10.1。

- [ ] **Step 3: Commit**

```bash
git add peri-tui/Cargo.toml
git commit -m "chore: switch ratatui-kit-markdown to crates.io release"
```

---

## 风险/注意事项

1. **`auto_quit_on_ctrl_c` 上游 PR**：官方 ratatui-kit 0.10.1 的 Ctrl+C 自动退出行为由 `SystemContext.auto_quit_on_ctrl_c` 控制。peri-tui 的 `app_shell.rs` 已设置 `ctx.auto_quit_on_ctrl_c = false`。如果切官方版本后 Ctrl+C 行为异常，检查该字段在官方版本中是否存在。

2. **text_selection.rs 功能失效**：文本选区依赖 RENDER_CACHE.entries (Vec<Line>) + WrappedLineInfo 做坐标映射。删除 RENDER_CACHE 后选区无法工作。后续独立 plan 用 ratatui-kit 原生能力补回。

3. **第一帧无高亮**：Markdown 组件的第一帧 `use_previous_size` 返回 width=0，触发 light fallback（无 syntect 高亮）。第二帧起正常。用户会看到短暂的代码无高亮闪烁。

4. **Hook 顺序**：MessageArea 组件的 hooks 列表发生根本变化——从订阅 RENDER_CACHE 改为订阅 VIEW_MODELS。ratatui-kit 按 hook 调用顺序管理状态，新组件自然不会有新旧 hook 顺序冲突。

5. **SubAgentGroup 递归渲染**：SubAgentGroup 内部的 child ViewModel 需要递归分发到各变体组件，通过 `bubbles/mod.rs` 中暴露的 `render_view_model` 函数实现。

6. **ScrollView 高度近似**：删除 wrap_map 后，`total_height` 使用 logical line count（不考虑换行）。ratatui-kit ScrollView 原生处理视口裁剪，不需要二分查找。超宽行的视觉效果由 ratatui-kit 内部 Wrap 处理。
