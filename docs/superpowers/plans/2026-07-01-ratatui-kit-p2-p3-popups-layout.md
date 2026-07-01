# ratatui-kit 迁移: Phase 2-3 弹窗系统 & 主布局迁移

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 4 个交互弹窗迁移到 ratatui-kit Modal 组件，用 flex 布局替代 Constraint-based 布局

**Architecture:** 弹窗用 ratatui-kit 内置 Modal/ConfirmModal/AlertModal + `use_input_layer` 实现 modal capture；主布局用 `View(flex_direction: Vertical)` + `Constraint::Length/Fill` 替代 `Layout::split`；状态栏和 setup wizard 独立 `#[component]` 化。Phase 5 之前的消息区和输入框用 `widget()` 桥接。

**Tech Stack:** ratatui-kit 0.6 (features=["full"]), Rust 2024, ratatui 0.30

**前置依赖**: Phase 0-1 必须已完成（ratatui-kit 依赖 + 14 个面板组件文件已创建）

---

## Phase 2: 弹窗系统迁移

### Task 2a: 创建 kit 弹窗模块骨架

**Files:**
- Create: `peri-tui/src/kit/popups/mod.rs`
- Create: `peri-tui/src/kit/popups/hitl_popup.rs`
- Create: `peri-tui/src/kit/popups/ask_user_popup.rs`
- Create: `peri-tui/src/kit/popups/rewind_popup.rs`
- Create: `peri-tui/src/kit/popups/oauth_popup.rs`
- Modify: `peri-tui/src/kit/mod.rs`

- [ ] **Step 1: 创建 popups/mod.rs**

```rust
//! ratatui-kit 弹窗组件集合

pub mod ask_user_popup;
pub mod hitl_popup;
pub mod oauth_popup;
pub mod rewind_popup;
```

- [ ] **Step 2: 修改 kit/mod.rs**

```rust
// 在现有 mod.rs 末尾添加：
pub mod popups;
```

- [ ] **验证**: `cargo check -p peri-tui`

---

### Task 2b: HITL 弹窗 — ConfirmModal

**Files:**
- Modify: `peri-tui/src/kit/popups/hitl_popup.rs`

```rust
//! HITL 工具审批弹窗 — ratatui-kit ConfirmModal 实现。

use crate::app::App;
use ratatui_kit::{
    prelude::*,
    ratatui::{
        style::{Style, Stylize},
        text::Line,
    },
};
use crate::ui::theme;

#[component]
pub fn HitlPopup(
    _hooks: &mut Hooks,
    app: &App,
    on_submit: Handler<'static, ()>,
    on_cancel: Handler<'static, ()>,
) -> impl Into<AnyElement<'static>> {
    let prompt = match &app.session_mgr.current().agent.interaction_prompt {
        Some(crate::app::InteractionPrompt::Approval(p)) => p,
        _ => return element!(Fragment {}),
    };

    let item_count = prompt.items.len();
    let lc = &app.services.lc;

    let title = if item_count == 1 {
        Line::from("Approve tool call?").style(Style::new().fg(theme::THINKING).bold())
    } else {
        Line::from(format!("Approve {} tool calls?", item_count)).style(Style::new().fg(theme::THINKING).bold())
    };

    let content = prompt.items.iter().enumerate()
        .map(|(idx, (item, &approved))| {
            let status = if approved { "✓" } else { "✗" };
            format!("{} {} {}", status, idx + 1, item.tool_name)
        })
        .collect::<Vec<_>>()
        .join("\n");

    ConfirmModal(
        open: true,
        width: ratatui::layout::Constraint::Length(70),
        height: ratatui::layout::Constraint::Length((item_count + 5).min(20).max(8) as u16),
        title,
        content,
        confirm_text: "Approve All".to_string(),
        cancel_text: "Reject All".to_string(),
        border_style: Style::new().fg(theme::WARNING),
        title_style: Style::new().fg(theme::THINKING).bold(),
        selected_button_style: Style::new().fg(theme::THINKING).bold(),
        on_confirm: move |_: ()| on_submit(()),
        on_cancel: move |_: ()| on_cancel(()),
    )
}
```

---

### Task 2c: Rewind 弹窗 — AlertModal

**Files:**
- Modify: `peri-tui/src/kit/popups/rewind_popup.rs`

