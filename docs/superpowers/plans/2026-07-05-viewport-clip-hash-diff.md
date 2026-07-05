# 视口裁剪 + Hash Diff 长上下文渲染优化

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 从 peri-main 迁移视口裁剪和 content_hash 前缀检测技术到 perihelion，解决长历史 thread 切换时的 CPU 100% 问题。

**Architecture:** 三步：① ViewModel 增加 `content_hash` 字段，render_bridge 增加 `wrap_map`（每逻辑行的视觉行映射）；② render_bridge rebuild 时基于 hash 前缀检测只重渲染变化的消息；③ message_area 基于 wrap_map 二分查找，每帧只传递可见行给 Paragraph。

**Tech Stack:** Rust, ratatui, pulldown-cmark, ratatui-kit AtomStatic

**参考项目:** `/Users/konghayao/Downloads/peri-main` (render_thread.rs, message_area.rs v1)

---

## 文件结构

| 文件 | 操作 | 职责 |
|------|------|------|
| `peri-acp-types/src/view_model.rs` | 修改 | 给 ViewModel 增加 `content_hash: u64` |
| `peri-tui/src/kit/render_bridge.rs` | 修改 | 增加 `WrappedLineInfo`、`wrap_map`、hash diff rebuild |
| `peri-tui/src/kit/message_area.rs` | 修改 | 增加 `viewport_clip` 视口裁剪 |

---

### Task 1: ViewModel 增加 content_hash

**Files:**
- Modify: `peri-acp-types/src/view_model.rs`

**背景**: peri-main 给每条 ViewModel 预计算 `content_hash: u64`，rebuild 时通过 hash 比对跳过未变化消息的渲染。hash 参与字段是影响渲染输出的语义内容，不参与的是纯 UI 状态（如 `is_streaming`）。

- [ ] **Step 1: 为 UserBubbleData 添加 content_hash**

在 `UserBubbleData` 结构体末尾添加字段：
```rust
/// 内容哈希——rebuild 时用于检测是否需重新渲染
#[serde(skip)]
pub content_hash: u64,
```

在构造时（所有 `UserBubbleData { ... }` 位置）添加 `.content_hash = hash_text(&data.text)` 或提供构造函数。hash 算法：`use std::hash::{Hash, Hasher}` → `let mut h = std::collections::hash_map::DefaultHasher::new(); text.hash(&mut h); h.finish()`。

- [ ] **Step 2: 为 AssistantBubbleData 添加 content_hash**

同上，hash 参与字段：`text`、`reasoning.text`（如有）。

- [ ] **Step 3: 为 ToolCardData 添加 content_hash**

hash 参与字段：`tool_name`、`input_summary`、`output_summary`、`is_error`、`diff.path`（如有）。

- [ ] **Step 4: 为 SubAgentGroupData 添加 content_hash**

hash 参与字段：`agent_id`、`agent_name`、`is_running`、`view_models` 的 hash 组合。

- [ ] **Step 5: 编译验证**

```bash
cargo check -p peri-acp-types -p peri-tui
```

- [ ] **Step 6: Commit**

```bash
git add peri-acp-types/src/view_model.rs
git commit -m "feat: add content_hash to ViewModel variants for hash diff"
```

---

### Task 2: render_bridge 增加 WrappedLineInfo + wrap_map

**Files:**
- Modify: `peri-tui/src/kit/render_bridge.rs`

**背景**: peri-main 通过 `WrappedLineInfo` 记录每条逻辑行 wrap 后的视觉行起止范围，支持 O(log N) 二分查找定位可见行。当前 perihelion 只有 `cumulative_heights: Vec<usize>`（总高度），没有 per-line 的视觉行映射。

- [ ] **Step 1: 定义 WrappedLineInfo 结构体**

在 `render_bridge.rs` 现有结构体之后添加：
```rust
/// 每条逻辑行的 wrap 信息——用于视口裁剪的二分查找
#[derive(Debug, Clone)]
pub struct WrappedLineInfo {
    /// 在 render_cache.lines 中的索引
    pub line_idx: usize,
    /// 该逻辑行渲染后的起始视觉行号（从 0 开始）
    pub visual_row: u16,
    /// 该逻辑行的视觉行数（>= 1）
    pub visual_height: u16,
}
```

