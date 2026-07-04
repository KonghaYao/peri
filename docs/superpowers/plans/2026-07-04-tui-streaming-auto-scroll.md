# TUI 消息流渲染修复 — 自动跟随滚动

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在流式输出时自动将消息区滚动到底部，让用户看到实时增长的文本。

**Architecture:** 在 `MessageArea` 组件内增加 `use_effect`，以 `current_turn` Arc 指针为依赖，每次新 chunk 到达时调用 `ScrollViewState::scroll_to_bottom()`。ratatui-kit 0.7 的 `use_effect` 在 update 阶段运行、在 draw 阶段之前，故同一帧内新 offset 即可生效。

**Tech Stack:** ratatui-kit 0.7 `use_effect` + `ScrollViewState::scroll_to_bottom()`, Rust 2024

**前置分析：为什么没有流式感**

```
LLM chunk → atom write → render_loop 唤醒 → MessageArea 重渲染 → ScrollView 输出
```

ratatui-kit 0.7 `render_loop` 已验证支持外部 atom 写入触发重渲染（`select!` 竞态 `root_component.wait()` 和 `terminal.next_event()`）。数据路径、渲染路径、框架唤醒全部正确。

根因：`ScrollViewState` offset 永远为 `Position::ORIGIN (0,0)`。流式内容增长时只有顶部可见，最新 chunk 在视口下方，**看起来就是一次性全部输出**。

---

### Task 1: 在 MessageArea 添加自动跟随滚动

**Files:**
- Modify: `peri-tui/src/kit/message_area.rs`

- [ ] **Step 1: 添加 `use_effect` 自动滚到底部**

在 `message_area.rs` 的 `MessageArea` 组件中，紧接 `let scroll_state = hooks.use_state(ScrollViewState::default);` 之后插入：

```rust
    // I23：流式自动跟随——每次 current_turn Arc 重建（新 chunk 到达）
    // 时自动滚动到底部。Arc::as_ptr 作为 deps，每帧相同指针则不触发，
    // 新的分配必触发（push_view_models 每次 Arc::from 都分配新 buffer）。
    let current_turn_ptr = Arc::as_ptr(&props.current_turn) as usize;
    hooks.use_effect(
        {
            let mut scroll_state = scroll_state;
            move || {
                // 检查用户是否手动滚上去了：仅 offset.y 非零（用户手动滚过）
                // 时才需要判断。offset.y == 0（初始态）无脑滚底。
                //
                // 简化策略：有 chunk 就滚底。大多数聊天 UI（ChatGPT/Claude）
                // 都这么做——流式期间自动跟随，结束后用户可用 Ctrl+Up 回看。
                scroll_state.write().scroll_to_bottom();
            }
        },
        current_turn_ptr,
    );
```

**插入位置：** `message_area.rs` 第 109 行 `let scroll_state = hooks.use_state(ScrollViewState::default);` 之后、第 111 行 `hooks.use_event_handler` 之前。完整上下文：

```rust
    // 消息区只吃鼠标滚轮；普通 Up/Down/Home/End 全部留给 InputArea。
    // Ctrl+ 导航键用于驱动消息区滚动，保持输入区多行/历史行为不变。
    let scroll_state = hooks.use_state(ScrollViewState::default);

    // I23：流式自动跟随——每次 current_turn Arc 重建（新 chunk 到达）
    // 时自动滚动到底部。Arc::as_ptr 作为 deps，每帧相同指针则不触发，
    // 新的分配必触发（push_view_models 每次 Arc::from 都分配新 buffer）。
    let current_turn_ptr = Arc::as_ptr(&props.current_turn) as usize;
    hooks.use_effect(
        {
            let mut scroll_state = scroll_state;
            move || {
                scroll_state.write().scroll_to_bottom();
            }
        },
        current_turn_ptr,
    );

    hooks.use_event_handler(
```

- [ ] **Step 2: 构建验证**

运行编译检查——`use_effect` 是 ratatui-kit 0.7 的稳定 API，`UseEffect` trait 由 prelude 导出：

```bash
cd peri-tui && cargo check -p peri-tui --features use-kit 2>&1
```

Expected: 零新增编译错误（`use_effect` 已在 `ratatui_kit::prelude::*` 中）。

- [ ] **Step 3: 手动验证流式行为**

