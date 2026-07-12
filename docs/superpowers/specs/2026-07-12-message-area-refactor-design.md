# message_area.rs 重构设计：按职责横向拆分

**日期**：2026-07-12
**状态**：Draft
**作者**：Claude (brainstorming skill)
**目标文件**：`peri-tui/src/kit/message_area.rs`（当前 1782 行）

## 背景

`message_area.rs` 是 TUI 中最大、最敏感的文件之一——CLAUDE.md 中多条 TRAP 警告都直接指向它（hook 顺序敏感、render body 禁止写 atom、`use_state` write vs `write_no_update` 自激回路、鼠标滚轮必须节流、u16 saturating 运算、剪贴板独立线程、parking_lot 同 thread 死锁、Arc<Vec> vs Vec 性能）。

经过若干次架构演变（特别是 `3bfb9fff` 删除 `render_bridge`/`bubbles`/`view_render` 三层管线，以及最近的视口裁剪改造），原本分布在多文件的逻辑全部塞回 `message_area.rs` 单文件，导致：

- 单文件 1782 行混合 7+ 职责：ViewModel 渲染 / 文本选区复制 / 滚动节流 / 自定义滚动条 / 视口裁剪 / Todo / Footer / 鼠标事件 / 智能吸底 / 剪贴板
- 主 `MessageArea` 组件函数本身 ~900 行（887-1782），其中嵌套 ~240 行鼠标事件处理器和 ~70 行 `extract_visual_range` 嵌套函数
- 4 个独立 use_state 缓存（`lines_cache`/`total_rows_cache`/`wrap_map_cache` + 滚动相关 state）模式重复，分散在 900 行代码中难以一致维护
- 多个具有相同形态的 trap（Arc<Vec>、write_no_update、parking_lot 死锁）分散在事件处理器各处
- 新功能无处下手

## 目标

1. **可维护性**：将 1782 行单文件拆为 6 个职责清晰的子模块，每个 80–500 行
2. **行为不变**：100% 纯重组——`cargo test` 与手动行为对比必须 100% 通过
3. **零外部破坏**：5 处外部模块对 `crate::kit::message_area::TodoItem`/`TodoStatus` 的引用路径保持不变
4. **不引入新概念**：不为想象中的需求设计 trait 或扩展点（YAGNI）

## 非目标

- **不修 bug**：history-replay-scroll-too-early（Open）不在本次范围内
- **不优化性能**：不动 Arc<Vec> 缓存策略、视口裁剪算法、节流窗口
- **不清理代码异味**：`render_*_lines` 中重复的 `THEME_ATOM.state().read()` 保留原状；测试 `proximity_check` 与生产代码 1/2 vs 1/4 偏差保留
- **不为特定新功能加扩展点**
- **不修改 DEBUG 环境变量**：`PERI_DISABLE_DRAG_SELECT` / `PERI_NO_HIGHLIGHT` 原样保留

## 设计

### §1 模块边界与内容归属

把 `peri-tui/src/kit/message_area.rs` 重组为 `peri-tui/src/kit/message_area/` 子目录，6 个文件：

```
peri-tui/src/kit/message_area/
├── mod.rs        (~320 行) MessageArea 组件本体：hooks 装配 + 视口裁剪渲染骨架
├── render.rs     (~450 行) vm_to_lines + 9 个变体渲染函数 + 摘要辅助
├── selection.rs  (~270 行) wrap_map/viewport_logical_range/highlight/extract/copy_to_clipboard
├── scroll.rs     (~280 行) ScrollThrottle/DragThrottle + 鼠标事件 + use_effect 吸底
├── footer.rs     (~200 行) build_footer_lines + TodoItem + render_todo_lines
└── props.rs      (~130 行) MessageAreaProps + MsgAreaTracker + ScrollbarFields/Hook + mouse_in_area
```

#### 详细归属表

