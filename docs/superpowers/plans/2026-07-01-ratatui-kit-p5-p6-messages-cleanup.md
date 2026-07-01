# ratatui-kit 迁移: Phase 5-6 消息区/输入框迁移 & 清理测试

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task.

**Goal:** 通过 `widget()` 桥接将消息区和输入框接入 ratatui-kit 组件树，删除键盘 fallback 死代码，更新测试和文档

**Architecture:** 消息区核心渲染逻辑不变（`view_render.rs` + `message_area.rs`），通过 `widget()` 封装为 ratatui-kit 兼容 Widget 并实现 `Widget` trait；输入框同样 `widget()` 桥接 `tui_textarea-2` 并实现 `StatefulWidget`；流式更新通过 `Atom<ViewModelsSnapshot>` 触发重渲染；删除 `event/keyboard/` 全目录后将输入处理迁移到 state machine + `use_event_handler`

**Tech Stack:** ratatui-kit 0.6 `widget()`/`stateful()` 桥接, ratatui 0.30, tui-textarea-2 0.11, Rust 2024

**前置依赖**: Phase 0-4 必须已完成（ratatui-kit 依赖 + kit 模块 + panel/popup/event_handlers 全量）

---

## ADR

### ADR-010: 消息区 widget() 桥接（非重写）

**决策**：`MessageAreaWidget` 不重写 ~1400 行核心渲染逻辑（`view_render.rs` 832 行 + `message_area.rs` 608 行），通过 `Widget` trait 封装现有代码。

### ADR-011: 输入框 widget() 桥接（非重写）

**决策**：`TextareaWidget` 封装 `tui_textarea::TextArea`，实现 `StatefulWidget`。Phase 6 迁移键盘处理到 `use_event_handler`。

---

## Phase 5: 消息区 + 输入框 widget 桥接

### Task 5a: MessageAreaWidget

**Files:**
- Create: `peri-tui/src/kit/message_area.rs`
- Modify: `peri-tui/src/kit/mod.rs`

- [ ] **Step 1: 创建 `kit/message_area.rs`**

```rust
//! MessageAreaWidget — ratatui-kit widget() 桥接消息区。
//!
//! 封装现有 message_area.rs 渲染逻辑为 ratatui Widget。
//! 流式更新由外部调用 build_sync_render_cache_v2 管理。

use ratatui::{buffer::Buffer, layout::Rect, style::Style, text::Line, widgets::Widget};
use crate::app::App;
use crate::app::message_state::MessageRenderCache;
use crate::ui::main_ui::message_area::{build_sync_render_cache_v2, viewport_clip};
use crate::ui::theme;
use peri_acp_types::view_model::ViewModel;

pub struct MessageAreaWidget<'a> {
    pub cache: &'a MessageRenderCache,
    pub scroll_offset: u16,
    pub diff_visible: bool,
    pub loading: bool,
    pub spinner_line: Option<Line<'static>>,
    pub max_scroll: u16,
}

impl MessageAreaWidget<'_> {
    pub fn from_app<'a>(
        app: &'a App,
        view_models: &'a [ViewModel],
        width: u16,
    ) -> (MessageAreaWidget<'a>, MessageRenderCache) {
        let diff_visible = app.session_mgr.current().ui.diff_visible;
        let cache = build_sync_render_cache_v2(view_models, diff_visible, width);

        let spinner_line = build_spinner_line(app);
        let widget = MessageAreaWidget {
            cache: app.session_mgr.current().messages.message_cache.as_ref()
                .unwrap_or_else(|| panic!("message_cache is None")),
            scroll_offset: app.session_mgr.current().ui.scroll_offset,
            diff_visible,
            loading: app.session_mgr.current().ui.loading,
            spinner_line,
            max_scroll: app.session_mgr.current().ui.scrollbar_max_offset,
        };
        (widget, cache)
    }
}

fn build_spinner_line(app: &App) -> Option<Line<'static>> {
    // 复用现有 spinner 逻辑（参考 message_area.rs 的 loading_spinner line）
    if app.session_mgr.current().ui.loading {
        let tick = app.session_mgr.current().spinner_state.tick();
        let frame = peri_widgets::spinner::animation::tick_to_frame(tick);
        let elapsed = peri_widgets::spinner::animation::format_elapsed(
            app.session_mgr.current().spinner_state.elapsed_ms(),
        );
        Some(Line::from(format!(" {} {}", frame, elapsed)))
    } else {
        None
    }
}

impl Widget for &MessageAreaWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let visible_height = area.height;

        // 视口裁剪
        let vis_start = self.scroll_offset as usize;
        let vis_end = (vis_start + visible_height as usize + 1)
            .min(self.cache.total_lines);

        let first = self.cache.wrap_map
            .partition_point(|info| info.visual_row_end as usize <= vis_start);
        let last = self.cache.wrap_map
            .partition_point(|info| (info.visual_row_start as usize) < vis_end)
            .saturating_sub(1);

        if first > last || first >= self.cache.lines.len() {
            return;
        }

        let lines: Vec<Line> = self.cache.lines[first..=last].to_vec();
        for (i, line) in lines.iter().enumerate() {
            if i as u16 >= visible_height { break; }
            let y = area.y + i as u16;
            for (x, span) in line.spans.iter().enumerate() {
                let x_pos = area.x + x as u16;
                if x_pos < area.right() {
                    buf.set_span(x_pos, y, span, span.content.len() as u16);
                }
            }
        }
    }
}
```

