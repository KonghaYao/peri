# 消息区就近判断自动吸底 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将消息区自动吸底跟随从二元开关 `auto_scroll: bool` 改为基于滚动位置与底部距离的就近判断，用户回看历史时不再被内容增长拉回底部。

**Architecture:** 删除 `auto_scroll` / `had_ct` 两个 `use_state`。在 render body 中将 `total_visual_rows` 和 `vis_height` 提前计算到 `use_effect` 声明之前，`use_effect` 以 `(entries_len, raw_ch)` 为依赖——每帧内容高度变化时触发。闭包内计算 `distance_to_bottom = total - scroll_y - vis_height`，仅当 `distance <= max(vis_height/2, 5)` 时调用 `scroll_to_bottom()`。用户任何滚动操作自然改变 `scroll_y`，无需额外 flag。

**Tech Stack:** ratatui-kit 0.7 `use_effect` + `ScrollViewState::offset()`/`scroll_to_bottom()`, Rust 2024, 无新增依赖

---

## 文件结构

- **Modify only**: `peri-tui/src/kit/message_area.rs` —— 唯一变更文件（253 行代码变更，净减约 30 行）
- **无新增文件**

关键修改点：
1. `vis_height` 提前到 `area_rect` 之后计算（当前 line 558 → 移到 line 333 附近）
2. `total_visual_rows` 提前到 `wrap_map` 之后计算（当前 line 569 → 移到 line 412 附近）
3. 删除 `auto_scroll` (line 274) 和 `had_ct` (line 275) 两个 `use_state`
4. 删除 `should_follow_on_content_change` (line 277) 及其使用 (line 304-306)
5. `use_effect` (line 529-545) 重写为就近判断，依赖改为 `(entries_len, raw_ch)`
6. 事件 handler 中删除所有 `auto_scroll.set(false)` 调用（5 处）及条件检查（1 处）

---

### Task 1: 将 total_visual_rows 和 vis_height 计算提前

**Files:**
- Modify: `peri-tui/src/kit/message_area.rs:331-338, 384-411, 556-575`

- [ ] **Step 1: 将 vis_height 移到 area_rect 之后**

当前 `vis_height` 在第 558 行计算，需要移到第 335 行 `vis_width` 之后，使它能被后续 `use_effect` 捕获。

在 `peri-tui/src/kit/message_area.rs` 中，找到第 335-338 行的 `vis_width` 计算，在其后插入 `vis_height` 计算：

```rust
    // 渲染宽度（提前计算，raw_wrap_map 和 highlighted_lines wrap_map 共用）
    let vis_width = area_rect
        .map(|r| r.width.saturating_sub(1))
        .unwrap_or(props.width as u16)
        .max(1);

    // 渲染高度（提前计算，供 use_effect 就近判断使用）
    let vis_height = area_rect.map(|r| r.height).unwrap_or(60).max(1);
```

- [ ] **Step 2: 将 total_visual_rows 移到 wrap_map 之后**

将 `total_visual_rows` 计算从第 569-575 行移到第 411 行 `wrap_map` 计算闭包之后、`use_event_handler` 之前。插入：

```rust
    };

    // ── 总视觉行数（供 use_effect 就近判断使用，提前到 hooks 之前）──
    let total_visual_rows: u16 = if wrap_map.is_empty() {
        if is_loading { 1 } else { 0 }
    } else {
        wrap_map
            .last()
            .map_or(0, |w| w.visual_row + w.visual_height)
    };

    {
```

- [ ] **Step 3: 原始 total_visual_rows 位置改为直接复用**

第 569-575 行已有同名变量被提前后，需移除重复计算。将：

```rust
    let total_visual_rows: u16 = if wrap_map.is_empty() {
        if is_loading { 1 } else { 0 }
    } else {
        wrap_map
            .last()
            .map_or(0, |w| w.visual_row + w.visual_height)
    };
```

替换为（声明注释说明语义未变）：

```rust
    // total_visual_rows 已在 hook 声明之前计算，此处复用同一值。
```

- [ ] **Step 4: 编译验证**

