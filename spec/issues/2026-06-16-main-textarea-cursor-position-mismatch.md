# 主输入框光标显示位置与实际编辑位置不同步

**状态**：Partial
**优先级**：中
**创建日期**：2026-06-16

## 问题描述

在主聊天输入框（textarea）中编辑文本时，光标的**显示位置**与**实际编辑位置**出现错位。用户通过键盘左右箭头移动光标，或在输入文字过程中，光标渲染在错误的列/位置上，与下一次输入会落到哪里不一致。该问题跨平台（macOS / Windows / Linux 均复现），且中英文输入场景下都会出现，与平台或字符宽度无关。

## 症状详情

| 维度 | 现象 |
|------|------|
| **触发位置** | TUI 底部主聊天输入框（textarea） |
| **触发交互** | （a）按键盘 `←` / `→` 箭头键主动移动光标；（b）正常输入文字的过程中 |
| **实际行为** | 光标在屏幕上的渲染位置与下一次输入会落到文字里的实际编辑位置不同步（用户看到光标在 A，但实际编辑/插入发生在 B） |
| **期望行为** | 光标渲染位置始终精确反映当前编辑位置 |
| **复现频率** | 用户描述为「平常输入、变换光标位置」时即可出现 |
| **平台范围** | macOS、Windows、Linux 均复现 |
| **输入语言范围** | 中文（CJK）与英文均复现 |
| **回归来源** | 该异常由 commit `c3204bd6`（merge `fix/windows-pty-and-crlf`）引入；该 commit 同时是另两个 textarea/输入相关 issue 的修复 commit |

## 复现条件

- **复现频率**：常态性出现（用户在「平常输入、变换光标位置」时即观察到）
- **触发步骤**：
  1. 在任意平台启动 `peri-tui`（`cargo run -p peri-tui`）
  2. 在底部主输入框中输入若干文字（中英文均可）
  3. 按键盘左右箭头移动光标，或在输入过程中观察光标
  4. 观察：光标的渲染位置与实际编辑位置不同步
- **环境**：
  - OS：macOS / Windows / Linux 均复现
  - 终端：用户未指定具体终端类型
  - 输入法/字符集：中英文均复现，未观察到字符宽度相关性

### 现象 2（2026-06-16，修复 #1 后发现的独立残留问题）

**触发条件**：主输入框中**某行文字宽度超过视口宽度**（textarea 触发水平滚动，`top_col > 0`），光标在该长行的右半部分时按 `←`。

| 维度 | 现象 |
|------|------|
| **触发位置** | 主聊天输入框（textarea），任何长行（行宽 > 视口宽度） |
| **触发交互** | 在长行末尾或右半部分按 `←` |
| **实际行为** | 光标**实际**左移了一格（编辑位置正确），但**渲染位置视觉不动**。用户以为按键没响应，连续按多次（约等于"光标当前位置到视口左边界的距离"次），光标才"突然动起来" |
| **期望行为** | 每次 `←` 视觉上能看到光标左移一格 |
| **复现频率** | 必现（只要 `top_col > 0` 且光标不在视口最左） |
| **平台范围** | 跨平台（与现象 1 同） |
| **与现象 1 的关系** | 现象 1（stash bug）修复后，现象 2 才显现出来——之前 stash bug 让光标偶尔跳两格，掩盖了视觉错位 |

## 涉及文件

> 注：以下文件基于用户描述（主输入框的光标与显示逻辑）+ commit `c3204bd6` 的实际改动范围列出。具体根因留给后续 `fix-issue` / `diagnose` 排查。

- `peri-tui/src/event/mod.rs` —— 该 commit 对本文件改动 162 行（最大改动），含键盘事件分发、鼠标滚轮过滤、光标位置相关逻辑，是用户描述「光标逻辑和显示逻辑被改动」最可能的落点
- `peri-tui/src/app/field_textarea.rs` —— 该 commit 改动了 `configure_style()`（背景由透明 `Color::Reset` 改为 `POPUP_BG` 纯黑，并修改了与光标位置/水平滚动相关的注释说明）。注：本组件用于表单输入框（Login/Config/Setup Wizard），需确认主输入框是否复用同一套样式或光标渲染逻辑
- 主聊天输入框的渲染与光标位置计算代码 —— 用户描述中明确指向，但具体文件需排查确认（可能涉及 textarea 渲染、字符宽度→列宽映射、光标行列计算）

## 关联

- 由 commit `c3204bd6`（`Merge fix/windows-pty-and-crlf`）引入
- 该 commit 同时是以下两个已有 issue 的修复 commit：
  - `spec/issues/2026-06-16-form-textarea-overlay-offset-windows.md`（状态：Verified）
  - `spec/issues/2026-06-16-mouse-wheel-scroll-textarea-priority-windows.md`（状态：Fixed，待验证）
  - 以及 `spec/issues/2026-06-16-mcp-failed-status-bar-persistent-error.md`
- 本次异常可能是上述修复（尤其是 `event/mod.rs` 的 162 行改动）引入的回归