| 新文件 | 内容 | 当前来源行 |
|--------|------|-----------|
| **`mod.rs`** | `MessageArea` 组件本体——所有 `hooks.use_*` 调用、`use_event_handler` 薄壳（委托 `scroll::handle_event`）、`use_effect` 薄壳（委托 `scroll::run_auto_follow`）、empty 分支、视口裁剪渲染骨架（viewport_lines 构建 + Paragraph::scroll）、scrollbar_fields 更新 | 887–1782（保留组件函数） |
| **`render.rs`** | `vm_to_lines` + 9 个变体函数（`render_reasoning_block`/`render_tool_card_lines`/`render_system_note_lines`/`render_subagent_group_lines`/`render_collapsed_group_lines`/`render_divider_lines`/`render_ask_user_block_lines`/`render_reminder_condensed`） + 摘要辅助（`COLLAPSED_BY_DEFAULT`/`AUTO_EXPAND`/`FORCE_EXPAND_ON_COMPLETE`/`compact_summary`/`compact_output_lines`/`format_running_duration`/`diff_change_summary`/`truncate_str`） + 间距辅助（`with_message_spacing`/`trim_trailing_blank_lines`） | 411–984 |
| **`selection.rs`** | `WrappedLineInfo` + `build_wrap_map` + `visual_to_logical` + **`viewport_logical_range`**（视口→逻辑行范围+首行偏移） + `highlight_logical_range` + `extract_visual_range`（从闭包内提升为模块级函数，参数化 `vis_width`） + `copy_to_clipboard` + `mark_copy_message` | 82–173, 1233–1305 |
| **`scroll.rs`** | `ScrollThrottle` + `DragThrottle` + `SCROLL_LINES`/`SCROLL_FRAME_MS` 常量 + `run_auto_follow`（吸底 effect 逻辑提为函数） + `handle_event`（鼠标+键盘事件处理器提为函数，含 `PERI_DISABLE_DRAG_SELECT` 分支） + `apply_scroll`（内嵌闭包提为模块级私有函数） | 41–80, 1052–1335 |
| **`footer.rs`** | `TodoStatus`/`TodoItem`/`hash_todo_items`/`render_todo_lines` + `build_footer_lines` | 175–225, 1490–1578 |
| **`props.rs`** | `MessageAreaProps` + `MsgAreaTracker` + **`ScrollbarFields`** + **`ScrollbarHook`（含 `impl Hook` 的 `post_component_draw` 渲染逻辑）** + `mouse_in_area` | 227–305, 387–410 |

### §2 跨模块共享类型与可见性

为保证**外部调用路径 100% 不变**（5 个外部文件不改），用 `pub use` 重导出关键类型。

#### `message_area/mod.rs` 的对外接口

```rust
// 对外暴露（保持现有路径 `crate::kit::message_area::X` 可用）
pub use footer::{TodoItem, TodoStatus};
pub use props::MessageAreaProps;

// MessageArea 组件本体仍在 mod.rs 定义
#[component]
pub fn MessageArea(props: &MessageAreaProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> { ... }

// 内部模块（不对外暴露，子模块间通过 super:: 相互引用）
mod render;
mod selection;
mod scroll;
mod footer;
mod props;
```

#### 模块间依赖关系

```
            ┌──────────────┐
            │   mod.rs     │  MessageArea 组件 + 视口裁剪渲染骨架
            └──────┬───────┘
       ┌──────────┼──────────┬──────────────┐
       ▼          ▼          ▼              ▼
   render.rs  scroll.rs  selection.rs    footer.rs
       ▲            │          ▲              ▲
       │            └──────────┤              │
       │                       │              │
       └─────── props.rs ──────┴──────────────┘
       (MessageAreaProps / MsgAreaTracker / ScrollbarFields / ScrollbarHook / mouse_in_area)
```

- `mod.rs` → 全部 4 个模块 + `props.rs`
- `scroll.rs` → `selection.rs`（鼠标 Down/Drag/Up 调用选区提取）+ `props.rs`（`mouse_in_area`）
- `selection.rs` → 无内部依赖（叶子）
- `footer.rs` → 无内部依赖（叶子）
- `props.rs` → 无内部依赖（叶子）

#### 可见性规则

