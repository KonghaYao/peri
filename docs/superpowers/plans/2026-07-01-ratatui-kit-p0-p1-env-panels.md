# ratatui-kit 迁移: Phase 0-1 环境准备 & 面板组件化

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 添加 ratatui-kit 依赖解决 edition 冲突，验证 PoC 桥接可行，将 14 个面板逐一迁移为 ratatui-kit #[component]

**Architecture:** peri-tui/Cargo.toml 加独立 `[workspace]` 表脱离父 workspace edition 约束；新建 `kit/` 模块封装所有 ratatui-kit 组件；面板通过 `PanelState` trait adapter 桥接到新组件；key dispatch 暂时保留原路径 `handle_key`

**Tech Stack:** Rust 2021/2024（peri-tui 升到 2024）, ratatui 0.30, ratatui-kit 0.6 (features=["full"]), tui-input, tui-textarea-2

---

## 架构决策记录

### ADR-001: 双轨并行策略

**决策**：Phase 1 创建 ratatui-kit 组件作为"光谱副本"（spectral copies）——组件与现有 `PanelState` impl 并行存在，互不依赖。

**理由**：
- ratatui-kit 是"接管整个终端"的框架，无法在现有 `ratatui::Frame` 渲染管道中直接嵌入其组件树
- 创建独立的 `#[component]` 函数作为未来集成的基础代码，Phase 1 末尾通过 `widget()` 桥接适配器将组件渲染到现有 Frame
- 现有 `PanelState::render()` / `handle_key()` 保持不变，双轨并行运行

### ADR-002: PanelState 接口保持不变

**决策**：Phase 0-1 不修改 `PanelState` trait、`PanelEffect`、`PanelReadContext`。key dispatch 继续走 `ModalState::Panel` → `handle_key`。Phase 4 才迁移到 `use_event_handler`。

---

## Phase 0: 环境准备 & PoC

### 预备检查

当前状态：
- 父 workspace: `Cargo.toml` line 19: `edition = "2021"`
- peri-tui: `edition.workspace = true` → 继承 2021
- ratatui-kit 要求 `edition = "2024"`
- peri-tui 当前 ratatui: `version = "0.30.1", features = ["unstable-rendered-line-info", "unstable-widget-ref"]`

ratatui-kit 使用的 ratatui 0.30 与 peri-tui 的 0.30.1 兼容（semver-compatible）。

### Task 0a: 修改 peri-tui/Cargo.toml

**Files:**
- Modify: `peri-tui/Cargo.toml`

- [ ] **Step 1: 添加独立 [workspace] 表 + upgrade edition to 2024 + 添加 ratatui-kit 依赖**

```toml
[workspace]
# ratatui-kit requires edition 2024 — this empty [workspace] table detaches
# peri-tui from the parent workspace so it can use its own edition.
```

同时修改 package 表，将 `edition.workspace = true` 改为 `edition = "2024"`，并添加依赖：

```toml
[package]
name = "peri-tui"
version.workspace = true
edition = "2024"
description = "TUI interface for Rust Agent - interactive terminal playground"

[dependencies]
# ... existing deps ...
ratatui-kit = { version = "0.6", features = ["full"] }
tui-input = "0.8"
```

由于 `[workspace]` 表的存在会阻止 `workspace = true` 引用外部 workspace，所有 `workspace = true` 的依赖都需要改为直接声明版本号。

```toml
[workspace]
# Empty workspace to detach from parent for edition 2024

[package]
name = "peri-tui"
version = "0.2.0"
edition = "2024"
description = "TUI interface for Rust Agent - interactive terminal playground"

[dependencies]
serde = { version = "1.0", features = ["derive", "rc"] }
peri-agent = { path = "../peri-agent" }
peri-middlewares = { path = "../peri-middlewares" }
peri-lsp = { path = "../peri-lsp" }
ratatui = { version = "0.30.1", features = ["unstable-rendered-line-info", "unstable-widget-ref"] }
ratatui-kit = { version = "0.6", features = ["full"] }
tui-input = "0.8"
tokio = { version = "1", features = ["full"] }
tokio-util = { version = "0.7", features = ["rt"] }
anyhow = "1.0"
chrono = { version = "0.4", features = ["serde"] }
uuid = { version = "1", features = ["v4", "v7", "serde"] }
serde_json = "1.0"
async-trait = "0.1"
tracing = "0.1"
dirs-next = "2"
tui-textarea-2 = "0.11"
parking_lot = "0.12"
arboard = "3"
png = "0.18"
base64 = { version = "0.22", features = ["alloc"] }
peri-widgets = { path = "../peri-widgets", features = ["markdown-highlight"] }
peri-acp = { path = "../peri-acp" }
peri-acp-types = { path = "../peri-acp-types" }
peri-web-pty = { path = "../peri-web-pty" }
agent-client-protocol = { version = "0.14", features = ["unstable"] }
agent-client-protocol-schema = { version = "0.13", features = ["unstable_elicitation"] }
unicode-width = "0.2"
unicode-segmentation = "1"
clap = { version = "4", features = ["derive"] }
sysinfo = "0.39"
fluent = "0.17"
fluent-bundle = "0.16"
unic-langid = { version = "0.9", features = ["macros"] }
tokio-tungstenite = { version = "0.29", features = ["rustls-tls-native-roots"] }
aes-gcm = "0.10"
ring = "0.17"
rmp-serde = "1.3"
thiserror = "2.0"
futures-util = "0.3"
fuzzy-matcher = "0.3"

[dev-dependencies]
tempfile = "3"

[target.'cfg(not(target_os = "windows"))'.dependencies]
tikv-jemallocator = "0.6"
tikv-jemalloc-ctl = { version = "0.6", features = ["stats", "use_std"] }
tikv-jemalloc-sys = "0.6"
```