- [ ] **Step 2: RenderCache 增加 wrap_map 字段**

在 `RenderCache` 结构体末尾添加：
```rust
/// 每逻辑行的视觉行映射——message_area 视口裁剪用
pub wrap_map: Vec<WrappedLineInfo>,
```

- [ ] **Step 3: 实现 build_wrap_map 函数（含空行去重）**

在拼接所有消息行后先去重连续空行，再用 `Paragraph::line_count()`（而非简单宽度除法）计算视觉行数，确保与 ratatui WordWrapper 完全一致：

```rust
/// 空行去重——连续空行只保留一个，移除末尾多余空行
fn dedup_lines(lines: &[ratatui::text::Line<'static>]) -> Vec<ratatui::text::Line<'static>> {
    let mut result: Vec<ratatui::text::Line<'static>> = Vec::with_capacity(lines.len());
    for line in lines {
        let is_empty = line.spans.is_empty()
            || line.spans.iter().all(|s| s.content.is_empty());
        if is_empty && result.last().map_or(false, |l: &ratatui::text::Line| {
            l.spans.is_empty() || l.spans.iter().all(|s| s.content.is_empty())
        }) {
            continue; // 连续空行去重
        }
        result.push(line.clone());
    }
    // 移除末尾空行
    while result.last().map_or(false, |l| l.spans.is_empty()) {
        result.pop();
    }
    result
}

fn build_wrap_map(lines: &[ratatui::text::Line<'static>], width: u16) -> Vec<WrappedLineInfo> {
    let deduped = dedup_lines(lines);
    let mut map = Vec::with_capacity(deduped.len());
    let mut row: u16 = 0;
    for (line_idx, line) in deduped.iter().enumerate() {
        let text = ratatui::text::Text::from(line.clone());
        let height = Paragraph::new(text)
            .wrap(ratatui::widgets::Wrap { trim: false })
            .line_count(width as usize) as u16;
        let visual_height = height.max(1);
        map.push(WrappedLineInfo {
            line_idx,
            visual_row: row,
            visual_height,
        });
        row = row.saturating_add(visual_height);
    }
    map
}
```


- [ ] **Step 4: 更新 rebuild 流程使用 build_wrap_map**

在 `rebuild_entries` 完成后的 `rebuild_cumulative_heights` 调用处同时构建 `wrap_map`：
```rust
cache.wrap_map = build_wrap_map(
    &cache.entries.iter().flat_map(|(_, e)| e.lines.iter()).cloned().collect::<Vec<_>>(),
    width,
);
```
注意：实际实现时需要从 entries 中提取所有行。更高效的做法是在 `append_entries` 后用一个单独的函数收集所有 lines。

- [ ] **Step 5: sync_render_cache 也更新 wrap_map**

`input_area.rs` 的 `sync_render_cache` 函数在增量追加后需重建 `wrap_map`。提取公用函数到 `render_bridge.rs`。

- [ ] **Step 6: 编译验证**

```bash
cargo check -p peri-tui
```

- [ ] **Step 7: Commit**

```bash
git add peri-tui/src/kit/render_bridge.rs peri-tui/src/kit/input_area.rs
git commit -m "feat: add WrappedLineInfo + wrap_map to RenderCache"
```

---

### Task 3: render_bridge hash diff 增量 rebuild

**Files:**
- Modify: `peri-tui/src/kit/render_bridge.rs`

**背景**: peri-main 在每次 rebuild 时通过 `content_hash` 做前缀稳定检测，只重新渲染 hash 变化的消息。稳定前缀的消息直接复用旧渲染缓存。

- [ ] **Step 1: 为 RenderTask（当前 render_bridge 内部状态）增加 hash 缓存**

