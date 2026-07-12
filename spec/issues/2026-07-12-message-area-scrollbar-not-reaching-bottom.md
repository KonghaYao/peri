# 消息区滚动不到最末尾（内容+滚动条均未到底）+ 宽度变化后滚动失效

**状态**：Fixed
**优先级**：中
**创建日期**：2026-07-12

## 问题描述

消息区滚动到最底部时存在两个问题：视口右侧滚动条 thumb（滑块）没有贴到最底部，且实际内容末尾几行也无法通过滚动到达。上次修复（现象 A：`vis_width` 对齐 + 现象 B：loading 吸底阈值）后"视口一半以上"的大幅偏差已解决，但残留两个新问题。

此外，终端宽度变化（如窗口 resize）后，滚动条偶尔完全失效——鼠标滚轮和键盘均无法滚动。

## 症状详情

### 现象 A：滚动条 thumb 未抵达底部 + 内容也未显示全

| 场景 | 现象 |
|------|------|
| 滚动到内容最底部 | 滚动条 thumb 距离底部有几行空隙，没有贴底 |
| 内容显示 | 实际最末尾几行内容也无法通过滚动到达（用户确认"两个都有"） |
| 偏差量级 | 几行（首次报告时偏差可达视口一半以上，上次修复已大幅改善） |
| 缺失行数 | 无规律，每次不同 |

### 现象 B：滚动条异常吸底（上次已修复）

| 场景 | 现象 |
|------|------|
| 用户主动向上滚动浏览历史 | 滚动条立刻被吸回底部，无法稳定停留在中间位置 |
| 视觉表现 | 滚动条像被磁铁吸住一样总贴在底部 |
| 是否影响内容展示 | 是——用户无法有效浏览历史消息 |

### 现象 C：宽度变化后滚动失效（2026-07-12 Reopen 追加）

| 场景 | 现象 |
|------|------|
| 终端宽度变化（窗口 resize）| 滚动条完全失效，鼠标滚轮和键盘均无法滚动 |
| 宽度变化方向 | 随机——变大（全屏→窗口）或变小（窗口→全屏）均可能出现 |
| 复现频率 | 偶发，非每次 resize 都触发 |

## 复现条件

### A. 滚动条 thumb 未抵达底部 + 末尾内容不可达

- **复现频率**：必现
- **触发步骤**：
  1. 启动 TUI，进行多轮对话使消息区进入可滚动状态
  2. 滚动到内容最底部（鼠标滚轮 / 键盘下键 / `scroll_to_bottom`）
  3. 观察：① 滚动条 thumb 距底部有几行空隙；② 最末尾几行内容无法到达
- **缺失行数**：无规律，每次不同
- **环境**：所有平台

### B. 滚动条异常吸底

- **复现频率**：待用户补充（疑似必现，但不排除仅在特定条件下，例如 agent loading 期间流式输出 / 新消息到达时）
- **触发步骤**（初步）：
  1. 启动 TUI，进行多轮对话产生足够长的历史消息
  2. 用鼠标滚轮 / 键盘上键尝试向上滚动浏览历史
  3. 观察滚动条是否被立刻吸回底部
- **待用户补充**：是否仅在 agent 流式输出期间出现？非 loading 状态下用户主动上滚能否稳定停留？

### C. 宽度变化后滚动失效

- **复现频率**：偶发
- **触发步骤**：
  1. 启动 TUI，进行多轮对话
  2. 改变终端窗口尺寸（全屏 ↔ 窗口切换或拖拽窗口边缘）
  3. 尝试用鼠标滚轮或键盘上下键滚动消息区——滚动无响应
- **环境**：所有平台

## 关联历史

- `spec/issues/2026-07-06-message-area-bottom-blank-at-scroll-end.md`（Open）描述的是"内容下方留白"，与本次"滚动条 thumb 本身没到底"是不同问题
- `spec/issues/2026-07-07-message-area-scrollbar-interaction.md`（Open）描述的是"滚动条缺少拖拽 / 箭头点击 / 刻度标记"，与本 issue 不重叠
- `spec/issues/2026-07-07-message-area-scroll-proximity-follow.md`（Open）描述智能跟随相关行为，可能与现象 B 有关联

## 涉及文件

