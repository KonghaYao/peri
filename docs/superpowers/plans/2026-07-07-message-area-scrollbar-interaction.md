# Message Area Scrollbar Interaction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 给 Peri TUI 消息区滚动条增加 thumb 拖拽、▲/▼ 点击跳顶/跳底，以及只标记 Human message 位置的滚动条刻度。

**Architecture:** 交互能力放在 ratatui-kit `ScrollView`/`ScrollBars` 层，Peri 不自建滚动条；Peri 只负责从 `RenderCache` 计算 Human 消息视觉行位置并传给 `clean_scrollbars_with_markers()`。滚动条 marker 使用 `ScrollBars` 配置渲染，不改变 `ScrollViewState` 的滚动语义，也不绕过 `RENDER_CACHE`。

**Tech Stack:** Rust 2024, ratatui-kit 0.7.2 fork (`KonghaYao/ratatui-kit`), ratatui 0.30.1, Peri `peri-tui` ratatui-kit 单路径。

---

## Scope Check

这个工作跨两个代码库：

1. **ratatui-kit fork**：增加通用 ScrollView 滚动条交互与 marker 渲染能力。
2. **perihelion**：更新依赖并把消息区 Human message 位置传给 `ScrollBars`。

如果执行环境无法直接修改并推送 `KonghaYao/ratatui-kit` fork，则先完成 Task 1-4 在本地 fork 分支，推送后再执行 Task 5-8 更新 Peri 依赖。不要修改 `~/.cargo/git/checkouts/...` 中的缓存源码作为最终方案；缓存源码不可提交。

## File Structure

### ratatui-kit fork（独立仓库）

- Modify: `crates/ratatui-kit/src/components/scroll_view/state.rs`
  - 新增 `max_x_offset()` / `max_y_offset()` / `set_scroll_y_clamped()` / `set_scroll_x_clamped()` / `clamp_to_known_bounds()`。
  - 保持现有 `scroll_to_top()` / `scroll_to_bottom()` 行为兼容。
- Modify: `crates/ratatui-kit/src/components/scroll_view/scrollbars.rs`
  - 新增 `ScrollbarGeometry`、`ScrollbarHit`。
  - 计算垂直滚动条 track/thumb/button 几何信息。
  - 渲染 marker。
  - 提供 `ScrollbarGeometry::hit_test()` 和 `ScrollbarGeometry::offset_for_track_row()`，供事件处理复用。
- Modify: `crates/ratatui-kit/src/components/scroll_view/mod.rs`
  - `UseScrollImpl` 持有上次渲染出的 `ScrollbarGeometry` 和当前 drag 状态。
  - 在 `ScrollView` 的事件处理器中识别滚动条点击/拖拽。
- Test: `crates/ratatui-kit/src/components/scroll_view/state_test.rs`
- Test: `crates/ratatui-kit/src/components/scroll_view/scrollbars_test.rs`

### perihelion

- Modify: `peri-tui/Cargo.toml`
  - 将 `ratatui-kit` 指向包含滚动条交互的 fork branch/rev。
- Modify: `peri-tui/src/kit/render_bridge.rs`
  - `RenderCache` 新增 `human_marker_rows: Vec<u16>`。
  - `RenderedEntry` 新增 `is_human: bool`。
  - 在 rebuild 时基于 entry 起始视觉行收集 Human message marker。
- Modify: `peri-tui/src/kit/panel_registry.rs`
  - 保留 `clean_scrollbars()`，新增 `clean_scrollbars_with_markers(marker_rows: Vec<u16>)`。
- Modify: `peri-tui/src/kit/message_area.rs`
  - 从 `RENDER_CACHE` 读取 `human_marker_rows`，传入 `clean_scrollbars_with_markers()`。
  - 鼠标事件中避免滚动条列触发文本选区。
- Test: `peri-tui/src/kit/render_bridge.rs` 内现有 `#[cfg(test)] mod tests` 或新增同文件测试。
- Test: `peri-tui/src/kit/message_area.rs` 现有测试模块。

---

## Task 1: ratatui-kit ScrollViewState 增加可测试的定位方法

**Files:**
- Modify: `../ratatui-kit/crates/ratatui-kit/src/components/scroll_view/state.rs`
- Create: `../ratatui-kit/crates/ratatui-kit/src/components/scroll_view/state_test.rs`

- [ ] **Step 1: 创建 ratatui-kit fork 工作区**

Run:

```bash
cd /Users/konghayao/code/ai
if [ ! -d ratatui-kit ]; then
  git clone https://github.com/KonghaYao/ratatui-kit.git ratatui-kit
fi
cd ratatui-kit
git checkout main
git pull --ff-only
git checkout -b feat/scrollbar-interaction
```