| 项目 | 可见性 | 原因 |
|------|--------|------|
| `MessageArea` | `pub`（mod.rs 内） | `layout.rs` 引用 |
| `MessageAreaProps` | `pub`（props.rs 内）+ `pub use props::MessageAreaProps`（mod.rs） | 外部 2 处引用 |
| `TodoItem`/`TodoStatus` | `pub`（footer.rs 内）+ `pub use footer::{...}`（mod.rs） | 外部 5 处引用 |
| `mark_copy_message` | `pub(super)`（selection.rs 内） | 仅 `mod.rs`/`scroll.rs` 调用 |
| `vm_to_lines`/9 个 `render_*_lines` | `pub(super)`（render.rs 内） | 仅 `mod.rs` 调用 |
| `ScrollThrottle`/`DragThrottle`/`SCROLL_LINES`/`SCROLL_FRAME_MS` | `pub(super)`（scroll.rs 内） | 仅 `mod.rs` 调用 |
| `WrappedLineInfo`/`build_wrap_map`/`visual_to_logical`/`viewport_logical_range`/`highlight_logical_range`/`extract_visual_range`/`copy_to_clipboard` | `pub(super)`（selection.rs 内） | `mod.rs` 和 `scroll.rs` 调用 |
| `ScrollbarFields`/`ScrollbarHook` | `pub(super)`（props.rs 内） | `mod.rs` 调用 |
| `MsgAreaTracker`/`mouse_in_area` | `pub(super)`（props.rs 内） | `mod.rs`/`scroll.rs` 调用 |

### §3 Hook 顺序 + 闭包搬迁的特殊处理

ratatui-kit 对 hook 调用顺序敏感——CLAUDE.md 明确："任何 `hooks.use_*` 调用点变更后，必须列出该组件中每一个 hook 调用"。重组后 28 个 hook 调用必须保持种类和相对顺序一致。

#### 当前所有 hooks.use_* 调用清单（搬迁后必须 byte-for-byte 一致）

**`MessageArea` 组件本体**（21 个）：

| # | 行 | 类型 | 标识 |
|---|----|------|------|
| 1 | 887 | `use_atom(&VIEW_MODELS)` | view_models |
| 2 | 888 | `use_atom(&ACP_STATE)` | acp_state |
| 3 | 889 | `use_atom(&TODO_ITEMS)` | todo_atom |
| 4 | 890 | `use_atom(&LANG_VERSION)` | — |
| 5 | 903 | `use_state((gen,width,**Arc<Vec<Line>>**))` | lines_cache |
| 6 | 908 | `use_state((gen,w,len,rows))` | total_rows_cache |
| 7 | 911 | `build_footer_lines(&mut hooks, ...)` | **转调，内含 7 个 hook** |
| 8 | 961 | `use_state(ScrollViewState)` | scroll_state |
| 9 | 962 | `use_state(usize) 0` | prev_items_len |
| 10 | 963 | `use_state(false)` | _prev_is_loading |
| 11 | 964 | `use_state(ScrollThrottle)` | scroll_throttle |
| 12 | 968 | `use_state(TextSelection)` | text_sel |
| 13 | 969 | `use_state(Option<(u16,u16)>)` | selection_down_pos |
| 14 | 970 | `use_state(DragThrottle)` | drag_throttle |
| 15 | 973 | `use_state((gen,w,**Arc<Vec<WrappedLineInfo>>**))` | wrap_map_cache |
| 16 | 976 | `use_hook(MsgAreaTracker::new)` | area_hook |
| 17 | 979 | `use_state(ScrollbarFields::default)` | scrollbar_fields |
| 18 | 980 | `use_hook(move \|\| ScrollbarHook { fields: scrollbar_fields })` | scrollbar 渲染 |
| 19 | 1052 | `use_event_handler(Global, High, ...)` | 鼠标+键盘 |
| 20 | 1307 | `use_state(u16) 0` | last_scrolled_at |
| 21 | 1308 | `use_effect(...)` | 吸底跟随 |

**`build_footer_lines` 内部**（7 个，第 7 项的展开）：

| # | 行 | 类型 | 标识 |
|---|----|------|------|
| 7.1 | 1516 | `use_state(SpinnerState)` | spinner_state |
| 7.2 | 1517 | `use_state(Option<Instant>)` | load_start |
| 7.3 | 1518 | `use_state(false)` | was_loading |
| 7.4 | 1519 | `use_state(u64) 0` | summary_elapsed_ms |
| 7.5 | 1520 | `use_atom(&LOADING_EPOCH)` | loading_epoch |
| 7.6 | 1521 | `use_state(u64) 0` | last_epoch |
| 7.7 | 1523 | `use_state(BRIDGE_RESET_COUNTER.get())` | last_reset_counter |

**搬迁铁律**：这 28 个 hook 调用，**种类和相对顺序在 mod.rs 中必须 byte-for-byte 保持一致**。任何重排、合并、删除都会触发 `"Hook type mismatch"` panic 或状态数据错位。