```bash
cd peri-tui && cargo check -p peri-tui 2>&1
```
Expected: 零编译错误。

---

### Task 2: 删除 auto_scroll / had_ct 及其上下游代码

**Files:**
- Modify: `peri-tui/src/kit/message_area.rs:274-277, 304-306, 509-513`

- [ ] **Step 1: 删除 use_state 声明**

删除第 274-275 行：

```rust
    let mut auto_scroll = hooks.use_state(|| true);
    let had_ct = hooks.use_state(|| false);
```

- [ ] **Step 2: 删除 should_follow_on_content_change 及 key change 块中的 auto_scroll 逻辑**

删除第 277 行：
```rust
    let should_follow_on_content_change = auto_scroll.get();
```

删除第 303-306 行（`// 内容变化时仅在当前仍处于跟随模式...` 注释及 `auto_scroll.set(true)` 逻辑）：
```rust
        // 内容变化时仅在当前仍处于跟随模式时继续滚到底；用户主动上滚后不抢回视口。
        if should_follow_on_content_change {
            auto_scroll.set(true);
        }
```

- [ ] **Step 3: 编译验证——此时应有 7 处 auto_scroll 未定义的错误**

```bash
cd peri-tui && cargo check -p peri-tui 2>&1
```
Expected: 编译失败，报 `auto_scroll` 未定义（事件 handler 和 use_effect 中的 7 处引用）。

---

### Task 3: 重写 use_effect 为就近判断

**Files:**
- Modify: `peri-tui/src/kit/message_area.rs:529-545`

- [ ] **Step 1: 替换 use_effect 闭包和依赖**

找到第 529-545 行的 use_effect 块，完整替换为：

```rust
    // I23-b：就近判断自动跟随——用户视口在底部附近时跟随内容增长，
    // 往上滚动超出 vis_height/2 后停止吸底。无需 auto_scroll flag，
    // 是否跟随完全由当前 scroll_y 与 bottom 的距离决定。
    hooks.use_effect(
        {
            let st = scroll_state;
            move || {
                let scroll_y = st.read().offset().y as u16;
                let total = total_visual_rows;
                let vh = vis_height;
                if total == 0 {
                    return;
                }
                let max_scroll = total.saturating_sub(vh);
                if scroll_y >= max_scroll {
                    // 已在或超出底部——scroll_to_bottom 是 no-op
                    return;
                }
                let distance = max_scroll.saturating_sub(scroll_y);
                let threshold = (vh / 2).max(5);
                if distance <= threshold {
                    st.write().scroll_to_bottom();
                }
            }
        },
        (entries_len, raw_ch),
    );
```

**设计说明**：
- `distance = max_scroll - scroll_y` 即用户距底部多少行
- `threshold = max(vis_height / 2, 5)` ——视口高度 50%（最少 5 行）
- `scroll_y >= max_scroll` 时已在底部，`scroll_to_bottom` 是 no-op，提前返回避免不必要的 state 写入
- 依赖 `(entries_len, raw_ch)` 在内容变化时触发，resize/todo 变化不触发也是正确的（只跟内容有关）

- [ ] **Step 2: 编译验证——此时只剩事件 handler 中的 auto_scroll 引用未解决**

```bash
cd peri-tui && cargo check -p peri-tui 2>&1
```
Expected: 还有若干 `auto_scroll` 未定义错误（来自事件 handler）。

---

### Task 4: 清理事件 handler 中的 auto_scroll 引用

**Files:**
- Modify: `peri-tui/src/kit/message_area.rs:456, 464, 490, 510-513, 521`

- [ ] **Step 1: 删除鼠标事件中的 auto_scroll.set(false)**

5 处删除：

**第 456 行**（Left button Down）——删除：
```rust
                                auto_scroll.set(false);
```

**第 464 行**（Left button Drag）——删除：
```rust
                                auto_scroll.set(false);
```

**第 490 行**（Left button Up）——删除：
```rust
                                auto_scroll.set(false);
```