Expected: 新分支 `feat/scrollbar-interaction` 创建成功。

- [ ] **Step 2: 写 failing test**

Create `../ratatui-kit/crates/ratatui-kit/src/components/scroll_view/state_test.rs`:

```rust
use super::ScrollViewState;
use ratatui::layout::{Position, Size};

#[test]
fn test_scroll_view_state_set_scroll_y_clamps_to_max_offset() {
    let mut state = ScrollViewState::with_offset(Position { x: 0, y: 0 });
    state.set_sizes_for_test(Size::new(20, 100), Size::new(20, 10));
    state.set_scroll_y_clamped(200);
    assert_eq!(state.offset().y, 90);
}

#[test]
fn test_scroll_view_state_set_scroll_y_allows_exact_position() {
    let mut state = ScrollViewState::with_offset(Position { x: 0, y: 0 });
    state.set_sizes_for_test(Size::new(20, 100), Size::new(20, 10));
    state.set_scroll_y_clamped(45);
    assert_eq!(state.offset().y, 45);
}

#[test]
fn test_scroll_view_state_scrollbar_top_bottom_helpers() {
    let mut state = ScrollViewState::with_offset(Position { x: 0, y: 25 });
    state.set_sizes_for_test(Size::new(20, 100), Size::new(20, 10));
    assert_eq!(state.max_y_offset(), 90);
    state.scroll_to_top();
    assert_eq!(state.offset().y, 0);
    state.scroll_to_bottom();
    state.clamp_to_known_bounds();
    assert_eq!(state.offset().y, 90);
}
```

Append this module declaration to the bottom of `state.rs`:

```rust
#[cfg(test)]
#[path = "state_test.rs"]
mod tests;
```

- [ ] **Step 3: Run test to verify it fails**

Run:

```bash
cd /Users/konghayao/code/ai/ratatui-kit
cargo test -p ratatui-kit scroll_view::state -- --nocapture
```

Expected: FAIL because `set_sizes_for_test`, `set_scroll_y_clamped`, `max_y_offset`, and `clamp_to_known_bounds` do not exist.

- [ ] **Step 4: Implement minimal state helpers**

Modify `../ratatui-kit/crates/ratatui-kit/src/components/scroll_view/state.rs` inside `impl ScrollViewState`:

```rust
    pub fn max_x_offset(&self) -> u16 {
        match (self.size, self.page_size) {
            (Some(size), Some(page_size)) => size.width.saturating_sub(page_size.width),
            _ => u16::MAX,
        }
    }

    pub fn max_y_offset(&self) -> u16 {
        match (self.size, self.page_size) {
            (Some(size), Some(page_size)) => size.height.saturating_sub(page_size.height),
            _ => u16::MAX,
        }
    }

    pub fn set_scroll_y_clamped(&mut self, y: u16) {
        self.offset.y = y.min(self.max_y_offset());
    }

    pub fn set_scroll_x_clamped(&mut self, x: u16) {
        self.offset.x = x.min(self.max_x_offset());
    }

    pub fn clamp_to_known_bounds(&mut self) {
        self.offset.x = self.offset.x.min(self.max_x_offset());
        self.offset.y = self.offset.y.min(self.max_y_offset());
    }

    #[cfg(test)]
    pub(crate) fn set_sizes_for_test(&mut self, size: Size, page_size: Size) {
        self.size = Some(size);
        self.page_size = Some(page_size);
    }
```

- [ ] **Step 5: Run test to verify it passes**

Run:

```bash
cd /Users/konghayao/code/ai/ratatui-kit
cargo test -p ratatui-kit scroll_view::state -- --nocapture
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
cd /Users/konghayao/code/ai/ratatui-kit
git add crates/ratatui-kit/src/components/scroll_view/state.rs crates/ratatui-kit/src/components/scroll_view/state_test.rs
git commit -m "feat(scrollview): add clamped scroll positioning helpers"
```

---

## Task 2: ratatui-kit 增加垂直滚动条几何与命中测试

**Files:**
- Modify: `../ratatui-kit/crates/ratatui-kit/src/components/scroll_view/scrollbars.rs`
- Create: `../ratatui-kit/crates/ratatui-kit/src/components/scroll_view/scrollbars_test.rs`

- [ ] **Step 1: Write failing geometry tests**

Create `../ratatui-kit/crates/ratatui-kit/src/components/scroll_view/scrollbars_test.rs`:

```rust
use super::scrollbars::{ScrollBars, ScrollbarHit};
use ratatui::layout::{Rect, Size};

#[test]
fn test_vertical_geometry_includes_buttons_track_and_thumb() {
    let bars = ScrollBars::default();
    let area = Rect::new(10, 5, 1, 20);
    let geometry = bars.vertical_geometry(area, Size::new(80, 100), 30).unwrap();
    assert_eq!(geometry.bar_area, area);
    assert_eq!(geometry.begin_button, Some(Rect::new(10, 5, 1, 1)));
    assert_eq!(geometry.end_button, Some(Rect::new(10, 24, 1, 1)));
    assert_eq!(geometry.track_area, Rect::new(10, 6, 1, 18));
    assert!(geometry.thumb_area.y >= geometry.track_area.y);
    assert!(geometry.thumb_area.bottom() <= geometry.track_area.bottom());
    assert_eq!(geometry.max_offset, 80);
}

#[test]
fn test_vertical_hit_test_distinguishes_buttons_thumb_and_track() {
    let bars = ScrollBars::default();
    let area = Rect::new(10, 5, 1, 20);
    let geometry = bars.vertical_geometry(area, Size::new(80, 100), 30).unwrap();
    assert_eq!(geometry.hit_test(10, 5), ScrollbarHit::BeginButton);
    assert_eq!(geometry.hit_test(10, 24), ScrollbarHit::EndButton);
    assert_eq!(geometry.hit_test(10, geometry.thumb_area.y), ScrollbarHit::Thumb);
    assert_eq!(geometry.hit_test(9, geometry.thumb_area.y), ScrollbarHit::Outside);
}

#[test]
fn test_vertical_track_row_maps_to_scroll_offset() {
    let bars = ScrollBars::default();
    let area = Rect::new(10, 5, 1, 20);
    let geometry = bars.vertical_geometry(area, Size::new(80, 100), 0).unwrap();
    assert_eq!(geometry.offset_for_track_row(geometry.track_area.y), 0);
    assert_eq!(geometry.offset_for_track_row(geometry.track_area.bottom().saturating_sub(1)), 80);
}
```

Append this module declaration to the bottom of `scrollbars.rs`:

```rust
#[cfg(test)]
#[path = "scrollbars_test.rs"]
mod tests;
```

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
cd /Users/konghayao/code/ai/ratatui-kit
cargo test -p ratatui-kit scroll_view::scrollbars -- --nocapture
```

Expected: FAIL because `ScrollbarHit`, `vertical_geometry`, and related structs do not exist.

- [ ] **Step 3: Implement geometry structs and helpers**

Modify `../ratatui-kit/crates/ratatui-kit/src/components/scroll_view/scrollbars.rs` near the existing `ScrollbarLayout` definition:

```rust
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum ScrollbarHit {
    Outside,
    BeginButton,
    EndButton,
    Thumb,
    Track,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub struct ScrollbarGeometry {
    pub bar_area: Rect,
    pub track_area: Rect,
    pub thumb_area: Rect,
    pub begin_button: Option<Rect>,
    pub end_button: Option<Rect>,
    pub max_offset: u16,
}

impl ScrollbarGeometry {
    pub fn hit_test(&self, column: u16, row: u16) -> ScrollbarHit {
        let point = Rect::new(column, row, 1, 1);
        if self.begin_button.is_some_and(|area| area.intersects(point)) {
            return ScrollbarHit::BeginButton;
        }
        if self.end_button.is_some_and(|area| area.intersects(point)) {
            return ScrollbarHit::EndButton;
        }
        if self.thumb_area.intersects(point) {
            return ScrollbarHit::Thumb;
        }
        if self.track_area.intersects(point) {
            return ScrollbarHit::Track;
        }
        ScrollbarHit::Outside
    }

    pub fn offset_for_track_row(&self, row: u16) -> u16 {
        if self.max_offset == 0 || self.track_area.height <= self.thumb_area.height {
            return 0;
        }
        let relative = row.saturating_sub(self.track_area.y);
        let travel = self.track_area.height.saturating_sub(self.thumb_area.height).max(1);
        let clamped = relative.min(travel);
        ((u32::from(clamped) * u32::from(self.max_offset)) / u32::from(travel)) as u16
    }
}
```

Add this public helper inside `impl ScrollBars<'_>`:

```rust
    pub fn vertical_geometry(
        &self,
        area: Rect,
        scroll_size: Size,
        y_offset: u16,
    ) -> Option<ScrollbarGeometry> {
        let max_offset = scroll_size.height.saturating_sub(area.height);
        if max_offset == 0 || area.height == 0 || area.width == 0 {
            return None;
        }

        let begin_button = Some(Rect::new(area.x, area.y, 1, 1));
        let end_button = Some(Rect::new(area.x, area.bottom().saturating_sub(1), 1, 1));
        let track_y = area.y.saturating_add(1);
        let track_height = area.height.saturating_sub(2);
        let track_area = Rect::new(area.x, track_y, 1, track_height);
        if track_height == 0 {
            return None;
        }

        let thumb_height = ((u32::from(track_height) * u32::from(area.height))
            / u32::from(scroll_size.height.max(1)))
            .max(1) as u16;
        let thumb_height = thumb_height.min(track_height);
        let travel = track_height.saturating_sub(thumb_height);
        let thumb_offset = if travel == 0 {
            0
        } else {
            ((u32::from(travel) * u32::from(y_offset.min(max_offset)))
                / u32::from(max_offset)) as u16
        };
        let thumb_area = Rect::new(area.x, track_y.saturating_add(thumb_offset), 1, thumb_height);

        Some(ScrollbarGeometry {
            bar_area: area,
            track_area,
            thumb_area,
            begin_button,
            end_button,
            max_offset,
        })
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run:

```bash
cd /Users/konghayao/code/ai/ratatui-kit
cargo test -p ratatui-kit scroll_view::scrollbars -- --nocapture
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cd /Users/konghayao/code/ai/ratatui-kit
git add crates/ratatui-kit/src/components/scroll_view/scrollbars.rs crates/ratatui-kit/src/components/scroll_view/scrollbars_test.rs
git commit -m "feat(scrollview): expose vertical scrollbar geometry"
```

---

## Task 3: ratatui-kit ScrollView 处理箭头点击与 thumb 拖拽

**Files:**
- Modify: `../ratatui-kit/crates/ratatui-kit/src/components/scroll_view/mod.rs`
- Modify: `../ratatui-kit/crates/ratatui-kit/src/components/scroll_view/scrollbars.rs`

- [ ] **Step 1: Write failing unit test for drag math**

Append to `scrollbars_test.rs`:

```rust
#[test]
fn test_drag_row_maps_middle_track_to_middle_offset() {
    let bars = ScrollBars::default();
    let geometry = bars.vertical_geometry(Rect::new(0, 0, 1, 22), Size::new(80, 122), 0).unwrap();
    let middle_row = geometry.track_area.y + geometry.track_area.height / 2;
    let offset = geometry.offset_for_track_row(middle_row);
    assert!(offset >= 45 && offset <= 55, "middle offset should be near half, got {offset}");
}
```

- [ ] **Step 2: Run test to verify drag math passes before wiring**

Run:

```bash
cd /Users/konghayao/code/ai/ratatui-kit
cargo test -p ratatui-kit test_drag_row_maps_middle_track_to_middle_offset -- --nocapture
```

Expected: PASS. This confirms geometry math is ready before event wiring.

- [ ] **Step 3: Add drag state type**

Modify `../ratatui-kit/crates/ratatui-kit/src/components/scroll_view/mod.rs` imports:

```rust
use crossterm::event::{Event, MouseButton, MouseEventKind};
use scrollbars::{ScrollbarGeometry, ScrollbarHit};
```

Add near `UseScrollImpl`:

```rust
#[derive(Debug, Clone, Copy, Default)]
struct ScrollbarDragState {
    active: bool,
}
```

Update `UseScrollImpl`:

```rust
pub struct UseScrollImpl {
    scroll_view_state: State<ScrollViewState>,
    scrollbars: ScrollBars<'static>,
    area: Option<ratatui::layout::Rect>,
    vertical_geometry: Option<ScrollbarGeometry>,
    drag_state: ScrollbarDragState,
    has_block: bool,
}
```

Update hook initialization:

```rust
let hook = hooks.use_hook(|| UseScrollImpl {
    scroll_view_state: props.scroll_view_state.unwrap_or(this_scroll_view_state),
    scrollbars: props.scroll_bars.clone(),
    area: None,
    vertical_geometry: None,
    drag_state: ScrollbarDragState::default(),
    has_block: props.block.is_some(),
});
```

- [ ] **Step 4: Store vertical geometry after draw**

In `UseScrollImpl::post_component_draw`, before `self.scrollbars.render_ref(...)`, compute and store geometry:

```rust
let area = self.area.unwrap_or_default();
self.vertical_geometry = self.scrollbars.vertical_geometry(
    Rect { height: area.height, ..area },
    buffer.area.as_size(),
    self.scroll_view_state.read().offset().y,
);
```

Then keep the existing `render_ref` call.

- [ ] **Step 5: Route mouse events to scrollbar interaction**

Add this method to `impl UseScrollImpl`:

```rust
impl UseScrollImpl {
    fn handle_scrollbar_mouse(&mut self, event: &Event) -> EventResult {
        let Event::Mouse(mouse) = event else {
            return EventResult::Ignored;
        };
        let Some(geometry) = self.vertical_geometry else {
            return EventResult::Ignored;
        };

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => match geometry.hit_test(mouse.column, mouse.row) {
                ScrollbarHit::BeginButton => {
                    self.scroll_view_state.write().scroll_to_top();
                    EventResult::Consumed
                }
                ScrollbarHit::EndButton => {
                    self.scroll_view_state.write().scroll_to_bottom();
                    EventResult::Consumed
                }
                ScrollbarHit::Thumb | ScrollbarHit::Track => {
                    self.drag_state.active = true;
                    let y = geometry.offset_for_track_row(mouse.row);
                    self.scroll_view_state.write().set_scroll_y_clamped(y);
                    EventResult::Consumed
                }
                ScrollbarHit::Outside => EventResult::Ignored,
            },
            MouseEventKind::Drag(MouseButton::Left) if self.drag_state.active => {
                let y = geometry.offset_for_track_row(mouse.row);
                self.scroll_view_state.write().set_scroll_y_clamped(y);
                EventResult::Consumed
            }
            MouseEventKind::Up(MouseButton::Left) if self.drag_state.active => {
                self.drag_state.active = false;
                EventResult::Consumed
            }
            _ => EventResult::Ignored,
        }
    }
}
```

Inside `ScrollView::update`, after `hook.scrollbars = props.scroll_bars.clone();`, register a high-priority current event handler:

```rust
hooks.use_event_handler_with_options(
    EventScope::Current,
    EventPriority::High,
    EventOptions { hit_test: true },
    {
        let hook = hook;
        move |event| {
            if disabled {
                return EventResult::Ignored;
            }
            hook.handle_scrollbar_mouse(&event)
        }
    },
);
```

Keep the existing Normal priority scroll handler unchanged so wheel/key behavior continues to work.

- [ ] **Step 6: Run ratatui-kit checks**

Run:

```bash
cd /Users/konghayao/code/ai/ratatui-kit
cargo test -p ratatui-kit scroll_view -- --nocapture
cargo check -p ratatui-kit --features full
```

Expected: both PASS.

- [ ] **Step 7: Commit**

```bash
cd /Users/konghayao/code/ai/ratatui-kit
git add crates/ratatui-kit/src/components/scroll_view/mod.rs crates/ratatui-kit/src/components/scroll_view/scrollbars.rs crates/ratatui-kit/src/components/scroll_view/scrollbars_test.rs
git commit -m "feat(scrollview): support scrollbar click and drag"
```

---

## Task 4: ratatui-kit ScrollBars 支持 marker 渲染

**Files:**
- Modify: `../ratatui-kit/crates/ratatui-kit/src/components/scroll_view/scrollbars.rs`
- Modify: `../ratatui-kit/crates/ratatui-kit/src/components/scroll_view/scrollbars_test.rs`

- [ ] **Step 1: Write failing marker tests**

Append to `scrollbars_test.rs`:

```rust
use ratatui::style::{Color, Style};