当前 render_bridge 的事件循环在 `spawn_render_bridge` 中。在循环外用变量保存 hash 历史：
```rust
// 在 spawn_render_bridge 的 async move 块中，cache 后添加：
let mut msg_hashes: Vec<u64> = Vec::new();
let mut msg_lines_cache: Vec<Vec<ratatui::text::Line<'static>>> = Vec::new();
```

- [ ] **Step 2: 实现 prefix_stable_len 计算**

```rust
fn prefix_stable_len(new_hashes: &[u64], old_hashes: &[u64]) -> usize {
    new_hashes
        .iter()
        .zip(old_hashes.iter())
        .position(|(new_h, old_h)| new_h != old_h)
        .unwrap_or_else(|| old_hashes.len().min(new_hashes.len()))
}
```

- [ ] **Step 3: 修改 rebuild 流程使用 hash diff**

在 `append_entries` 调用前，计算 `prefix_stable_len`：
1. 从 incoming ViewModel snapshot 提取所有 content_hash
2. 计算 `stable = prefix_stable_len(&new_hashes, &msg_hashes)`
3. 稳定前缀的消息：直接从 `msg_lines_cache` 复用，跳过 markdown 解析
4. 变化部分：正常走 `extend_entries` + `render_v2_vm`
5. 更新 `msg_hashes` 和 `msg_lines_cache`

具体伪代码：
```rust
let new_hashes: Vec<u64> = snapshot.committed.iter().map(|vm| vm.content_hash()).collect();
let stable = prefix_stable_len(&new_hashes, &msg_hashes);

// 稳定前缀：从缓存复用
cache.entries.truncate(stable);
// 变化部分：正常渲染
extend_entries_async(&mut cache.entries, &snapshot.committed[stable..], width, stable, true).await;

msg_hashes = new_hashes;
// 更新 msg_lines_cache（渲染线程专用，跳过稳定前缀的 re-render）
```

- [ ] **Step 4: 处理 thread 切换时清空 hash 缓存**

当 `committed_len < last_committed_len` 时（切换 thread 或 clear），清空 `msg_hashes` 和 `msg_lines_cache`。

- [ ] **Step 5: 编译验证**

```bash
cargo check -p peri-tui
cargo test -p peri-tui --lib -- input_area
cargo test -p peri-tui --lib -- input_history
```

- [ ] **Step 6: Commit**

```bash
git add peri-tui/src/kit/render_bridge.rs
git commit -m "feat: hash diff incremental rebuild in render_bridge"
```

---

### Task 4: message_area 视口裁剪

**Files:**
- Modify: `peri-tui/src/kit/message_area.rs`

**背景**: peri-main 的 `viewport_clip` 通过 RENDER_CACHE 的 `wrap_map` 做二分查找，只传递视口内的行给 Paragraph，避免 ratatui 对全量行做 WordWrapper 遍历。ratatui 即使设了 `scroll(offset)` 仍会对 offset 前的所有行做 grapheme 分割 + wrap 计算。

- [ ] **Step 1: 在 LineCache 重建时构建 wrap_map**

在 `message_area.rs` 的 key-change 重建段（`if line_cache.read().key != new_key`），从 `cache_snapshot.wrap_map` 获取 wrap 数据并存储到 LineCache：
```rust
#[derive(Default)]
struct LineCache {
    // ... 现有字段
    wrap_map: Vec<crate::kit::render_bridge::WrappedLineInfo>,
}
```

重建时：
```rust
lc.wrap_map = cache_snapshot.wrap_map.clone();
```
注意：`wrap_map` 的 `clone()` 开销：5000 行时约 5000 × 3 × 8 bytes = 120KB，可接受（仅在 key 变化时执行）。

- [ ] **Step 2: 实现 viewport_clip 函数**