**第 510-513 行**（ScrollDown/ScrollUp 后的条件 set）——将：
```rust
                // 仅在 auto_scroll 为 true 时才写入，避免每次鼠标事件触发不必要的 re-render
                if auto_scroll.get() {
                    auto_scroll.set(false);
                }
                return EventResult::Consumed;
```
替换为：
```rust
                return EventResult::Consumed;
```

**第 521 行**（Ctrl+↑↓HomeEnd 键盘事件）——删除：
```rust
                    auto_scroll.set(false);
```

- [ ] **Step 2: 编译验证**

```bash
cd peri-tui && cargo check -p peri-tui 2>&1
```
Expected: 零编译错误、零警告。

---

### Task 5: 构建与端到端验证

- [ ] **Step 1: 完整构建**

```bash
cd peri-tui && cargo build -p peri-tui 2>&1
```
Expected: 零编译错误。

- [ ] **Step 2: 流式输出跟随**

启动 TUI，提交一个简单 prompt（如 "write a short poem"），观察：
- 消息区从顶部开始出现
- 每收到一个 chunk，消息区自动滚到底部（这和新代码一致，老代码也跟）
- 最新流式文本始终在可见区域内

- [ ] **Step 3: 回看历史不抢滚动（核心验证）**

在流式输出期间按 `Ctrl+Up` 往上滚动超过半屏，观察后续 chunk 到达时：
- 不再自动滚到底部
- 停留在用户回看的历史位置

- [ ] **Step 4: 滚回底部恢复跟随**

接上一步，在用户回看历史后，按 `Ctrl+End` 滚回底部，观察后续 chunk：
- 恢复自动跟随
- 自动滚到底部

- [ ] **Step 5: 少量上滚不应断开跟随**

在流式输出期间按 1~2 次 `Ctrl+Up`（少量上滚，仍在底部区域），观察后续 chunk：
- 仍自动跟随到底部

- [ ] **Step 6: Commit**

```bash
git add peri-tui/src/kit/message_area.rs
git commit -m "refactor(tui): proximity-based auto-scroll instead of binary flag

Replace the binary auto_scroll bool with a distance-to-bottom check:
when user is within max(vis_height/2, 5) lines of the bottom, content
growth auto-scrolls to bottom; when user scrolls up beyond that zone,
auto-following stops naturally without a separate flag.

Remove auto_scroll/had_ct use_state — proximity is computed from
scroll_y position each frame, so scrolling back to bottom resumes
following automatically.

Co-Authored-By: deepseek-v4-pro <deepseek-ai@claude-code-best.win>"
```

---

### Task 6: 单测——就近判断逻辑验证

**Files:**
- Modify: `peri-tui/src/kit/message_area.rs`（测试区域，719 行之后）

- [ ] **Step 1: 新增 distance_threshold 辅助函数和测试**

在测试区域（第 718 行 `#[cfg(test)]` 块内）追加：