## 调研记录（2026-06-16）

### 根因定位

**Bug 位置**：`peri-tui/src/event/mod.rs:99-101` 的 early return 路径不消费 `EVENT_STASH`。

**机制**：

commit `c3204bd6` 引入 `EVENT_STASH`（thread_local 单槽）来暂存 `coalesce_mouse_events` 期间遇到的非 scroll 事件。但 stash 的**消费点**（`take`）放在 `next_event` 内部 loop 开头（第 106 行），**晚于**所有 early return 路径：

| early return 路径 | 行号 | 是否消费 stash |
|------|------|------|
| `quit_pending_since` 超时 | 62-67 | ❌ |
| `rewind_pending_since` 超时 | 70-75 | ❌ |
| `mouse_available` 探测分支 | 79-97 | ❌ |
| **`poll(50ms)` 超时返回 `Ok(None)`** | **99-101** | ❌ ← **常态路径，最常触发** |

### 触发场景

1. 队列：`[MouseScroll, Key(Left)]`（鼠标在某个 panel 滚动的同时按 ←，跨平台都会发生）
2. `next_event` #1：`read` 到 `MouseScroll` → `coalesce_mouse_events` drain 读到 `Key(Left)` → **stash 它** → 返回 `MouseScroll`，滚动消息区
3. 队列此时为空
4. `next_event` #2：**没走到 stash take 那行**，`poll(50ms)` 直接超时 → `return Ok(None)` → stash 不消费
5. `next_event` #3 ~ #N：同样超时，stash 一直挂着
6. 用户看不到光标响应，又按一次 ←
7. `next_event` #N+1：`poll` 命中 → `take` stash（**旧的 ←**）→ 光标左移
8. `next_event` #N+2：`read` 队列里的 **新 ←** → 光标再次左移

**视觉表现**：按一次 ← 光标左移两次，或光标位置与预期差一格。跨平台（任何鼠标都会触发 MouseScroll）、与字符宽度无关（中英文都出现）——与用户描述完全吻合。

### 排除的次要怀疑点

| 怀疑点 | 结论 |
|------|------|
| `thread_local` + multi-thread runtime 跨 worker 风险 | 排除——`rt.block_on(async { run_app(...) })` 顶层 future 始终在主线程执行，stash 实际单线程访问 |
| `EVENT_STASH` 单槽覆盖 | 排除——`next_event` loop 开头必先 `take`，单槽够用 |
| Windows `filter_mouse_wheel_keys` 的 stash | 跨平台问题，Windows 专属代码不是本次回归源（用户在 macOS/Linux 也复现） |

### 修复方向

把 stash `take` 提到 `next_event` 函数最开头，**所有 early return 之前**。如果 stash 有事件，跳过 `poll` 直接走后续的 `coalesce_mouse_events` → `detect_simulated_paste` → `handle_event` 路径。

具体策略：
- 函数开头先 `EVENT_STASH.with(|s| s.borrow_mut().take())`
- 如果 `Some(stashed)`：跳过 `mouse_available` 探测 / `poll(50ms)` / quit_pending / rewind_pending 中的所有"超时返回"分支（这些都是 UI 计时逻辑，与事件无关），直接把 stashed 当作"已读取的事件"送入 coalesce/filter/handle 路径
- 如果 `None`：保持原有 early return + poll 路径不变

注意：`quit_pending_since` / `rewind_pending_since` 的超时触发 `Action::Redraw`，**应当先于 stash 处理**（UI 计时优先级高于事件），所以这两个分支保留在 stash take 之前；只把 `mouse_available` 探测和 `poll(50ms)` 超时这两个"等待事件"的路径改为优先消费 stash。

### 涉及行（修复点）

- `peri-tui/src/event/mod.rs:59-137` —— `next_event` 函数体
- 关键改动行：第 99-101 行（poll 超时）和第 105-124 行（loop take stash）

## 调研记录 #2（2026-06-16，现象 2 根因）

### 根因定位

**Bug 位置**：`peri-tui/src/app/ime.rs:65-66` `textarea_cursor_pos` 函数中的水平滚动推断逻辑。

```rust
let scroll_col = cursor_display_col.saturating_sub(visible_width.saturating_sub(1));
let visible_col = cursor_display_col.saturating_sub(scroll_col);
```

### 机制

当前推断**总是假设光标在视口最右列**（即 `visible_col = visible_width - 1`），相当于"textarea 始终把光标贴在右边界"。这与 tui-textarea 的实际滚动行为不一致。

**tui-textarea-2 0.11.0 实际行为**（`widget.rs:81-90` `next_scroll_top`）：

```rust
fn next_scroll_top(prev_top, cursor, len) {
    if cursor < prev_top { cursor }                       // 光标移出左边界 → 滚动跟随
    else if cursor >= prev_top + len { cursor + 1 - len } // 移出右边界 → 滚动跟随
    else { prev_top }                                     // 在视口内 → scroll 不变
}
```