- [ ] **验证**: `cargo check -p peri-tui`

---

### Task 5b: TextareaWidget

**Files:**
- Create: `peri-tui/src/kit/input_area.rs`
- Modify: `peri-tui/src/kit/mod.rs`

- [ ] **Step 1: 创建 `kit/input_area.rs`**

```rust
//! TextareaWidget — ratatui-kit widget() 桥接 tui_textarea-2。

use ratatui::{buffer::Buffer, layout::Rect, style::{Modifier, Style}, widgets::{StatefulWidget, Widget}};
use crate::ui::theme;
use tui_textarea::TextArea;

#[derive(Clone, Default)]
pub struct TextareaState {
    pub focused: bool,
    pub bar_focused: bool,
    pub loading: bool,
}

pub struct TextareaWidget<'a> {
    pub textarea: &'a mut TextArea<'static>,
    pub state: &'a TextareaState,
}

impl StatefulWidget for TextareaWidget<'_> {
    type State = TextareaState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        if state.bar_focused {
            self.textarea.set_style(Style::default().fg(ratatui::style::Color::DarkGray));
        } else {
            self.textarea.set_style(Style::default().fg(theme::TEXT));
        }

        if state.focused {
            self.textarea.render(area, buf);
        } else {
            let mut ta = self.textarea.clone();
            ta.set_cursor_style(Style::default());
            ta.render(area, buf);
        }

        // macOS cursor 残影修复
        #[cfg(not(target_os = "windows"))]
        if state.focused {
            for y in area.y..area.bottom() {
                for x in area.x..area.right() {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        if cell.symbol() == " " && cell.modifier.contains(Modifier::REVERSED) {
                            cell.modifier.remove(Modifier::REVERSED);
                            cell.bg = theme::TEXT;
                        }
                    }
                }
            }
        }
    }
}
```

- [ ] **验证**: `cargo check -p peri-tui`

---

### Task 5c: 布局集成（kit/layout.rs + render/mod.rs）

**Files:**
- Modify: `peri-tui/src/kit/layout.rs`
- Modify: `peri-tui/src/render/mod.rs`

- [ ] **Step 1: layout.rs 集成**

```rust
// kit/layout.rs: 在 SessionColumn 中通过 widget() 使用 MessageAreaWidget + TextareaWidget

#[component]
pub fn SessionColumn(mut hooks: Hooks, props: SessionColumnProps) -> impl Into<AnyElement<'static>> {
    View(flex_direction: Direction::Vertical) {
        // 2. MessageArea (Fill(1))
        // 6. InputArea (Length(line_count + 2).min(screen_h * 2/5).max(3))
        // 7. StatusBar
    }
}
```

- [ ] **Step 2: render/mod.rs draw() 集成**

保留 legacy 路径用 `#[cfg(not(feature = "kit-layout"))]` 保护，新增 kit 路径用 feature gate 控制。