**注意：当前文件已无 `highlight_cache` 和 `init_frames`**（视口裁剪改造时已删除）。spec 以当前状态为准。

#### 关键 [TRAP]（重组时必须保留，不得"顺手清理"）

| Trap | 含义 | 涉及代码 |
|------|------|----------|
| **`Arc<Vec>` 而非 `Vec`** | ratatui-kit 每次 dispatch 后都触发 render，Drag 60-120Hz 反复读缓存。Vec 深拷贝 O(N) 拖满 CPU；Arc::clone 是 O(1) 引用计数 | `lines_cache.2`、`wrap_map_cache.2` |
| **parking_lot 同 thread 死锁** | 同一 thread 同时持有 read + write guard 时 `try_write` 返回 Err → expect panic。必须先把 read guard 的值 copy 出 owned，drop guard，再 write | Drag/Up 事件中的 `selection_down_pos.read()` / `text_sel.read()` / `wrap_map_cache.read()` |
| **`write_no_update` vs `write`** | ratatui-kit `ReactiveMutRef::Drop` 无条件 `notifier.wake()`（不检查值是否变化）。render body 内或 wake 噪音路径必须用 `write_no_update()`，避免自激回路 100% CPU | `selection_down_pos`、`drag_throttle`、`scroll_throttle`、所有缓存更新 |
| **render body 禁止写 atom** | render 期间任何 atom 写入会与组件生命周期交互形成 render → state write → render 自激回路 | 整个组件函数 |
| **视口裁剪 lazy 构建** | Drag 期间 generation/width/lines_len 不变，wrap_map / total_visual_rows 都已命中缓存。仅在 cache 未命中或非 highlight 路径才构建 `all_lines` | 950–1000（lazy 构建 core_lines_arc 使用路径） |

#### 闭包搬迁策略：薄壳委托

`use_event_handler` 和 `use_effect` 必须在 mod.rs 调用（ratatui-kit 要求 hook 在组件函数里注册），但闭包体改为调用 `scroll.rs` 的模块级函数，**只保留"收集 state + 委托"两行**。

**`mod.rs` 中（薄壳）**：

```rust
// hook #19
hooks.use_event_handler(EventScope::Global, EventPriority::High, {
    move |event| {
        if let Event::Key(key) = &event {
            let _ = focus_router::message_accepts_key(key);
        }
        scroll::handle_event(
            &event,
            area_rect,
            vis_width,
            &scroll_state, &scroll_throttle, &text_sel,
            &selection_down_pos, &drag_throttle,
            &wrap_map_cache, &lines_cache,
        )
    }
});

// hook #21
hooks.use_effect(
    {
        move || scroll::run_auto_follow(&scroll::AutoFollowCtx {
            total_visual_rows, vis_height,
            scroll_state: scroll_state.clone(),
            prev_items_len: prev_items_len.clone(),
            last_scrolled_at: last_scrolled_at.clone(),
            items_len, is_loading,
        })
    },
    (items_len, vm_generation, is_loading),
);
```

**`scroll.rs` 中（实际逻辑）**：

```rust
pub(super) fn handle_event(
    event: &Event,
    area_rect: Option<Rect>,
    vis_width: u16,
    scroll_state: &State<ScrollViewState>,
    scroll_throttle: &State<ScrollThrottle>,
    text_sel: &State<TextSelection>,
    selection_down_pos: &State<Option<(u16, u16)>>,
    drag_throttle: &State<DragThrottle>,
    wrap_map_cache: &State<(u64, u16, Arc<Vec<WrappedLineInfo>>)>,
    lines_cache: &State<(u64, usize, Arc<Vec<Line<'static>>)>,
) -> EventResult {
    // 当前 1052-1233 行的鼠标+键盘处理逻辑原样搬迁
    // 含 PERI_DISABLE_DRAG_SELECT 分支
    // 含 parking_lot 同 thread 死锁规避（copy 出 owned 值后 drop guard 再 write）
    // 调用 selection::extract_visual_range / selection::copy_to_clipboard / selection::mark_copy_message
}

pub(super) struct AutoFollowCtx {
    pub total_visual_rows: u16,
    pub vis_height: u16,
    pub scroll_state: State<ScrollViewState>,
    pub prev_items_len: State<usize>,
    pub last_scrolled_at: State<u16>,
    pub items_len: usize,
    pub is_loading: bool,
}

pub(super) fn run_auto_follow(ctx: &AutoFollowCtx) {
    // 当前 1308-1340 行的吸底逻辑原样搬迁（注意：prev==0 && len>0 分支已删除）
}

// 内嵌闭包提为模块级私有函数
fn apply_scroll(
    delta: i32,
    scroll_throttle: &State<ScrollThrottle>,
    scroll_state: &State<ScrollViewState>,
) {
    // 当前 1056-1080 行原样搬迁
}
```