#[test]
fn test_marker_row_for_offset_maps_to_track() {
    let bars = ScrollBars::default().with_vertical_markers(vec![0, 40, 80], Style::default().fg(Color::Yellow));
    let geometry = bars.vertical_geometry(Rect::new(10, 5, 1, 20), Size::new(80, 100), 0).unwrap();
    let rows = bars.vertical_marker_rows(&geometry);
    assert_eq!(rows.first().copied(), Some(geometry.track_area.y));
    assert_eq!(rows.last().copied(), Some(geometry.track_area.bottom().saturating_sub(1)));
    assert_eq!(rows.len(), 3);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
cd /Users/konghayao/code/ai/ratatui-kit
cargo test -p ratatui-kit test_marker_row_for_offset_maps_to_track -- --nocapture
```

Expected: FAIL because marker APIs do not exist.

- [ ] **Step 3: Add marker config**

Modify `ScrollBars<'a>` struct:

```rust
    pub vertical_markers: Vec<u16>,
    pub vertical_marker_style: ratatui::style::Style,
```

Update `Default`:

```rust
            vertical_markers: Vec::new(),
            vertical_marker_style: ratatui::style::Style::default(),
```

Add methods to `impl ScrollBars<'_>`:

```rust
    pub fn with_vertical_markers(mut self, markers: Vec<u16>, style: ratatui::style::Style) -> Self {
        self.vertical_markers = markers;
        self.vertical_marker_style = style;
        self
    }

    pub fn vertical_marker_rows(&self, geometry: &ScrollbarGeometry) -> Vec<u16> {
        if geometry.max_offset == 0 || geometry.track_area.height == 0 {
            return Vec::new();
        }
        let travel = geometry.track_area.height.saturating_sub(1).max(1);
        self.vertical_markers
            .iter()
            .map(|offset| {
                let clamped = (*offset).min(geometry.max_offset);
                let row_offset = (u32::from(travel) * u32::from(clamped) / u32::from(geometry.max_offset)) as u16;
                geometry.track_area.y.saturating_add(row_offset)
            })
            .collect()
    }
```

- [ ] **Step 4: Render markers after scrollbar**

In `render_vertical_scrollbar`, after rendering the ratatui scrollbar widget, add:

```rust
        if let Some(geometry) = self.vertical_geometry(area, scroll_size, state.offset.y) {
            for row in self.vertical_marker_rows(&geometry) {
                if let Some(cell) = buf.cell_mut((area.x, row)) {
                    cell.set_symbol("▪");
                    cell.set_style(self.vertical_marker_style);
                }
            }
        }
```

- [ ] **Step 5: Run tests and check**

Run:

```bash
cd /Users/konghayao/code/ai/ratatui-kit
cargo test -p ratatui-kit scroll_view::scrollbars -- --nocapture
cargo check -p ratatui-kit --features full
```

Expected: PASS.

- [ ] **Step 6: Commit and push fork branch**

```bash
cd /Users/konghayao/code/ai/ratatui-kit
git add crates/ratatui-kit/src/components/scroll_view/scrollbars.rs crates/ratatui-kit/src/components/scroll_view/scrollbars_test.rs
git commit -m "feat(scrollview): render vertical scrollbar markers"
git push -u origin feat/scrollbar-interaction
```

Expected: branch exists on `KonghaYao/ratatui-kit` and can be referenced from Peri.

---

## Task 5: Peri 更新 ratatui-kit dependency 到交互分支

**Files:**
- Modify: `peri-tui/Cargo.toml:31`
- Modify: `Cargo.lock`
- Modify: `peri-tui/Cargo.lock`

- [ ] **Step 1: Update dependency branch**

Modify `peri-tui/Cargo.toml` line 31:

```toml
ratatui-kit = { git = "https://github.com/KonghaYao/ratatui-kit.git", branch = "feat/scrollbar-interaction", features = ["full"] }
```

- [ ] **Step 2: Update lockfiles**

Run:

```bash
cd /Users/konghayao/code/ai/perihelion
cargo update -p ratatui-kit
cargo check -p peri-tui --lib
```

Expected: `Cargo.lock` and `peri-tui/Cargo.lock` update ratatui-kit git rev; `cargo check` passes or only fails on downstream API usage not yet wired. If it fails because branch is missing, stop and push Task 4 branch first.

- [ ] **Step 3: Commit**

```bash
cd /Users/konghayao/code/ai/perihelion
git add peri-tui/Cargo.toml Cargo.lock peri-tui/Cargo.lock
git commit -m "chore(tui): use scrollbar interaction ratatui-kit branch"
```

---

## Task 6: Peri RenderCache 记录 Human message 视觉行位置

**Files:**
- Modify: `peri-tui/src/kit/render_bridge.rs`

- [ ] **Step 1: Write failing tests**

Add to bottom of `render_bridge.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use peri_acp_types::view_model::{hash_str, UserBubbleData, ViewModel, AssistantBubbleData};

    fn user(text: &str) -> ViewModel {
        ViewModel::UserBubble(UserBubbleData {
            text: text.to_string(),
            content_hash: hash_str(text),
            is_system_reminder: false,
        })
    }

    fn assistant(text: &str) -> ViewModel {
        ViewModel::AssistantBubble(AssistantBubbleData {
            text: text.to_string(),
            reasoning: None,
            tool_card_ids: Vec::new(),
            content_hash: hash_str(text),
        })
    }

    #[test]
    fn test_is_human_vm_only_matches_user_bubble() {
        assert!(is_human_vm(&user("hello")));
        assert!(!is_human_vm(&assistant("world")));
    }

    #[test]
    fn test_rebuild_human_marker_rows_uses_entry_start_rows() {
        let entries = vec![
            (VmKey::Committed(0), RenderedEntry { height: 2, lines: Arc::from(vec![Line::from("u")]), is_human: true }),
            (VmKey::Committed(1), RenderedEntry { height: 3, lines: Arc::from(vec![Line::from("a")]), is_human: false }),
            (VmKey::Committed(2), RenderedEntry { height: 4, lines: Arc::from(vec![Line::from("u2")]), is_human: true }),
        ];
        assert_eq!(human_marker_rows_for_entries(&entries), vec![0, 5]);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
cd /Users/konghayao/code/ai/perihelion
cargo test -p peri-tui --lib render_bridge -- --nocapture
```

Expected: FAIL because `RenderedEntry::is_human`, `is_human_vm`, and `human_marker_rows_for_entries` do not exist.

- [ ] **Step 3: Add data fields and helpers**

Modify `RenderedEntry`:

```rust
#[derive(Debug, Clone)]
pub struct RenderedEntry {
    pub height: usize,
    pub lines: Arc<[Line<'static>]>,
    pub is_human: bool,
}
```

Modify `RenderCache`:

```rust
    /// Human/UserBubble 消息在完整视觉内容中的起始行，用于滚动条 marker。
    pub human_marker_rows: Vec<u16>,
```

Add helpers near `extract_hashes`:

```rust
fn is_human_vm(vm: &peri_acp_types::view_model::ViewModel) -> bool {
    matches!(vm, peri_acp_types::view_model::ViewModel::UserBubble(_))
}

fn human_marker_rows_for_entries(entries: &[(VmKey, RenderedEntry)]) -> Vec<u16> {
    let mut rows = Vec::new();
    let mut current: u16 = 0;
    for (_key, entry) in entries {
        if entry.is_human {
            rows.push(current);
        }
        current = current.saturating_add(entry.height as u16);
    }
    rows
}

fn rebuild_human_marker_rows(cache: &mut RenderCache) {
    cache.human_marker_rows = human_marker_rows_for_entries(&cache.entries);
}
```

- [ ] **Step 4: Populate `is_human` in append_entries**

Modify `append_entries` when pushing `RenderedEntry`:

```rust
        entries.push((
            key,
            RenderedEntry {
                height,
                lines: Arc::from(lines),
                is_human: is_human_vm(vm),
            },
        ));
```

- [ ] **Step 5: Rebuild marker rows everywhere cache is rebuilt**

After each existing `rebuild_cumulative_heights(&mut cache);`, add:

```rust
rebuild_human_marker_rows(&mut cache);
```

Required locations:

1. Main event loop before building `all_lines` for wrap map.
2. `rebuild_all()` after `rebuild_cumulative_heights(cache);`.

- [ ] **Step 6: Run tests**

Run:

```bash
cd /Users/konghayao/code/ai/perihelion
cargo test -p peri-tui --lib render_bridge -- --nocapture
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
cd /Users/konghayao/code/ai/perihelion
git add peri-tui/src/kit/render_bridge.rs
git commit -m "feat(tui): track human message rows for scrollbar markers"
```

---

## Task 7: Peri 将 Human markers 接入 message_area 滚动条

**Files:**
- Modify: `peri-tui/src/kit/panel_registry.rs:486-508`
- Modify: `peri-tui/src/kit/message_area.rs:251-604`

- [ ] **Step 1: Add marker-aware scrollbar factory**

Modify `panel_registry.rs` by replacing `clean_scrollbars()` with a wrapper plus new function:

```rust
pub fn clean_scrollbars() -> ScrollBars<'static> {
    clean_scrollbars_with_markers(Vec::new())
}

pub fn clean_scrollbars_with_markers(marker_rows: Vec<u16>) -> ScrollBars<'static> {
    let semantic = crate::kit::theme::semantic();
    let thumb_bg = semantic.text.dim;
    ScrollBars {
        vertical_scrollbar: Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .thumb_symbol(" ")
            .thumb_style(Style::default().bg(thumb_bg))
            .track_symbol(None)
            .begin_symbol(Some("▲"))
            .begin_style(
                Style::default()
                    .fg(semantic.text.muted)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            )
            .end_symbol(Some("▼"))
            .end_style(
                Style::default()
                    .fg(semantic.text.muted)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            ),
        vertical_scrollbar_visibility: ScrollbarVisibility::Automatic,
        ..ScrollBars::default()
    }
    .with_vertical_markers(marker_rows, Style::default().fg(semantic.accent))
}
```

- [ ] **Step 2: Update message_area import**

Modify `message_area.rs` import:

```rust
use crate::kit::panel_registry::{clean_scrollbars, clean_scrollbars_with_markers};
```

- [ ] **Step 3: Preserve marker rows before dropping cache snapshot**

In `MessageArea`, before `drop(cache_snapshot);`, add:

```rust
    let human_marker_rows = cache_snapshot.human_marker_rows.clone();
```

The surrounding block becomes:

```rust
    let mut all_lines: Vec<Line<'static>> = cache_snapshot
        .entries
        .iter()
        .flat_map(|(_, entry)| entry.lines.iter().cloned())
        .collect();
    all_lines.extend(footer_lines);
    let empty = cache_snapshot.entries.is_empty() && !is_loading && todo_items.is_empty();
    let human_marker_rows = cache_snapshot.human_marker_rows.clone();
    let _all_line_count = all_lines.len();
    let content_lines = Arc::new(all_lines.clone());
    drop(cache_snapshot);
```

- [ ] **Step 4: Use marker-aware scrollbars**

Modify ScrollView props:

```rust
scroll_bars: clean_scrollbars_with_markers(human_marker_rows),
```

Do not change existing `clean_scrollbars()` callers in panels; only message area needs Human markers.

- [ ] **Step 5: Prevent scrollbar column from starting text selection**

Inside the mouse handler, before computing `visual_row`, add:

```rust
                        let scrollbar_col = area.x.saturating_add(area.width.saturating_sub(1));
                        if mouse.column == scrollbar_col {
                            return EventResult::Ignored;
                        }
```

This lets ratatui-kit `ScrollView`'s Current-scope high-priority handler consume scrollbar clicks/drags instead of message_area treating them as text selection.

- [ ] **Step 6: Run targeted tests/check**

Run:

```bash
cd /Users/konghayao/code/ai/perihelion
cargo test -p peri-tui --lib message_area -- --nocapture
cargo test -p peri-tui --lib render_bridge -- --nocapture
cargo check -p peri-tui --lib
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
cd /Users/konghayao/code/ai/perihelion
git add peri-tui/src/kit/panel_registry.rs peri-tui/src/kit/message_area.rs
git commit -m "feat(tui): show human message markers on message scrollbar"
```

---

## Task 8: Manual verification and regression checks

**Files:**
- Modify: `docs/verify-tui.md:20-23`

- [ ] **Step 1: Run full relevant checks**

Run:

```bash
cd /Users/konghayao/code/ai/perihelion
cargo test -p peri-tui --lib -- --nocapture
cargo check -p peri-tui --lib
```

Expected: PASS.

- [ ] **Step 2: Manual TUI verification**

Run:

```bash
cd /Users/konghayao/code/ai/perihelion
cargo run -p peri-tui -- -a
```

Manual steps:

1. 发送 5-10 轮消息，确保消息区超过一屏。
2. 观察右侧滚动条：应显示 ▲、▼、thumb，以及若干 Human message marker（`▪` 或配置的 marker 符号）。
3. 鼠标拖拽 thumb：消息内容应跟随滚动。
4. 点击 ▲：消息区跳到顶部。
5. 点击 ▼：消息区跳到底部。
6. 在消息内容区域拖拽文本：文本复制功能仍可用。
7. 在滚动条列拖拽：不应启动文本选区。
8. 使用滚轮和 Ctrl+U/D/Home/End：原有滚动行为仍可用。

- [ ] **Step 3: Update verification checklist**

Modify `docs/verify-tui.md` line 23 from:

```markdown
- [ ] 滚动条样式， 拖拽，及上下点的点击快速跳转
```

to:

```markdown
- [x] 滚动条样式，拖拽，上下点点击快速跳转，Human 消息位置刻度
```

- [ ] **Step 4: Commit verification doc**

```bash
cd /Users/konghayao/code/ai/perihelion
git add docs/verify-tui.md
git commit -m "docs(tui): mark scrollbar interaction verified"
```

---

## Final verification before completion

Run:

```bash
cd /Users/konghayao/code/ai/perihelion
lefthook run pre-commit
```

Expected: PASS. If `lefthook` is unavailable, run:

```bash
cargo fmt --all -- --check
cargo check --workspace
cargo test -p peri-tui --lib -- --nocapture
```

Expected: PASS.

---

## Self-Review

### Spec coverage

- Scrollbar drag → Task 1-3 implement ScrollViewState positioning, geometry hit testing, and mouse drag handling.
- ▲ click to top and ▼ click to bottom → Task 3 handles `BeginButton` / `EndButton` with `scroll_to_top()` / `scroll_to_bottom()`.
- Human message approximate positions → Task 4 adds marker rendering, Task 6 computes Human message visual row starts, Task 7 passes markers from Peri message area.
- ratatui-kit layer requirement → Task 1-4 implement generic framework support; Peri only passes data/config.
- Existing text selection should not regress → Task 7 prevents scrollbar column from becoming a message selection start; Task 8 manual verification includes copy regression.

### Placeholder scan

No `TBD`, `TODO`, `implement later`, or vague “add tests” steps remain. Every code-changing step includes exact code or exact replacement text.

### Type consistency

- `ScrollViewState::set_scroll_y_clamped`, `max_y_offset`, and `clamp_to_known_bounds` are introduced in Task 1 and used in Task 3.
- `ScrollbarGeometry`, `ScrollbarHit`, `vertical_geometry`, and `offset_for_track_row` are introduced in Task 2 and used in Task 3.
- `with_vertical_markers` and `vertical_marker_rows` are introduced in Task 4 and used in Task 7.
- `RenderCache::human_marker_rows`, `RenderedEntry::is_human`, `is_human_vm`, and `human_marker_rows_for_entries` are introduced in Task 6 and consumed in Task 7.