```rust
/// 视口裁剪：二分查找 wrap_map，返回 [first_visible, last_visible] 范围内的逻辑行
fn viewport_clip(
    wrap_map: &[WrappedLineInfo],
    vis_start: u16,
    vis_height: u16,
) -> (usize, usize) {
    let total_rows = wrap_map.last().map_or(0, |w| w.visual_row + w.visual_height);
    let clamped_start = vis_start.min(total_rows.saturating_sub(1));
    let clamped_end = (vis_start + vis_height).min(total_rows);

    let first = wrap_map.partition_point(|w| w.visual_row + w.visual_height <= clamped_start);
    let last = wrap_map.partition_point(|w| w.visual_row < clamped_end);

    (first, last)
}
```

- [ ] **Step 3: 修改渲染段使用 viewport_clip**

从 ScrollViewState 读取当前滚动偏移作为 `vis_start`。**关键：保留 ScrollView 包裹，内部 View 高度设为 wrap_map 总视觉行数，ScrollViewState 自然处理键盘/鼠标/auto-scroll 事件。内部 Paragraph 只渲染可见行 + 局部偏移。**

```rust
let scroll_y = scroll_state.read().offset().y as u16;
let vis_height = /* 消息区可用高度，如 40 */;
let (first, last) = viewport_clip(&line_cache_data.wrap_map, scroll_y, vis_height);

let visible_lines: Vec<Line<'static>> = cache_snapshot
    .entries
    .iter()
    .flat_map(|(_, entry)| entry.lines.iter())
    .skip(first)
    .take(last.saturating_sub(first))
    .cloned()
    .collect();

let local_offset = line_cache_data.wrap_map
    .get(first)
    .map_or(0, |w| scroll_y.saturating_sub(w.visual_row));

// total_visual_rows = 从 wrap_map 最后一条记录的 visual_row + visual_height
let total_visual_rows: u16 = line_cache_data.wrap_map
    .last()
    .map_or(0, |w| w.visual_row + w.visual_height);

element!(
    ScrollView(
        flex_direction: Direction::Vertical,
        width: Constraint::Fill(1),
        height: Constraint::Fill(1),
        scroll_view_state: scroll_state,
        scroll_bars: clean_scrollbars(),
    ) {
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Length(total_visual_rows.max(1)),
        ) {
            Text(text: Paragraph::new(RatText::from(visible_lines)).scroll((local_offset, 0)))
        }
    }
)
```

注意：
- `ScrollView` 保留——`scroll_state` 继续响应鼠标滚轮、Ctrl+U/D/Home/End 和 `scroll_to_bottom()`
- 内部 `View` 的 `height` = 总视觉行数（从 wrap_map 最后记录推算），ScrollView 依此创建正确大小的虚拟视口
- `Paragraph::scroll((local_offset, 0))` 做子行级微调——将可见行的第一行正确对齐到视口顶部

**为何仍需 `Paragraph::scroll()`**：wrap_map 的 `visual_row` 粒度是"整行"级别，但 ScrollView 的 `scroll_y` 可能让某行只露出下半部分。`local_offset` 补偿这个差值，确保渲染起始行与 ScrollView 视口顶部精确对齐。

- [ ] **Step 4: 移除 chunk 分块逻辑**

视口裁剪后必然只有几十行，不需要 200 行/chunk 的分块。移除 `highlight_chunks` 缓存（或保留作为选区高亮的数据源，但不用于渲染）。

- [ ] **Step 5: 编译验证 + 测试**

```bash
cargo check -p peri-tui
cargo test -p peri-tui --lib -- input_area
cargo test -p peri-tui --lib -- input_history
```

- [ ] **Step 6: Commit**

```bash
git add peri-tui/src/kit/message_area.rs
git commit -m "feat: viewport clip - only render visible lines via wrap_map binary search"
```

---

### Task 5: 端到端验证

- [ ] **Step 1: 全量测试**

```bash
cargo fmt
cargo check -p peri-tui
cargo test -p peri-tui --lib
```

- [ ] **Step 2: 确认无 warning**

```bash
cargo clippy -p peri-tui 2>&1 | grep -E "peri-tui.*warning" | grep -v "redundant_closure\|collapsible_if\|needless_update" | head -5
```

只有预存 warning（redundant_closure 等），无新增。

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "feat: viewport clip + hash diff for long context rendering"
```

---