#### `extract_visual_range` 提升

当前定义在 `MessageArea` 函数体内（1233–1305），捕获 `vis_width`。搬到 `selection.rs` 后改为模块级函数（签名已纯参数化）——**唯一改动是定义位置**。

```rust
pub(super) fn extract_visual_range(
    lines: &[Line<'static>],
    wrap_map: &[WrappedLineInfo],
    vis_start: (u16, u16),
    vis_end: (u16, u16),
    width: u16,
) -> Option<String> { ... }
```

注意：当前实现包含 `clamp sr/er 到 wrap_map 视觉范围内`（避免 footer 区域的 `visual_to_logical` 返回 None）——这部分逻辑必须原样保留。

#### 设计约束

1. **State clone 成本**：`State<T>` 在 ratatui-kit 是 Arc 内核，clone 廉价。`AutoFollowCtx` 用 by-value `State` 字段，效果上等同 move 进闭包。
2. **`handle_event` 9 参数 vs struct ctx**：选 9 参数直接传，与 ratatui-kit 风格一致，不引入新概念。
3. **`AutoFollowCtx` 用 struct**：字段太多（7 个），struct 更清晰——这是 7 参数函数的常见取舍，不算"新概念"。
4. **不修改 deps**：`use_effect` 的 deps `(items_len, vm_generation, is_loading)` 完全保持。
5. **视口裁剪渲染骨架保留在 mod.rs**：viewport_lines 构建、scroll_offset_y 计算、Paragraph::scroll 调用都是 render body 的一部分，依赖大量组件 state，不提取到子函数（参数会膨胀到 10+）。

### §4 测试归属 + 验证策略

#### 测试归属（无变化，与当前测试区一一对应）

| 测试 | 当前来源行 | 落到 |
|------|-----------|------|
| `test_render_todo_lines_icons_and_crossed` | 1604 | `footer.rs` |
| `test_render_todo_lines_empty` | 1648 | `footer.rs` |
| `test_spinner_summary_after_loading_ends` | 1660 | `footer.rs` |
| `test_token_count_no_write_when_unchanged` | 1671 | `footer.rs` |
| `test_footer_loading_steady_state_has_no_control_state_transition` | 1680 | `footer.rs` |
| `test_empty_with_todo_items_shows_footer_not_welcome` | 1692 | `mod.rs` |
| `test_empty_without_todo_is_truly_empty` | 1705 | `mod.rs` |
| `proximity_check` + 7 个 `test_proximity_*` | 1715–1782 | `scroll.rs` |

**注意**：当前测试 `proximity_check` 用 `(vis_height / 2).max(5)`，生产代码（1328 行附近）用 `(vis_height / 4).max(5)`——这是测试辅助函数本身的偏差，**纯重组范围内不修**。

每个子模块的 `#[cfg(test)] mod tests` 独立——单元测试就近。

#### 验证步骤

1. **编译**：`cargo build --workspace`
2. **单测**：`cargo test -p peri-tui --lib`（基线：当前文件应全过）
3. **全量测试**：`cargo test --workspace --lib`（基线：963 passed，2 个已存在 peri-middlewares 失败可忽略）
4. **行为对比**（手动）：
   - 长对话滚动（节流是否工作）
   - 视口裁剪渲染（滚动条显示、长对话不卡）
   - 历史回放（吸底逻辑——Open 的 history-replay-scroll-too-early issue 现状）
   - 鼠标拖拽选中复制（含 `PERI_DISABLE_DRAG_SELECT=1` 验证回退路径）
   - 高亮渲染（含 `PERI_NO_HIGHLIGHT=1` 验证回退路径）
   - loading spinner + "Brewed for Ns"
   - Todo 列表渲染
   - Welcome 屏（空对话）
   - 消息间距、tool card 折叠/展开