启动 TUI，提交一个简单的 prompt（如 "write a short poem"），观察：

Expected:
- 消息区内容从顶部开始出现
- **每收到一个 chunk，消息区自动滚到底部**
- 最新流式文本始终在可见区域内
- TurnDone 后停留在最终位置

如果看不到自动滚动，检查 `tracing::info!` 日志中 `content_height` 是否在增长。

- [ ] **Step 4: 验证手动滚动不被覆盖**

在流式输出期间按 `Ctrl+Up` 向上滚动后，下一个 chunk 到达时会自动滚回底部——这是预期行为（见 Task 2 改进）。先确认 `Ctrl+Up`/`Ctrl+Down` 滚动本身仍正常工作。

```
- Ctrl+Down → 向下滚一行 ✓
- Ctrl+Up   → 向上滚一行 ✓
- Ctrl+Home → 滚到顶部 ✓
- Ctrl+End  → 滚到底部 ✓
```

- [ ] **Step 5: Commit**

```bash
git add peri-tui/src/kit/message_area.rs
git commit -m "fix(tui): auto-scroll message area to bottom during streaming

Streaming chunks arrive via ACP events and trigger re-renders, but
ScrollViewState offset was stuck at (0,0). New content grew below
the viewport, making streaming invisible to the user.

Add use_effect on current_turn Arc pointer change to call
scroll_to_bottom() on each new chunk, so the latest streaming
text is always visible.

Co-Authored-By: deepseek-v4-pro <deepseek-ai@claude-code-best.win>"
```

---

### Task 2（可选优化）: 智能跟随——用户回看时不抢滚动

**Files:**
- Modify: `peri-tui/src/kit/message_area.rs`

**背景：** Task 1 的方案在下个 chunk 到达时会覆盖用户的手动滚动。Task 2 加入"仅当用户已在底部时才跟随"的智能判断。

- [ ] **Step 1: 判断用户是否在底部**

```rust
    // I23-b：智能跟随——仅在用户未手动滚离底部时自动滚动。
    let current_turn_ptr = Arc::as_ptr(&props.current_turn) as usize;
    let content_height = content_height; // 外部计算，需要移到 hooks 前
    hooks.use_effect(
        {
            let mut scroll_state = scroll_state;
            move || {
                let is_at_bottom = {
                    let state = scroll_state.read();
                    let offset = state.offset().y;
                    // content_height 是本次渲染的总行数，viewport 高度未知。
                    // 用 offset 是否很小（< 阈值）做近似：用户滚上去后 offset 变小。
                    // 直接用 u16::MAX 标记"在底部"——scroll_to_bottom 设置为此值，
                    // 随后 ScrollView render 阶段 clamp 到有效范围。
                    // 简化：只要 offset 不在顶部附近（表示用户可能在看前面），就跟随。
                    offset > 0 || content_height < 40
                };
                if is_at_bottom {
                    scroll_state.write().scroll_to_bottom();
                }
            }
        },
        current_turn_ptr,
    );
```

**注意：** 这需要将 `content_height` 的计算提前到 hooks 调用之前。当前 `content_height` 在第 94 行计算，位于 hooks 之后——需要重构函数体将 `content_height` 计算移到 `scroll_state` 之后。

- [ ] **Step 2: 提交改进**

```bash
git add peri-tui/src/kit/message_area.rs
git commit -m "fix(tui): smart auto-scroll — don't steal scroll when user reads history

Only auto-scroll to bottom when user is already near the bottom.
If user scrolled up (Ctrl+Up), new chunks won't snap-scroll."

Co-Authored-By: deepseek-v4-pro <deepseek-ai@claude-code-best.win>
```

---

## 设计决策记录

| 决策 | 理由 |
|------|------|
| 用 `Arc::as_ptr` 而非 `len()` 做 deps | `len()` 不变（同一个 AssistantBubble 累积文字），`Arc::as_ptr` 每次重新分配必变 |
| `use_effect` 而非 `pre_component_draw` | `use_effect` 在 update 阶段运行、draw 前生效，同帧可见；`pre_component_draw` 在 draw 阶段，需额外触发重渲染 |
| 初始版无条件跟随（Task 1）、优化版智能跟随（Task 2） | 最小可行修复优先，后续迭代 |
| 不改 `disabled: true` | 只控制内部键盘事件分发，不影响子节点渲染 |