关键差异：**光标在视口内移动时，tui-textarea 的 `top_col` 不变**，光标实际在视口内左右移动；但 peri-tui 的推断每次都重算 `scroll_col = cursor - (width - 1)`，把光标强制贴回视口最右。

### 视觉表现（数值推演）

设想 `visible_width = 80`，长文字行 100 列：

| 光标位置（display col） | tui-textarea 实际 (scroll_col, visible_col) | peri-tui 推断 | 视觉差异 |
|------|------|------|------|
| 100 | (21, 79) | (21, 79) | ✓ 正确（光标在行尾时唯一吻合点） |
| 99 | (21, **78**) | (20, **79**) | ✗ 光标视觉不动 |
| 98 | (21, **77**) | (19, **79**) | ✗ 光标视觉不动 |
| ... | ... | ... | ... |
| 22 | (21, 1) | (0, 22) | ✗ 严重错位 |
| 21 | (0, 21) | (0, 21) | ✓ 突然正确 |

**用户感知**：按 `←` 时光标**视觉上不动**；连续按到光标接近 `top_col` 边界时（约 79 次），光标"突然动起来"。完全符合用户描述"往左要按好几个"。

### 已确认的事实

- tui-textarea-2 0.11.0 中 `Viewport::scroll_top()` 方法是 `pub` 的，但 `TextArea::viewport` 字段是 `pub(crate)`——**外部无法直接读取 textarea 当前的 scroll 状态**
- `pub fn scroll(&mut self, ...)` 是手动滚动 API，不是状态读取
- 因此 peri-tui 无法在渲染时拿到 textarea 的真实 `top_col`，只能从光标位置反推——而反推在视口中间场景下信息不足

### 修复方向（用户选择「只记录不修」）

| 方案 | 评估 |
|------|------|
| 在 app state 维护 sticky `last_scroll_col`，每帧用 `next_scroll_top` 逻辑更新 | 完全模拟 textarea 行为，准确；需要状态跟踪 + 拦截每个 textarea 实例 |
| fork/vendor 一份 tui-textarea-2，加 `pub fn scroll_top(&self) -> (u16, u16)` getter | 最准确、改动小；引入依赖维护成本 |
| 自实现 textarea widget | 最彻底；改动最大，不适合本次修复 |

**当前选择**：暂不修复。`textarea` 主线场景（行宽 ≤ 视口宽度）下 `textarea_cursor_pos` 推断正确，仅长行（`top_col > 0`）下存在视觉错位，不影响编辑正确性（编辑位置始终准确）。

### 涉及文件（现象 2 修复点，待修复时使用）

- `peri-tui/src/app/ime.rs:45-74` —— `textarea_cursor_pos` 函数
- `peri-tui/src/ui/main_ui/mod.rs:276` —— `set_cursor_position` 调用入口

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-06-16 | — | Open | agent | 创建 |
| 2026-06-16 | Open | Open | agent | 追加调研记录，根因定位为 EVENT_STASH 在 early return 路径下不消费 |
| 2026-06-16 | Open | Fixed | coder agent | 修复 #1：将 EVENT_STASH 的 take 从内层 loop 提到函数开头（quit/rewind 计时之后），让 poll(50ms) 超时路径不再跳过 stash 消费 |
| 2026-06-16 | Fixed | Partial | agent | 修复 #1 经实机验证暴露出独立的残留问题：`textarea_cursor_pos`（`peri-tui/src/app/ime.rs:65-66`）水平滚动推断错误（现象 2）。用户选择暂不修复，仅记录 |

## 修复记录

### 修复 #1（2026-06-16）

- **操作人**：coder agent
- **用户原意**：主输入框光标位置应与按键操作同步，按一次 ← 光标左移一格，不应出现"左移两次"或位置错位
- **修复内容**：
  - `peri-tui/src/event/mod.rs` `next_event` 函数（约 59-137 行）：
    - 在 `quit_pending_since` / `rewind_pending_since` 两个 UI 计时 early return 之后立即 `EVENT_STASH.with(|s| s.borrow_mut().take())`，把 stash 消费提到所有"等待事件"路径之前
    - `mouse_available` 探测分支加 `&& stashed.is_none()` 短路：probe 仅启动时跑一次，stash 此时不可能有值，短路纯属防御性
    - `poll(50ms)` 超时分支改为 `if let Some(stashed) = stashed { stashed } else { 原 poll + loop 路径 }`：stash 有事件直接用，跳过 poll；stash 空时保持原行为
    - stash 命中分支不再重新走 Windows `filter_mouse_wheel_keys`——stash 中的 Key(Up/Down) 是用户真实按键，二次 peek 可能在队列恰好有 MouseScroll 时被误判为孤儿 wheel key 丢弃
- **涉及文件**：`peri-tui/src/event/mod.rs`
- **验证**：
  - `cargo build -p peri-tui` 通过
  - `cargo test -p peri-tui --lib` 全过（652 passed; 0 failed）
- **验证状态**：待用户实机验证（单元测试无法覆盖 crossterm 事件队列时序）