5. **diff 检查**：`git diff --stat` 应显示新文件总行数 ≈ 旧文件 1782 行（差异来自 import 语句和 mod 声明）

### §5 风险与回滚

**风险等级**：低–中（敏感文件 + trap 密集）。

| 风险 | 触发条件 | 缓解 |
|------|----------|------|
| Hook 顺序错位 panic | 搬迁中漏 hook 或重排 | §3 已列出 28 个 hook 完整清单，搬迁后逐行核对 |
| 外部模块编译失败 | `TodoItem`/`TodoStatus` 路径变化 | §2 已设计 `pub use` 重导出，外部 5 处调用路径不变 |
| 闭包 state 捕获错误 | `State<T>` 不能 clone 进闭包 | ratatui-kit `State<T>` 是 Arc 内核，可 clone；若不行则降级为"闭包内联"方案 |
| Arc<Vec> 误改回 Vec | 搬迁时把 `Arc::clone` 写成 `clone()` | §3 trap 表已标注，review 时重点检查 |
| parking_lot 死锁 panic | 搬迁时把"copy → drop guard → write"误写为"持 read guard 时 write" | §3 trap 表已标注，review 时重点检查 Drag/Up 事件路径 |
| 行为偏差 | 搬迁时手抖改了逻辑 | 验证步骤 4 手动测关键场景 |

**回滚**：单 commit revert 即可恢复。

### §6 CLAUDE.md 更新

#### 根 CLAUDE.md "核心文件树"段

当前写的是：

```
peri-tui/src/kit/message_area.rs  # 消息区（直接消费 VIEW_MODELS，视口裁剪 + ScrollThrottle）
```

重组后更新为：

```
peri-tui/src/kit/message_area/  # 消息区（视口裁剪 + 文本选区 + Todo）
  mod.rs          # MessageArea 组件 + hook 装配 + 视口裁剪渲染骨架
  render.rs       # TuiRenderUnit → Vec<Line>
  selection.rs    # wrap_map / viewport_logical_range / 选区高亮 / 剪贴板
  scroll.rs       # ScrollThrottle / DragThrottle / 鼠标事件 / 吸底
  footer.rs       # build_footer_lines + TodoItem
  props.rs        # Props + MsgAreaTracker + ScrollbarFields/Hook
```

#### peri-tui/CLAUDE.md "渲染管道"段（已过时，但本次重组范围内不修）

`peri-tui/CLAUDE.md` 的渲染管道段仍描述 `render_bridge` + `RENDER_CACHE` + `ScrollView`，与当前代码（视口裁剪 + Arc<Vec> 缓存）已脱节。**这是独立的文档维护问题，不在本次重组范围**。重组完成后可单独发起 CLAUDE.md 文档同步任务。

#### Trap 仍然适用

CLAUDE.md 关于 message_area 的其他 trap（`write_no_update` 自激回路、render body 禁止写 atom 等）**全部仍然适用**，因为这些约束是关于代码模式而不是文件位置。本次重组为这些 trap 添加精确的"在新模块中的位置"指引：

- "render body 禁止写 atom" → mod.rs / scroll.rs（render 路径）
- "use_state write vs write_no_update 自激回路" → scroll.rs（事件处理器）+ mod.rs（缓存更新）
- "parking_lot 同 thread 死锁" → scroll.rs（Drag/Up 事件路径）
- "Arc<Vec> vs Vec" → mod.rs（缓存声明）+ selection.rs（wrap_map 读取）

## 实施顺序（建议）

1. 新建 `peri-tui/src/kit/message_area/` 目录，删除旧 `message_area.rs`，建立 6 个空文件 + mod.rs 入口
2. 按"叶子优先"顺序搬运，每搬一个文件立即 `cargo build -p peri-tui` 验证：
   - `props.rs`（叶子，含 ScrollbarFields/Hook）
   - `footer.rs`（叶子）
   - `render.rs`（叶子）
   - `selection.rs`（叶子，含 viewport_logical_range）
   - `scroll.rs`（依赖 selection + props）
   - `mod.rs`（装配所有 + 视口裁剪渲染骨架）
3. 搬迁 hook 调用，逐行对照 §3 清单
4. 搬迁测试
5. 更新 CLAUDE.md 文件树
6. 运行完整验证步骤

详细步骤由后续 implementation plan 展开。