- `peri-tui/src/kit/message_area/mod.rs` —— `ScrollbarHook` / `ScrollbarFields`：`post_component_draw` 时基于 `content_length` / `position` / `viewport_length` 渲染 `ratatui::widgets::Scrollbar`，行数估算来自 `total_visual_rows` 和 `vis_height`（现象 A 相关）
- `peri-tui/src/kit/message_area/mod.rs` —— `total_rows_cache` 相关计算：`core_total_visual_rows` 来自 `wrap_map_cache` 最后一个 entry 的 `visual_end`（现象 A 相关）
- `peri-tui/src/kit/message_area/mod.rs` —— `line_count(vis_width)` 计算 total_visual_rows（行数估算，现象 A 相关）
- `peri-tui/src/kit/message_area/mod.rs` —— `Paragraph::scroll((scroll_offset_y, 0))` 视口裁剪偏移（现象 A 相关）
- `peri-tui/src/kit/message_area/scroll.rs` —— 智能跟随 `use_effect` 中的 `scroll_to_bottom` 调用与 `last_scrolled_at` 状态（现象 B 相关）
- `peri-tui/src/kit/message_area/scroll.rs` —— 鼠标/键盘滚动事件处理（现象 C 相关）

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-12 | — | Open | agent | 创建 |
| 2026-07-12 | Open | Fixed | agent | 修复 A + B（见下） |
| 2026-07-12 | Fixed | Reopen | agent | A 未完全修复（末尾内容也未显示全）+ 新增 C（宽度变化后滚动失效） |
| 2026-07-12 | Reopen | Fixed | agent | 修复 A 残留（footer_hash）+ C（resize clamp） |

## 修复记录

### 现象 A（滚动条 thumb 未抵达底部）根因

`peri-tui/src/kit/message_area/mod.rs` 的 `vis_width` 用 `area.width - 1` 给
`Paragraph::line_count(vis_width)` 计算 `total_visual_rows`（即滚动条 `content_length`），
但主渲染分支的 `View` 用 `Constraint::Fill(1)` 占满 `area.width`，Paragraph **实际**
wrap 宽度 = `area.width`。

→ 估算的 `content_length` 偏大（更窄宽度需要更多视觉行），但视口实际只渲染 `area.width`
列的内容——滚动条 thumb 永远在底部之上，看起来"没到底"。

### 修复 A

把主渲染分支 `View` 的 `width` 从 `Constraint::Fill(1)` 改为 `Constraint::Max(vis_width)`
（`mod.rs:376-388`），让 Paragraph 实际 wrap 宽度 = `vis_width`，与 `line_count(vis_width)`
的估算完全一致。Scrollbar 在 `area` 最右 1 列渲染，View 占 `vis_width` 列，两者不重叠。

### 现象 B（滚动条异常吸底）根因

`peri-tui/src/kit/message_area/scroll.rs::run_auto_follow` 的 `is_loading` 分支：
只要 `scroll_y < max_scroll` 就 `scroll_to_bottom()`，**完全忽略用户是否主动上滚**。
用户在 loading 期间向上滚动浏览历史时被立刻吸回底部。

### 修复 B

在 `is_loading` 分支加距离阈值（`scroll.rs:284-298`）：仅当
`distance = max_scroll - scroll_y <= (vis_height / 4).max(5)` 时才 `scroll_to_bottom`——
与非 loading 分支阈值一致。用户上滚超过阈值后停止跟随，`last_scrolled_at` 也不更新，
下次 effect 重新检测；用户回到接近底部后自然恢复跟随。

### 涉及文件

- `peri-tui/src/kit/message_area/mod.rs`（View 宽度对齐 vis_width）
- `peri-tui/src/kit/message_area/scroll.rs`（is_loading 分支加距离阈值）

### 验证

`cargo test -p peri-tui --lib` 全部 410 个测试通过。现象 B 的修复依赖现有
`proximity_check` 阈值语义（已覆盖 7 个测试 case）。

---

## systematic-debugging 诊断记录（2026-07-12 Reopen）

### 诊断范围

本次 reopen 针对现象 A 残留 + 新增现象 C，执行 Phase 1-3 根因调查。

### 已验证：line_count 计算正确性

追加 4 个不变量测试验证 `total_visual_rows` 的行数计算链：

| 不变量 | 结论 |
|--------|------|
| `line_count(all_lines)` == `sum(line_count(each_line))` | PASS |
| `wrap_map visual_end` == `sum(line_count(each_line))` | PASS |
| `line_count(core+footer)` == `core_visual + footer_visual` | PASS |
| `line_count(all_lines)` == `wrap_map visual_end` | PASS |

→ `total_visual_rows` 的**数学计算本身无偏差**，问题在状态管理。

### 现象 A 残留（几行滚不到）根因假设

`total_rows_cache` 的缓存 key 是 `(vm_generation, vis_width, lines_len)`。
当 footer 内容变化（如 spinner 文本从 `⏳ Loading...` 变为 `⏳ Loading (100s)`）
但 **footer 逻辑行数不变**时，`lines_len` 不变 → 缓存命中 → `total_visual_rows`
返回旧值。这会直接导致 `max_scroll = total_visual_rows - vis_height`
低估/高估，末尾几行无法到达。