```rust
//! Rewind 回滚确认弹窗 — ratatui-kit AlertModal 实现。

use crate::app::{App, InteractionPrompt, RewindMode};
use crate::ui::theme;
use ratatui_kit::{
    crossterm::event::KeyCode,
    prelude::*,
    ratatui::{style::{Style, Stylize}, text::Line},
};

#[component]
pub fn RewindPopup(
    _hooks: &mut Hooks,
    app: &App,
    on_confirm: Handler<'static, ()>,
    on_cancel: Handler<'static, ()>,
) -> impl Into<AnyElement<'static>> {
    let prompt = match &app.session_mgr.current().agent.interaction_prompt {
        Some(InteractionPrompt::Rewind(p)) => p,
        _ => return element!(Fragment {}),
    };

    let title = Line::from("Rewind Confirmation").style(Style::new().fg(theme::ACCENT).bold());

    let mut lines: Vec<String> = Vec::new();
    for (i, item) in prompt.items.iter().enumerate() {
        let marker = if i == prompt.cursor { "❯ " } else { "  " };
        lines.push(format!("{}{} [{} msg after]", marker, item.summary, item.message_count_after));
    }

    if prompt.mode == RewindMode::ConfirmRevert && !prompt.items.is_empty() {
        let selected = &prompt.items[prompt.cursor];
        lines.push(String::new());
        lines.push("Files to restore:".to_string());
        for fc in &selected.file_changes {
            lines.push(format!("  {} ({})", fc.path, fc.operation));
        }
    }

    let message = lines.join("\n");
    let content_height = (lines.len() + 4).min(20).max(8) as u16;

    AlertModal(
        open: true,
        width: ratatui::layout::Constraint::Length(70),
        height: ratatui::layout::Constraint::Length(content_height),
        title,
        message,
        close_hint: Line::from(" Enter/y = confirm | Esc/n/q = cancel ").centered(),
        close_keys: vec![KeyCode::Enter, KeyCode::Char('y'), KeyCode::Char('Y')],
        border_style: Style::new().fg(theme::ACCENT),
        on_close: move |_: ()| on_confirm(()),
    )
}
```

---

### Task 2d: OAuth 弹窗 — Modal

**Files:**
- Modify: `peri-tui/src/kit/popups/oauth_popup.rs`

```rust
//! OAuth 授权弹窗 — ratatui-kit Modal 实现。

use crate::app::App;
use crate::ui::theme;
use ratatui_kit::{
    prelude::*,
    ratatui::{layout::{Constraint, Direction}, style::{Style, Stylize}, text::Line},
};

#[component]
pub fn OauthPopup(
    _hooks: &mut Hooks,
    app: &App,
    on_open_url: Handler<'static, ()>,
    on_cancel: Handler<'static, ()>,
) -> impl Into<AnyElement<'static>> {
    let prompt = match app.global_ui.oauth_prompt.as_ref() {
        Some(p) => p,
        None => return element!(Fragment {}),
    };

    Border(
        flex_direction: Direction::Vertical,
        border_style: Style::new().fg(theme::THINKING),
        top_title: Line::from(format!("OAuth: {}", prompt.server_name)).fg(theme::THINKING).bold(),
        bottom_title: Line::from(" Enter/o = open | Esc/q/c = cancel ").fg(theme::MUTED).centered(),
        height: Constraint::Length(8),
    ) {
        Text(text: Line::from("Authorization required:").fg(theme::TEXT))
        Text(text: Line::from(&prompt.authorization_url).fg(theme::SAGE))
        if prompt.url_opened {
            Text(text: Line::from("Opened in browser.").fg(theme::SAGE))
        } else {
            Text(text: Line::from("Press Enter to open URL.").fg(theme::MUTED))
        }
        if let Some(ref err) = prompt.error_message {
            Text(text: Line::from(err).fg(theme::ERROR))
        }
    }
}
```

---

### Task 2e: AskUser 弹窗 — Modal + ScrollView

**Files:**
- Modify: `peri-tui/src/kit/popups/ask_user_popup.rs`