```rust
    // ── 就近判断阈值计算 ──

    /// 计算距底部的距离，及是否应自动跟到底部。
    ///
    /// 若 total=0 返回 false（无内容不滚动）。
    /// 若 scroll_y 已在底部（>= max_scroll）返回 true 但上层调用应走
    /// no-op 跳过（不做 scroll_to_bottom 写入避免 re-render 环路）。
    fn proximity_check(total: u16, scroll_y: u16, vis_height: u16) -> bool {
        if total == 0 {
            return false;
        }
        let max_scroll = total.saturating_sub(vis_height);
        if scroll_y >= max_scroll {
            // 已在或超出底部——上层应 no-op 跳过
            return false;
        }
        let distance = max_scroll.saturating_sub(scroll_y);
        let threshold = (vis_height / 2).max(5);
        distance <= threshold
    }

    #[test]
    fn test_proximity_at_bottom_should_follow() {
        let total = 100;
        let vis_height = 20;
        let scroll_y = total - vis_height; // 刚好在底部
        assert!(scroll_y >= total.saturating_sub(vis_height));
        // 用户已在底部时不调用 scroll_to_bottom（避免 no-op 写入）
    }

    #[test]
    fn test_proximity_within_half_viewport_should_follow() {
        let total = 100;
        let vis_height = 20;
        // 距底部 10 行 → threshold = 20/2 = 10 → 应跟随
        let scroll_y = total - vis_height - 10;
        assert!(proximity_check(total, scroll_y, vis_height));
    }

    #[test]
    fn test_proximity_beyond_half_viewport_should_not_follow() {
        let total = 100;
        let vis_height = 20;
        // 距底部 11 行 → threshold = 10 → 不应跟随
        let scroll_y = total - vis_height - 11;
        assert!(!proximity_check(total, scroll_y, vis_height));
    }

    #[test]
    fn test_proximity_near_top_should_not_follow() {
        let total = 200;
        let vis_height = 30;
        // 距底部 150 行，远远超过 threshold=15
        let scroll_y = 20;
        assert!(!proximity_check(total, scroll_y, vis_height));
    }

    #[test]
    fn test_proximity_small_viewport_minimum_threshold() {
        let total = 50;
        let vis_height = 6;
        // threshold = max(6/2, 5) = 5
        // 距底部 5 行 → 应跟随
        let scroll_y = total - vis_height - 5; // 39
        assert!(proximity_check(total, scroll_y, vis_height));
        // 距底部 6 行 → 不应跟随
        let scroll_y = total - vis_height - 6; // 38
        assert!(!proximity_check(total, scroll_y, vis_height));
    }

    #[test]
    fn test_proximity_empty_content_no_follow() {
        assert!(!proximity_check(0, 0, 20));
    }

    /// 回归：total < vis_height 时（内容未满一屏），max_scroll=0，
    /// 任何 scroll_y >= 0 都已在底部，不应触发 scroll_to_bottom 写入。
    #[test]
    fn test_proximity_content_smaller_than_viewport_at_bottom() {
        let total = 10;
        let vis_height = 30;
        // max_scroll = 0，scroll_y=0 时已在底部 → false（上层 no-op）
        assert!(!proximity_check(total, 0, vis_height));
    }
```

- [ ] **Step 2: 运行单测**

```bash
cargo test -p peri-tui --lib -- message_area::tests 2>&1
```
Expected: test result: ok. 7 passed.

- [ ] **Step 3: Commit**

```bash
git add peri-tui/src/kit/message_area.rs
git commit -m "test(tui): unit tests for proximity-based auto-scroll threshold

Test cases cover: at bottom, within half viewport, beyond threshold,
near top, small viewport minimum, empty content, content smaller than
viewport.

Co-Authored-By: deepseek-v4-pro <deepseek-ai@claude-code-best.win>"
```

---

## 设计决策记录

| 决策 | 理由 |
|------|------|
| 用 `(entries_len, raw_ch)` 而非 `new_key` 做 deps | `new_key` 含 `is_loading`/`todo_hash`，loading 切换和 todo 变更会误触发 effect。仅内容高度/条目变化才需判断是否跟随 |
| 阈值 `max(vis_height/2, 5)` | 用户原意「视口 50%」；min=5 防止小终端（<10 行）阈值过小 |
| `scroll_y >= max_scroll` 时提前返回，不调 `scroll_to_bottom` | `use_effect` 中 state write 仍会触发 re-render 调度。虽然大概率是 no-op（值未变），但显式跳过避免不必要的框架调度开销 |
| `total_visual_rows` 和 `vis_height` 提到 hooks 之前 | `use_effect` 闭包只捕获声明时的 snapshot，提前计算后即可捕获到准确值 |
| `total_visual_rows` 计算提前到 `wrap_map` 之后 | 原第 569 行在 `empty` 检查之后，移到 412 行之前不影响任何逻辑——`wrap_map` 后的 `use_event_handler`/`use_effect` 均不使用它 |
| 删除 `auto_scroll` 而非保留兼容 | 就近判断完全替代其功能，保留会造成两套机制并存导致行为不一致 |
| 鼠标/键盘事件不再设任何 flag | 用户滚动直接改变 `scroll_y`，下个 chunk 到达时 `use_effect` 自动重算距离，无需维护额外状态 |