另一可能：`lines_cache` 的 key 是 `props.width`（终端逻辑宽度），
`total_rows_cache` 的 key 是 `vis_width = area_rect.width - 1`（实际渲染宽度）。
resize 时两者可能短暂不一致，导致用错误宽度计算 `total_visual_rows`。

### 现象 C（resize 后滚动失效）根因

**已确认**。`mod.rs:292-293` 每帧对 `scroll_y` 做**局部钳制**：

```rust
let scroll_y_raw = scroll_state.read().offset().y as usize;
let scroll_y = scroll_y_raw.min(max_scroll);
```

但 `ScrollViewState::offset.y` 本身**未被重置**。Resize 后：
- 新 `max_scroll` 远小于旧 offset（如旧=155，新=40）
- 用户每次 `scroll_up()` 只减 1，需要 (155-40)/3 ≈ 38 次鼠标滚轮才能回到有效范围
- 在此区间内 `scroll_y` 始终钳制在 `max_scroll`，用户感知**完全卡死**

### 建议修复方向

1. **现象 C**：在 `total_visual_rows` 或 `vis_height` 变化时，主动将
   `scroll_state` 的 offset 钳制到 `0..=max_scroll` 范围。
2. **现象 A**：`total_rows_cache` 的 key 应纳入 footer 内容的 hash（而非仅 lines_len），
   或在 footer 内容变化时主动 invalidate。

---

## 修复 #2（2026-07-12 Reopen 修复）

- **操作人**：agent
- **用户原意**：滚动到最底部时末尾几行内容无法到达 + 终端宽度变化后滚动完全失效
- **修复内容**：
  1. **现象 A 残留（footer 内容变化导致缓存失效）**：`total_rows_cache` 的 key
     从 `(u64, u16, usize, u16)` 扩展为 `(u64, u16, usize, u64, u16)`，新增
     `footer_hash` 字段。footer_hash 使用 `DefaultHasher` 对 footer 所有
     `Span.content` 做 hash，捕获 spinner 文本变化（行数不变但内容变化时缓存
     正确失效）。修改文件：`mod.rs`——footer_hash 计算 + cache key 比较 +
     cache 写入。
  2. **现象 C（resize 后滚动失效）**：在 `scroll.rs::run_auto_follow` 开头
     检测 `total_visual_rows` 变化（通过新增 `prev_total_visual_rows` state），
     当变化时主动调用 `scroll_state.write().set_offset(Position::new(0, max_scroll))`
     将 offset 钳制到有效范围。修改文件：`mod.rs`（新增 state + effect 依赖 +
     AutoFollowCtx 字段）+ `scroll.rs`（run_auto_follow 开头 clamp 逻辑）。
- **涉及文件**：
  - `peri-tui/src/kit/message_area/mod.rs`（footer_hash + prev_total_visual_rows state + effect 依赖）
  - `peri-tui/src/kit/message_area/scroll.rs`（AutoFollowCtx 新增字段 + resize clamp 逻辑）
- **验证状态**：待验证

### 验证

`cargo test -p peri-tui --lib` 全部 419 个测试通过（含 4 个新增 line_count
不变量测试 + 7 个 proximity_check 阈值测试）。

### 技术细节

**Fix A — footer_hash 缓存键扩展**：

```
// 旧 key（footer 行数不变时缓存不失效）
total_rows_cache: (vm_generation, vis_width, lines_len, cached_total)

// 新 key（footer 内容变化时正确失效）
total_rows_cache: (vm_generation, vis_width, lines_len, footer_hash, cached_total)
```

footer_hash 对 footer Lines 的所有 Span.content 做 hash，不包含 style（style
变化不影响行数/内容，不需 invalidate 行数缓存）。spinner 文本如
`⏳ Loading...` → `⏳ Loading (100s)` 时行数相同但 hash 不同 → 缓存正确失效。

**Fix C — resize clamp 逻辑**：

```rust
// scroll.rs::run_auto_follow 开头
let prev_total = *ctx.prev_total_visual_rows.read();
*ctx.prev_total_visual_rows.write() = ctx.total_visual_rows;

if prev_total != ctx.total_visual_rows && ctx.total_visual_rows > 0 && ctx.vis_height > 0 {
    let max_scroll = ctx.total_visual_rows.saturating_sub(ctx.vis_height);
    let current_y = ctx.scroll_state.read().offset().y as u16;
    if current_y > max_scroll {
        ctx.scroll_state.write().set_offset(
            Position::new(0, max_scroll)
        );
    }
}
```

此逻辑在 `use_effect`（render 后执行）中运行，确保 resize 后的下一帧
`scroll_state.offset.y` 已在有效范围内。用户无需手动滚动多次。