- [ ] **Step 2: 验证编译**

```bash
cd peri-tui && cargo metadata --format-version 1 2>/dev/null | grep -E '"ratatui":|"ratatui-kit":' || echo "Need to check versions manually"
cd peri-tui && cargo check 2>&1
```

---

### Task 0b: 创建 kit 模块骨架

**Files:**
- Create: `peri-tui/src/kit/mod.rs`
- Create: `peri-tui/src/kit/panels/mod.rs`

- [ ] **Step 1: 创建 kit/mod.rs**

```rust
//! ratatui-kit 集成模块。
//!
//! 该模块包含所有 ratatui-kit #[component] 面板组件。
//! Phase 1 期间，组件与现有 PanelState 实现并行存在（双轨策略）。
//! Phase 4 将通过桥接适配器集成到现有渲染管道。

pub mod panels;
```

- [ ] **Step 2: 创建 kit/panels/mod.rs**

```rust
//! 面板组件集合 —— ratatui-kit #[component] 版本。
//!
//! 每个面板对应一个子模块，使用 ratatui-kit 的 element! 宏、
//! hooks（use_state, use_atom, use_event_handler）和内置组件
//! （Border, Text, Select, ScrollView, Input, VirtualList 等）。

pub mod agent;
pub mod betas;
pub mod config;
pub mod cron;
pub mod hooks;
pub mod login;
pub mod mcp;
pub mod memory;
pub mod model;
pub mod plugin;
pub mod status;
pub mod tasks;
pub mod thread_browser;
pub mod workflow;
```

- [ ] **Step 3: 验证**

```bash
cd peri-tui && cargo check 2>&1 | head -30
```

---

### Task 0c: 创建 PoC 验证组件

**Files:**
- Create: `peri-tui/src/kit/poc.rs`
- Modify: `peri-tui/src/kit/mod.rs`

- [ ] **Step 1: 创建 PoC 组件**

```rust
//! PoC 验证组件 —— Phase 0 完成后删除此文件。

use ratatui_kit::{
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::{Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};

#[component]
fn PocPanel(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut selected = hooks.use_state(|| 0_usize);
    let items = vec!["Opus", "Sonnet", "Haiku"];

    let lines: Vec<ratatui::text::Line> = items
        .iter()
        .enumerate()
        .map(|(i, label)| {
            let is_selected = *selected.read() == i;
            let style = if is_selected {
                Style::new().yellow().bold()
            } else {
                Style::new().white()
            };
            let prefix = if is_selected { "> " } else { "  " };
            Line::from(vec![
                Span::styled(prefix, Style::new().dark_gray()),
                Span::styled(*label, style),
            ])
        })
        .collect();

    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().dark_gray(),
            top_title: Line::from(" PoC Panel ").cyan().bold().centered(),
            bottom_title: Line::from(" Enter select | Esc close ").dark_gray().centered(),
            width: Constraint::Length(40),
            height: Constraint::Length(10),
        ) {
            { widget(Paragraph::new(ratatui::text::Text::from(lines))) }
        }
    )
}
```

- [ ] **Step 2: 注册并验证**

```bash
cd peri-tui && cargo check 2>&1
```

- [ ] **Step 3: 清理 PoC**

删除 `peri-tui/src/kit/poc.rs`，从 `mod.rs` 移除 `pub mod poc;`。

---

## Phase 1: 面板组件化

Phase 1 分三批迁移 14 个面板。每批完成后 `cargo check` 验证。

### 第一批: 简单面板 (Model / Login / Config / Agent)

#### Task 1: ModelPanel

**Files:**
- Create: `peri-tui/src/kit/panels/model.rs`