- [ ] **验证**: `cargo check -p peri-tui && cargo build -p peri-tui`

---

### Task 5d: 流式更新桥接

- 在 `draw()` 函数中注入 `Atom<Vec<ViewModel>>` 更新
- 每次 streaming tick 时写 Atom → 触发 kit 组件重渲染
- 测试：启动 TUI 发送消息，确认流式消息实时更新且帧率 ≥ 30 FPS

---

## Phase 6: 清理 & 测试

### Task 6a: 删除 event/keyboard/ 死代码

**删除文件**:
- `peri-tui/src/event/keyboard.rs`
- `peri-tui/src/event/keyboard/bar_focus.rs`
- `peri-tui/src/event/keyboard/normal_keys.rs`
- `peri-tui/src/event/keyboard/popups.rs`
- `peri-tui/src/event/keyboard/setup_wizard.rs`

**修改文件**:
- `peri-tui/src/event/mod.rs` — 删除 `pub mod keyboard;`
- `peri-tui/src/runtime/main_loop.rs` — 删除 `dispatch_fallback` 中的 keyboard 调用

- [ ] **Step 1: 逐文件删除 + 验证**

```bash
rm peri-tui/src/event/keyboard/setup_wizard.rs && cargo check -p peri-tui 2>&1 | head
rm peri-tui/src/event/keyboard/popups.rs && cargo check -p peri-tui 2>&1 | head
rm peri-tui/src/event/keyboard/normal_keys.rs && cargo check -p peri-tui 2>&1 | head
rm peri-tui/src/event/keyboard/bar_focus.rs && cargo check -p peri-tui 2>&1 | head
rm peri-tui/src/event/keyboard.rs && cargo check -p peri-tui 2>&1 | head
```

---

### Task 6b: 删除不用的文件

**删除**:
- `peri-tui/src/render/block_mode.rs` — P5 占位骨架
- `peri-tui/src/ui/main_ui/popups/hitl.rs`
- `peri-tui/src/ui/main_ui/popups/ask_user.rs`
- `peri-tui/src/ui/main_ui/popups/rewind.rs`
- `peri-tui/src/ui/main_ui/popups/oauth.rs`

**修改**:
- `peri-tui/src/render/mod.rs` — 删除 `pub mod block_mode;`
- `peri-tui/src/ui/main_ui/popups/mod.rs` — 删除对应声明
- `peri-tui/src/ui/main_ui/mod.rs` — 删除 popup 渲染调用

- [ ] **Step 1: 逐文件删除 + 验证**

```bash
rm peri-tui/src/render/block_mode.rs && cargo check -p peri-tui 2>&1 | head
rm peri-tui/src/ui/main_ui/popups/hitl.rs && cargo check -p peri-tui 2>&1 | head
rm peri-tui/src/ui/main_ui/popups/ask_user.rs && cargo check -p peri-tui 2>&1 | head
rm peri-tui/src/ui/main_ui/popups/rewind.rs && cargo check -p peri-tui 2>&1 | head
rm peri-tui/src/ui/main_ui/popups/oauth.rs && cargo check -p peri-tui 2>&1 | head
```

---

### Task 6c: 测试更新 + 文档更新 + 最终验证

- [ ] **测试**: `cargo test -p peri-tui --lib` 全部通过
- [ ] **Clippy**: `cargo clippy -p peri-tui --lib -- -D warnings` 零警告
- [ ] **文档**: 更新 `peri-tui/CLAUDE.md` 和根 `CLAUDE.md` 架构描述
- [ ] **完整构建**: `cargo build -p peri-tui` 零错误

---

## 验收标准

- [ ] `kit/message_area.rs` + `kit/input_area.rs` 创建且编译通过
- [ ] layout.rs 集成 MessageAreaWidget + TextareaWidget
- [ ] `event/keyboard/` 全目录物理删除
- [ ] `render/block_mode.rs` + `popups/*.rs` 物理删除
- [ ] `cargo build -p peri-tui` 零错误
- [ ] `cargo clippy -p peri-tui --lib -- -D warnings` 零警告
- [ ] `cargo test -p peri-tui --lib` 全部通过
- [ ] 手动冒烟测试：chat、流式消息、panel 打开/关闭、session 切换 全部正常