```rust
//! AskUser 多问题表单弹窗 — ratatui-kit Modal + ScrollView 实现。

use crate::app::App;
use crate::ui::theme;
use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind},
    prelude::*,
    ratatui::{layout::{Constraint, Direction}, style::{Style, Stylize}, text::{Line, Span}},
};

#[component]
pub fn AskUserPopup(
    mut hooks: Hooks,
    app: &mut App,
    on_submit: Handler<'static, ()>,
    on_cancel: Handler<'static, ()>,
) -> impl Into<AnyElement<'static>> {
    let prompt = match app.session_mgr.current_mut().agent.interaction_prompt.as_mut() {
        Some(crate::app::InteractionPrompt::Questions(p)) => p,
        _ => return element!(Fragment {}),
    };

    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, {
        move |event| {
            let Event::Key(key) = event else { return EventResult::Ignored };
            if key.kind != KeyEventKind::Press { return EventResult::Ignored }
            match key.code {
                KeyCode::Esc => { on_cancel(()); EventResult::Consumed }
                KeyCode::Enter => { on_submit(()); EventResult::Consumed }
                _ => EventResult::Ignored,
            }
        }
    });

    let cur = &prompt.questions[prompt.active_tab];
    let mut lines: Vec<String> = Vec::new();

    for l in cur.data.question.lines() { lines.push(l.to_string()); }
    lines.push(String::new());

    for (i, opt) in cur.data.options.iter().enumerate() {
        let is_selected = cur.selected.get(i).copied().unwrap_or(false);
        if cur.data.multi_select {
            let check = if is_selected { "●" } else { "○" };
            lines.push(format!(" {} {}. {}", check, i + 1, opt.label));
        } else {
            lines.push(format!(" {}. {}", i + 1, opt.label));
        }
    }

    let content = lines.join("\n");
    let content_height = (lines.len() + 4).min(24).max(10) as u16;

    Modal(
        open: true,
        width: Constraint::Length(72),
        height: Constraint::Length(content_height),
        placement: ratatui_kit::components::Placement::Center,
        style: Style::new().dim(),
    ) {
        Border(
            flex_direction: Direction::Vertical,
            border_style: Style::new().fg(theme::THINKING),
            top_title: Line::from("Ask User").fg(theme::ACCENT).bold(),
            padding: ratatui::widgets::Padding::new(1, 1, 0, 0),
        ) {
            WrappedText(text: content, wrap_width: Some(68), style: Style::new().fg(theme::TEXT))
        }
    }
}
```

---

### Task 2f: 集成弹窗渲染到 main_ui

**Files:**
- Modify: `peri-tui/src/ui/main_ui/mod.rs`

- [ ] **Step 1: 替换弹窗渲染调用**

在 `render_session_column` 函数中，将 popup 渲染调用从 legacy 替换为 kit 组件：

```rust
// 修改前:
popups::hitl::render_hitl_popup(f, app, panel_area);
// 修改后 (Phase 2 bridge):
crate::kit::popups::hitl_popup::render_hitl_with_frame(f, app, panel_area);
```

注意：Phase 2 使用 `render_*_with_frame` bridge 函数（创建在各自 popup 文件中），将 ratatui-kit 组件渲染到 raw Frame 上。Phase 3 时切换到 element! 全量渲染。

---

## Phase 3: 主布局 + 状态栏迁移

### Task 3a: 主布局组件

**Files:**
- Create: `peri-tui/src/kit/layout.rs`
- Modify: `peri-tui/src/kit/mod.rs`

```rust
//! SessionColumn 顶层布局组件。

use ratatui_kit::{
    prelude::*,
    ratatui::layout::{Constraint, Direction},
};

#[component]
pub fn SessionColumn(_hooks: Hooks) -> impl Into<AnyElement<'static>> {
    View(flex_direction: Direction::Vertical) {
        // 1. Sticky Header (Phase 5 bridge)
        // 2. Message Area (Phase 5 bridge) — Constraint::Fill(1)
        // 3. Attachment Bar
        // 4. Panel Area
        // 5. Queued Messages
        // 6. Input Area (Phase 5 bridge)
        // 7. Status Bar — ratatui-kit 原生组件
        // 8. Background Agent Bar
    }
}
```

### Task 3b: 状态栏组件

**Files:**
- Create: `peri-tui/src/kit/status_bar.rs`

状态栏组件作为蓝图文件，展示两行信息（权限模式/工作目录/模型名/资源 + 快捷键提示）。Phase 5 正式启用。

### Task 3c: Setup Wizard 组件

**Files:**
- Create: `peri-tui/src/kit/setup_wizard.rs`

四阶段向导（Choose → Language → Form → Done），每阶段对应一个渲染分支。Phase 3 仅创建基础框架 + Esc 退出处理。

### Task 3d: AppShell 根组件

**Files:**
- Create: `peri-tui/src/kit/app_shell.rs`

```rust
#[component]
pub fn AppShell(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut exit = hooks.use_exit();

    hooks.use_event_handler(EventScope::Current, EventPriority::Normal, move |event| {
        let Event::Key(key) = event else { return EventResult::Ignored };
        if key.kind != KeyEventKind::Press { return EventResult::Ignored }
        match key.code {
            KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => { exit(); EventResult::Consumed }
            _ => EventResult::Ignored,
        }
    });

    View(flex_direction: Direction::Vertical) {
        // Phase 5: SessionColumn → widget() bridge
        // Phase 2: 弹窗 Modal overlay
    }
}
```

---

## 验收标准

- [ ] 4 个弹窗 kit 组件文件已创建 + 编译通过
- [ ] layout.rs, status_bar.rs, setup_wizard.rs, app_shell.rs 已创建
- [ ] `cargo check -p peri-tui` 通过
- [ ] `cargo clippy -p peri-tui --lib -- -D warnings` 通过