```rust
//! ratatui-kit ModelPanel component.

use ratatui_kit::{
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::{Style, Stylize},
        text::Line,
    },
};

use crate::ui::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AliasTab {
    Opus, Sonnet, Haiku,
}

impl AliasTab {
    pub fn label(&self) -> &'static str {
        match self { Self::Opus => "Opus", Self::Sonnet => "Sonnet", Self::Haiku => "Haiku" }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effort { Low, Medium, High, Xhigh, Max }

impl Effort {
    fn label(&self) -> &'static str {
        match self { Self::Low => "low", Self::Medium => "medium", Self::High => "high", Self::Xhigh => "xhigh", Self::Max => "max" }
    }
    fn next(&self) -> Self {
        match self { Self::Low => Self::Medium, Self::Medium => Self::High, Self::High => Self::Xhigh, Self::Xhigh => Self::Max, Self::Max => Self::Low }
    }
}

const MAX_TOKENS_PRESETS: &[u32] = &[8000, 16000, 32000, 64000, 128000];

#[derive(Default)]
struct ModelPanelProps {
    active_alias: AliasTab,
    effort: Effort,
    max_tokens: u32,
    context_1m: bool,
}

#[component]
fn ModelPanel(mut hooks: Hooks, props: ModelPanelProps) -> impl Into<AnyElement<'static>> {
    let items: Vec<&'static str> = vec!["Opus", "Sonnet", "Haiku"];
    let settings_text = format!(
        "Max Tokens: {}   Effort: {}   1M Context: {}",
        props.max_tokens, props.effort.label(),
        if props.context_1m { "ON" } else { "OFF" }
    );

    element!(
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::BORDER),
            top_title: Line::from(" Select model ").fg(theme::THINKING).bold().centered(),
            width: Constraint::Length(44),
            height: Constraint::Length(14),
        ) {
            Select::<&'static str>(
                width: Constraint::Fill(1),
                items: items.clone(),
                default_index: Some(match props.active_alias { AliasTab::Opus => 0, AliasTab::Sonnet => 1, AliasTab::Haiku => 2 }),
                highlight_style: Style::new().fg(theme::SAGE).bold(),
                style: Style::new().fg(theme::TEXT),
                empty_message: "",
                on_select: move |_item: &'static str| {},
            )
            Text(text: Line::from(settings_text).fg(theme::MUTED).centered())
        }
    )
}
```

#### Task 2-4: Login, Config, Agent 面板

> 各面板完整代码请参见 agent 输出。每个面板遵循相同模式：`#[component] fn XxxPanel(mut hooks: Hooks, props: ...) → element!(Border(...) { ... })`。使用 `Select<T>` 处理列表选择，`Input`/`SearchInput` 处理文本输入，`ScrollView` 处理长列表。

#### Task 5-9: Hooks, Mcp, Plugin, Cron, Tasks 面板（第二批）

使用 `ScrollView` + 嵌套 `Select` 的样式。每项一个 `View(key: i)` 确保 reconciliation 稳定。

#### Task 10-14: Status, Memory, Betas, Workflow, ThreadBrowser 面板（第三批）

`StatusPanel` 使用 `Border` + `Text` 展示数据行。`MemoryPanel` 使用 `SearchInput` + `ScrollView` 组合。`WorkflowPanel` 和 `ThreadBrowserPanel` 使用 `VirtualList` 处理大数据量。

### Task 15: 整体验证

```bash
cd peri-tui && cargo check 2>&1
```

### Task 16: 添加 adapter 接口

**Files:**
- Create: `peri-tui/src/kit/adapter.rs`
- Modify: `peri-tui/src/kit/mod.rs`

```rust
// kit/adapter.rs
use ratatui::layout::Rect;
use ratatui::Frame;

#[allow(dead_code)]
pub fn render_kit_component(_f: &mut Frame, _area: Rect, _component_tree: &dyn std::any::Any) {
    // Phase 4: 实现 ratatui-kit → ratatui Frame 渲染桥接
}
```

---

## 验收标准

- [ ] `peri-tui/Cargo.toml` 有独立 `[workspace]` 表 + `ratatui-kit` 依赖
- [ ] `peri-tui` edition 升级到 2024 且编译通过
- [ ] `peri-tui/src/kit/mod.rs` 模块入口存在
- [ ] `peri-tui/src/kit/panels/` 下 14 个文件各含一个 `#[component] fn`
- [ ] 所有 14 个 PanelState impl（`panel/panels/*.rs`）未修改
- [ ] `cargo check -p peri-tui` 全部通过

### Critical Files for Implementation
- `peri-tui/Cargo.toml` — edition 2024 升级 + ratatui-kit 依赖 + [workspace] 独立表
- `peri-tui/src/kit/panels/model.rs` — 第一个 Panel 迁移（验证 Select + use_state + 主题色模式）
- `peri-tui/src/kit/panels/workflow.rs` — VirtualList 使用验证
- `peri-tui/src/kit/panels/memory.rs` — SearchInput + ScrollView 组合验证
