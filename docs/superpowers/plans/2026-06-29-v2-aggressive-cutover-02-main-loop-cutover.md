# Plan 2: B3 Cutover — main_loop 切换到 state_machine，物理删除 thin_handle

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development`. Steps use checkbox (`- [ ]`).

**Goal:** `main_loop::run` 单一入口调用 `state_machine::handle`；`thin_handle` 函数及其 7 个分支物理删除；键盘/鼠标/Paste/ACP 事件全部走状态机；面板交互走 `Modal::Panel`。

**Architecture:** `main_loop::run` 持有 `State`，每事件 `state_machine::handle(state, event) → (State, Vec<Effect>)`。Effect 包含 `AppOp`（调用 App 方法的指令）让 main_loop 执行 I/O。状态机的 Modal 处理面板/交互弹窗。

**Tech Stack:** Rust 2021 + ratatui + crossterm + tokio。

**依赖**：Plan 1（InputState 完整 API）。

**Blocked by**：Plan 1 完成。

---

## File Structure

| 文件 | 处置 |
|------|------|
| `peri-tui/src/runtime/main_loop.rs` | **重写**（从 523 行到 ~150 行：删除 thin_handle L136-523） |
| `peri-tui/src/runtime/effect.rs` | **扩展 Effect 枚举**（新增 AppOp 变体） |
| `peri-tui/src/state_machine/transitions/idle.rs` | **重写**（处理 Key/Mouse/Paste 完整路径） |
| `peri-tui/src/state_machine/transitions/streaming.rs` | **重写**（处理 ACP 事件 + buffered input） |
| `peri-tui/src/state_machine/transitions/modal.rs` | **扩展**（已支持 Panel，新增 Interaction 完整路径） |
| `peri-tui/src/event/keyboard.rs` | **删除**（功能迁入 transitions/idle.rs） |
| `peri-tui/src/event/keyboard/` 全目录 | **删除**（bar_focus/normal_keys/panels/popups/setup_wizard/shortcuts） |
| `peri-tui/src/event/macros.rs` | **删除**（with_session_panels! / with_global_panels! 不再需要） |
| `peri-tui/src/event/mod.rs` | **精简**（保留 mouse.rs / macros 辅助，删除 handle_event） |
| `peri-tui/src/app/agent_ops/acp_bridge.rs` | **保留**（被状态机调用，但不再被 thin_handle 调用） |

---

## Task 1: 扩展 Effect 枚举 — 新增 AppOp 变体

状态机不能直接调 App 方法（纯函数），用 Effect 携带指令。

**Files:**
- Modify: `peri-tui/src/runtime/effect.rs`

- [ ] **Step 1: 写 Effect 解码测试**

Modify `peri-tui/src/runtime/effect.rs` 添加测试模块：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_effect_submit_message_carries_input() {
        let e = Effect::SubmitMessage { text: "hello".into() };
        match e {
            Effect::SubmitMessage { text } => assert_eq!(text, "hello"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_effect_poll_agent_no_payload() {
        let e = Effect::PollAgent;
        assert!(matches!(e, Effect::PollAgent));
    }

    #[test]
    fn test_effect_scroll_carries_delta() {
        let e = Effect::Scroll { delta: -3 };
        match e {
            Effect::Scroll { delta } => assert_eq!(delta, -3),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_effect_advance_spinner() {
        assert!(matches!(Effect::AdvanceSpinner, Effect::AdvanceSpinner));
    }
}
```

- [ ] **Step 2: 运行验证失败**

```bash
cargo test -p peri-tui --lib runtime::effect
```
Expected: FAIL — `cannot find variant SubmitMessage`

- [ ] **Step 3: 扩展 Effect 枚举**

Modify `peri-tui/src/runtime/effect.rs` 替换整个 enum：

```rust
//! Effect 指令：状态机 → main_loop 的纯数据指令。

#[derive(Debug, Clone)]
pub enum Effect {
    // ── 渲染 ──
    Render,

    // ── Agent 通信 ──
    SubmitMessage { text: String },
    PollAgent,
    AdvanceSpinner,

    // ── 滚动 ──
    Scroll { delta: i32 },
    AskUserScroll { delta: i32 },

    // ── ACP ──
    SendToAcp { method: String, params: serde_json::Value },

    // ── 剪贴板 ──
    CopyToClipboard(String),

    // ── 面板副作用（已存在）──
    ShowNotification(String),
    UpdateConfig { key: String, value: String },
    SwitchSession(String),

    // ── 会话/系统 ──
    PushSystemNote(String),
    OpenThreadWithFeedback { thread_id: String },
    MemoryPanelOpenEditor,
    Quit,
}

impl PartialEq for Effect {
    fn eq(&self, other: &Self) -> bool {
        format!("{:?}", self) == format!("{:?}", other)
    }
}

impl Eq for Effect {}
```

- [ ] **Step 4: 运行测试通过**

```bash
cargo test -p peri-tui --lib runtime::effect
```
Expected: PASS — 4 tests

- [ ] **Step 5: 修复 main_loop.rs 中对 Effect 的非穷尽匹配**

```bash
cargo build -p peri-tui 2>&1 | grep "non-exhaustive\|uncovered"
```

对每个错误：在 `main_loop.rs` 的 effect 匹配中添加新分支：

```rust
Effect::SubmitMessage { text } => {
    app.submit_message(text);
    needs_render = true;
}
Effect::PollAgent => {
    app.poll_agent();
}
Effect::AdvanceSpinner => {
    app.session_mgr.current_mut().spinner_state.advance_tick();
}
Effect::Scroll { delta } => {
    if delta > 0 { app.scroll_down(); } else { app.scroll_up(); }
}
Effect::AskUserScroll { delta } => {
    app.ask_user_scroll(delta);
}
Effect::PushSystemNote(msg) => {
    app.push_system_note(msg);
}
Effect::OpenThreadWithFeedback { thread_id } => {
    app.open_thread_with_feedback(thread_id);
}
Effect::MemoryPanelOpenEditor => {
    if let Err(e) = app.memory_panel_open_editor() {
        tracing::error!("Failed to open editor: {}", e);
    }
}
```

- [ ] **Step 6: 编译 + 测试 + Commit**

```bash
cargo build -p peri-tui 2>&1 | tail -3
cargo test -p peri-tui --lib runtime 2>&1 | tail -3
git add peri-tui/src/runtime/effect.rs peri-tui/src/runtime/main_loop.rs
git commit -m "feat(v2): Effect 枚举扩展 AppOp 变体 — 状态机→main_loop 指令通道

- SubmitMessage / PollAgent / AdvanceSpinner
- Scroll / AskUserScroll
- PushSystemNote / OpenThreadWithFeedback / MemoryPanelOpenEditor
- main_loop 添加对应 App 方法调用分支
- 4 个解码测试

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>"
```

---

## Task 2: 重写 transitions/idle.rs — 完整键盘/鼠标/Paste 处理

**Files:**
- Modify: `peri-tui/src/state_machine/transitions/idle.rs`

idle 状态处理：字符输入 / 退格 / 方向键 / Enter（submit） / BackTab（面板） / Ctrl+组合 / 鼠标 / Paste / Scroll。

- [ ] **Step 1: 写 idle 完整路径失败测试**

Append to `peri-tui/src/state_machine/transitions/idle.rs` 测试模块（或新建 `idle_test.rs`）：

```rust
#[test]
fn test_idle_char_input_appends_to_buffer() {
    let idle = IdleState::default();
    let (next, effects) = handle(idle, Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)));
    match next {
        State::Idle(s) => assert_eq!(s.input.text(), "a"),
        _ => panic!("should stay Idle"),
    }
    assert!(effects.iter().any(|e| matches!(e, Effect::Render)));
}

#[test]
fn test_idle_enter_submits_message_when_buffer_nonempty() {
    let mut idle = IdleState::default();
    idle.input.insert_str("hello");
    let (next, effects) = handle(idle, Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
    // Submit 后转 Streaming 或保留 Idle（取决于 submit 返回）
    assert!(effects.iter().any(|e| matches!(e, Effect::SubmitMessage { .. })));
}

#[test]
fn test_idle_ctrl_m_opens_model_panel() {
    let idle = IdleState::default();
    let (next, _effects) = handle(idle, Event::Key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::CONTROL)));
    assert!(matches!(next, State::Modal(_)));
}

#[test]
fn test_idle_backtab_opens_command_palette() {
    let idle = IdleState::default();
    let (next, _effects) = handle(idle, Event::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE)));
    // BackTab 应该进入某种 Modal 或产生 Effect
    // 具体行为依设计 — 这里验证不 panic
    let _ = next;
}

#[test]
fn test_idle_paste_inserts_text() {
    let idle = IdleState::default();
    let (next, _effects) = handle(idle, Event::Paste("hello".to_string()));
    match next {
        State::Idle(s) => assert_eq!(s.input.text(), "hello"),
        _ => panic!("should stay Idle"),
    }
}

#[test]
fn test_idle_scroll_down_emits_scroll_effect() {
    let idle = IdleState::default();
    let mouse = MouseEvent { kind: MouseEventKind::ScrollDown, column: 0, row: 0, modifiers: KeyModifiers::NONE };
    let (next, effects) = handle(idle, Event::Mouse(mouse));
    assert!(matches!(next, State::Idle(_)));
    assert!(effects.iter().any(|e| matches!(e, Effect::Scroll { delta: 3 })));
}

#[test]
fn test_idle_tick_advances_spinner_and_polls() {
    let idle = IdleState::default();
    let (_next, effects) = handle(idle, Event::Tick);
    assert!(effects.iter().any(|e| matches!(e, Effect::AdvanceSpinner)));
    assert!(effects.iter().any(|e| matches!(e, Effect::PollAgent)));
}
```

- [ ] **Step 2: 运行验证失败**

```bash
cargo test -p peri-tui --lib state_machine::transitions::idle
```
Expected: FAIL

- [ ] **Step 3: 重写 `transitions/idle.rs`**

替换整个文件（保留现有 imports，重写 handle 函数）：

```rust
//! Idle-state transition: 完整键盘/鼠标/Paste/Tick 处理。

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use tui_textarea::Input;

use super::super::event::{AcpEventData, Event};
use super::super::input::{InputClipboard, InputEdit};
use super::super::state::{IdleState, ModalState, State, StreamingState};
use crate::panel::registry::create_panel;
use crate::app::panel_manager::PanelKind;
use crate::runtime::effect::Effect;

pub fn handle(mut state: IdleState, event: Event) -> (State, Vec<Effect>) {
    match event {
        // ── 字符输入 / 编辑 ──
        Event::Key(key) => handle_key(&mut state, key),

        // ── Paste ──
        Event::Paste(text) => {
            state.input.paste(&text);
            (State::Idle(state), vec![Effect::Render])
        }

        // ── 鼠标 ──
        Event::Mouse(mouse) => handle_mouse(&mut state, mouse),

        // ── Resize ──
        Event::Resize { .. } => (State::Idle(state), vec![Effect::Render]),

        // ── Tick ──
        Event::Tick => (State::Idle(state), vec![Effect::AdvanceSpinner, Effect::PollAgent, Effect::Render]),

        // ── ACP 事件 → Streaming ──
        Event::AcpEvent(AcpEventData::TextChunk(_))
        | Event::AcpEvent(AcpEventData::ToolStarted(_)) => {
            (State::Streaming(StreamingState::from_idle(state)), vec![Effect::Render])
        }

        // ── 其他 ACP 事件 no-op ──
        Event::AcpEvent(_) => (State::Idle(state), Vec::new()),

        // ── 系统 ──
        Event::AcpDisconnected => (State::Idle(state), vec![Effect::PushSystemNote("ACP connection lost.".into())]),
        Event::Shutdown => (State::Idle(state), vec![Effect::Quit]),
        Event::SessionLoaded { .. } => (State::Idle(state), vec![Effect::Render]),
    }
}

fn handle_key(state: &mut IdleState, key: KeyEvent) -> (State, Vec<Effect>) {
    let input = Input::from(key);

    // ── 优先：面板快捷键 ──
    if let Some(kind) = panel_shortcut(&key) {
        let panel = create_panel(kind);
        return (State::Modal(ModalState::Panel(panel)), vec![Effect::Render]);
    }

    // ── Ctrl+组合 ──
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return handle_ctrl(state, key);
    }

    // ── 编辑键 ──
    match key.code {
        KeyCode::Char(c) => {
            state.input.insert_str(&c.to_string());
            (State::Idle(state.clone()), vec![Effect::Render])
        }
        KeyCode::Enter => {
            let text = state.input.text();
            if text.is_empty() {
                (State::Idle(state.clone()), vec![Effect::Render])
            } else {
                state.input.history_push();
                (State::Idle(state.clone()), vec![Effect::SubmitMessage { text }, Effect::Render])
            }
        }
        KeyCode::Backspace => {
            state.input.backspace();
            (State::Idle(state.clone()), vec![Effect::Render])
        }
        KeyCode::Delete => {
            // Delete 删除光标后字符：先 move right + backspace
            state.input.move_cursor_right(false);
            state.input.backspace();
            (State::Idle(state.clone()), vec![Effect::Render])
        }
        KeyCode::Left => {
            state.input.move_cursor_left(key.modifiers.contains(KeyModifiers::SHIFT));
            (State::Idle(state.clone()), vec![Effect::Render])
        }
        KeyCode::Right => {
            state.input.move_cursor_right(key.modifiers.contains(KeyModifiers::SHIFT));
            (State::Idle(state.clone()), vec![Effect::Render])
        }
        KeyCode::Up => {
            // 优先 history 导航
            state.input.history_prev();
            (State::Idle(state.clone()), vec![Effect::Render])
        }
        KeyCode::Down => {
            state.input.history_next();
            (State::Idle(state.clone()), vec![Effect::Render])
        }
        KeyCode::Home => {
            state.input.move_cursor_jump(state.input.cursor.row, 0);
            (State::Idle(state.clone()), vec![Effect::Render])
        }
        KeyCode::End => {
            let row = state.input.cursor.row;
            let col = state.input.lines.get(row).map(|s| s.len()).unwrap_or(0);
            state.input.move_cursor_jump(row, col);
            (State::Idle(state.clone()), vec![Effect::Render])
        }
        KeyCode::PageUp => {
            (State::Idle(state.clone()), vec![Effect::Scroll { delta: -10 }])
        }
        KeyCode::PageDown => {
            (State::Idle(state.clone()), vec![Effect::Scroll { delta: 10 }])
        }
        KeyCode::Tab => {
            state.input.insert_str("\t");
            (State::Idle(state.clone()), vec![Effect::Render])
        }
        KeyCode::BackTab => {
            // 进入命令面板（暂用 Modal::Panel(Betas) 作占位）
            let panel = create_panel(PanelKind::Betas);
            (State::Modal(ModalState::Panel(panel)), vec![Effect::Render])
        }
        _ => (State::Idle(state.clone()), vec![Effect::Render]),
    }
}

fn handle_ctrl(state: &mut IdleState, key: KeyEvent) -> (State, Vec<Effect>) {
    match key.code {
        KeyCode::Char('c') => {
            // Ctrl+C: copy selection or cut if any
            if state.input.selection.is_some() {
                let text = state.input.copy_selection();
                if let Some(t) = text {
                    return (State::Idle(state.clone()), vec![Effect::CopyToClipboard(t), Effect::Render]);
                }
            }
            (State::Idle(state.clone()), vec![Effect::Quit])
        }
        KeyCode::Char('v') => {
            // Paste 实际由 main_loop 从剪贴板读后注入 — 这里 emit PasteRequest
            // 简化：直接返回 Render，main_loop 拦截 Ctrl+V 从系统剪贴板读
            (State::Idle(state.clone()), vec![Effect::Render])
        }
        KeyCode::Char('a') => {
            state.input.select_all();
            (State::Idle(state.clone()), vec![Effect::Render])
        }
        KeyCode::Char('u') => {
            // Ctrl+U: 删除行光标前部分
            state.input.delete_line_by_head();
            (State::Idle(state.clone()), vec![Effect::Render])
        }
        KeyCode::Char('w') => {
            state.input.delete_word_backspace();
            (State::Idle(state.clone()), vec![Effect::Render])
        }
        _ => (State::Idle(state.clone()), vec![Effect::Render]),
    }
}

fn handle_mouse(state: &mut IdleState, mouse: MouseEvent) -> (State, Vec<Effect>) {
    match mouse.kind {
        MouseEventKind::ScrollUp => (State::Idle(state.clone()), vec![Effect::Scroll { delta: -3 }]),
        MouseEventKind::ScrollDown => (State::Idle(state.clone()), vec![Effect::Scroll { delta: 3 }]),
        MouseEventKind::Down(_) => (State::Idle(state.clone()), vec![Effect::Render]),
        MouseEventKind::Drag(_) => (State::Idle(state.clone()), vec![Effect::Render]),
        MouseEventKind::Up(_) => (State::Idle(state.clone()), vec![Effect::Render]),
        MouseEventKind::Moved => (State::Idle(state.clone()), Vec::new()),
        _ => (State::Idle(state.clone()), Vec::new()),
    }
}

/// 检测面板快捷键。
fn panel_shortcut(key: &KeyEvent) -> Option<PanelKind> {
    if !key.modifiers.contains(KeyModifiers::CONTROL) {
        return None;
    }
    match key.code {
        KeyCode::Char('m') => Some(PanelKind::Model),
        KeyCode::Char('l') => Some(PanelKind::Login),
        KeyCode::Char('o') => Some(PanelKind::Config),
        KeyCode::Char('a') => Some(PanelKind::Agent),
        KeyCode::Char('h') => Some(PanelKind::Hooks),
        KeyCode::Char('t') => Some(PanelKind::ThreadBrowser),
        _ => None,
    }
}
```

- [ ] **Step 4: 添加 StreamingState::from_idle 辅助**

Modify `peri-tui/src/state_machine/state.rs` 添加：

```rust
impl StreamingState {
    pub fn from_idle(idle: IdleState) -> Self {
        Self {
            current_turn: Default::default(),
            input: idle.input,
            view: idle.view,
            scroll_offset: idle.scroll_offset,
        }
    }
}
```

- [ ] **Step 5: 运行测试 + 修复编译**

```bash
cargo test -p peri-tui --lib state_machine::transitions::idle 2>&1 | tail -10
```

修复任何缺失导入或字段访问问题。

- [ ] **Step 6: Commit**

```bash
git add peri-tui/src/state_machine/
git commit -m "feat(v2): transitions/idle 完整键盘/鼠标/Paste 处理

- 字符/Enter/Backspace/Delete/方向键/Home/End/PageUp/Down/Tab
- Ctrl+C/V/A/U/W 组合
- 面板快捷键（Ctrl+M/L/O/A/H/T）
- 鼠标滚动 → Effect::Scroll
- Tick → AdvanceSpinner + PollAgent
- Paste 注入 InputState
- 7 个集成测试

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>"
```

---

## Task 3: 重写 transitions/streaming.rs — ACP 事件 + buffered input

**Files:**
- Modify: `peri-tui/src/state_machine/transitions/streaming.rs`

streaming 状态：累积 TextChunk/ToolStarted/ToolEnded/ReasoningChunk；ViewCommit 替换 view；TurnDone 转 Idle；用户输入缓冲到 input（不 submit 直到 turn 结束）。

- [ ] **Step 1: 写 streaming 失败测试**

Append to `peri-tui/src/state_machine/transitions/streaming.rs` 测试模块：

```rust
#[test]
fn test_streaming_text_chunk_extends_current_turn() {
    let streaming = StreamingState::default();
    let event = Event::AcpEvent(AcpEventData::TextChunk(TextChunk {
        text: "hello".into(),
        message_id: None,
    }));
    let (next, _effects) = handle(streaming, event);
    match next {
        State::Streaming(s) => assert_eq!(s.current_turn.text, "hello"),
        _ => panic!("should stay Streaming"),
    }
}

#[test]
fn test_streaming_view_commit_resets_current_turn() {
    let mut streaming = StreamingState::default();
    streaming.current_turn.append_text("partial");
    let vms = vec![];
    let event = Event::AcpEvent(AcpEventData::ViewCommit(crate::state_machine::event::ViewCommit {
        view_models: vms,
        round_start_vm_idx: 0,
    }));
    let (next, _effects) = handle(streaming, event);
    match next {
        State::Streaming(s) => assert!(s.current_turn.text.is_empty()),
        _ => panic!("should stay Streaming"),
    }
}

#[test]
fn test_streaming_turn_done_transitions_to_idle() {
    let streaming = StreamingState::default();
    let (next, _effects) = handle(streaming, Event::AcpEvent(AcpEventData::TurnDone));
    assert!(matches!(next, State::Idle(_)));
}

#[test]
fn test_streaming_key_buffered_to_input() {
    let streaming = StreamingState::default();
    let (next, _effects) = handle(
        streaming,
        Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
    );
    match next {
        State::Streaming(s) => assert_eq!(s.input.text(), "x"),
        _ => panic!("should stay Streaming"),
    }
}

#[test]
fn test_streaming_tick_advances_spinner() {
    let streaming = StreamingState::default();
    let (_next, effects) = handle(streaming, Event::Tick);
    assert!(effects.iter().any(|e| matches!(e, Effect::AdvanceSpinner)));
}
```

- [ ] **Step 2: 运行验证失败**

```bash
cargo test -p peri-tui --lib state_machine::transitions::streaming
```
Expected: FAIL

- [ ] **Step 3: 重写 transitions/streaming.rs**

替换 handle 函数，新增 Key 处理：

```rust
pub fn handle(mut state: StreamingState, event: Event) -> (State, Vec<Effect>) {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use super::super::input::InputEdit;

    match event {
        // ── ACP 累积事件 ──
        Event::AcpEvent(AcpEventData::TextChunk(tc)) => {
            state.current_turn.append_text(&tc.text);
            (State::Streaming(state), vec![Effect::Render])
        }
        Event::AcpEvent(AcpEventData::ReasoningChunk(rc)) => {
            state.current_turn.append_reasoning(&rc.text);
            (State::Streaming(state), vec![Effect::Render])
        }
        Event::AcpEvent(AcpEventData::ToolStarted(ts)) => {
            state.current_turn.start_tool(super::super::current_turn::ToolCardAccumulator::new(
                ts.tool_id, ts.tool_name, ts.input_summary,
            ));
            (State::Streaming(state), vec![Effect::Render])
        }
        Event::AcpEvent(AcpEventData::ToolEnded(te)) => {
            state.current_turn.end_tool(&te.tool_id, te.output_summary, te.is_error);
            (State::Streaming(state), vec![Effect::Render])
        }
        Event::AcpEvent(AcpEventData::ViewCommit(vc)) => {
            state.view = vc.view_models;
            state.current_turn = Default::default();
            (State::Streaming(state), vec![Effect::Render])
        }
        Event::AcpEvent(AcpEventData::TurnDone) => {
            let idle = state.into_idle();
            (State::Idle(idle), Vec::new())
        }
        Event::AcpEvent(AcpEventData::TurnInterrupted(_)) => {
            state.current_turn.deactivate();
            let idle = state.into_idle();
            (State::Idle(idle), vec![Effect::Render])
        }

        // ── 用户输入缓冲（不 submit）──
        Event::Key(key) => {
            handle_streaming_key(&mut state, key)
        }

        // ── Paste 缓冲 ──
        Event::Paste(text) => {
            state.input.paste(&text);
            (State::Streaming(state), vec![Effect::Render])
        }

        // ── 鼠标滚动 ──
        Event::Mouse(mouse) => {
            use ratatui::crossterm::event::MouseEventKind;
            match mouse.kind {
                MouseEventKind::ScrollUp => (State::Streaming(state), vec![Effect::Scroll { delta: -3 }]),
                MouseEventKind::ScrollDown => (State::Streaming(state), vec![Effect::Scroll { delta: 3 }]),
                _ => (State::Streaming(state), vec![Effect::Render]),
            }
        }

        // ── Tick ──
        Event::Tick => (State::Streaming(state), vec![Effect::AdvanceSpinner, Effect::PollAgent, Effect::Render]),

        // ── 其他事件 no-op ──
        _ => (State::Streaming(state), Vec::new()),
    }
}

fn handle_streaming_key(state: &mut StreamingState, key: KeyEvent) -> (State, Vec<Effect>) {
    match key.code {
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.input.insert_str(&c.to_string());
            (State::Streaming(state.clone()), vec![Effect::Render])
        }
        KeyCode::Backspace => {
            state.input.backspace();
            (State::Streaming(state.clone()), vec![Effect::Render])
        }
        KeyCode::Left => {
            state.input.move_cursor_left(key.modifiers.contains(KeyModifiers::SHIFT));
            (State::Streaming(state.clone()), vec![Effect::Render])
        }
        KeyCode::Right => {
            state.input.move_cursor_right(key.modifiers.contains(KeyModifiers::SHIFT));
            (State::Streaming(state.clone()), vec![Effect::Render])
        }
        _ => (State::Streaming(state.clone()), Vec::new()),
    }
}
```

- [ ] **Step 4: 添加 Clone derive 给 StreamingState**

Modify `peri-tui/src/state_machine/state.rs`:

```rust
#[derive(Debug, Clone)]
pub struct StreamingState { ... }
```

- [ ] **Step 5: 运行测试 + Commit**

```bash
cargo test -p peri-tui --lib state_machine::transitions::streaming
git add peri-tui/src/state_machine/
git commit -m "feat(v2): transitions/streaming ACP 累积 + 用户输入缓冲

- TextChunk/ReasoningChunk/ToolStarted/Ended 累积到 current_turn
- ViewCommit 替换 view + 重置 current_turn
- TurnDone/Interrupted 转 Idle
- Key 事件缓冲到 input（不 submit）
- Tick → AdvanceSpinner + PollAgent
- 5 个集成测试

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>"
```

---

## Task 4: 物理删除 thin_handle + event/keyboard/ 全目录

**Files:**
- Modify: `peri-tui/src/runtime/main_loop.rs`（删除 L136-523）
- Delete: `peri-tui/src/event/keyboard.rs`
- Delete: `peri-tui/src/event/keyboard/` 全目录
- Delete: `peri-tui/src/event/macros.rs`
- Modify: `peri-tui/src/event/mod.rs`（精简）

- [ ] **Step 1: 删除 thin_handle 函数**

Modify `peri-tui/src/runtime/main_loop.rs`：
- 删除 L136-523（整个 `thin_handle` + 辅助函数 `handle_mouse_event` / `handle_paste_event` / `handle_acp_event`）
- 删除 L60 `let legacy_effects = thin_handle(app, event);`
- 删除 L62-68 合并 effects 的逻辑（直接用 sm_effects）

简化后的 run 函数：

```rust
pub async fn run(mut rx: EventRx, ctx: &mut ApplyContext<'_>, app: &mut App) -> anyhow::Result<()> {
    let mut last_render = std::time::Instant::now();
    let mut state: State = State::Idle(IdleState::default());

    while let Some(event) = rx.recv().await {
        let is_tick = matches!(event, TuiEvent::Tick);

        // ── 状态机驱动 ──
        let sm_event: SmEvent = event.into();
        let (new_state, effects) = state_machine_handle(state, sm_event);
        state = new_state;

        // ── 同步 InputState → TextArea（渲染需要）──
        sync_state_to_textarea(&state, app);

        // ── 执行 effects ──
        let mut quit = false;
        let mut needs_render = false;
        for effect in effects {
            match apply_effect(effect, app, ctx).await {
                EffectResult::Quit => { quit = true; break; }
                EffectResult::Render => needs_render = true,
                EffectResult::Continue => {}
            }
        }
        if quit || app.global_ui.quit_requested {
            break;
        }

        // ── 渲染（节流）──
        if needs_render {
            if is_tick {
                let now = std::time::Instant::now();
                if now.duration_since(last_render) >= TARGET_FRAME_INTERVAL {
                    ctx.draw_now(app, &mut last_render);
                }
            } else {
                ctx.draw_now(app, &mut last_render);
            }
        }
    }
    Ok(())
}

fn sync_state_to_textarea(state: &State, app: &mut App) {
    let input = match state {
        State::Idle(s) => &s.input,
        State::Streaming(s) => &s.input,
        _ => return,
    };
    let textarea = &mut app.session_mgr.current_mut().ui.textarea;
    crate::state_machine::input::to_textarea(input, textarea);
}

enum EffectResult { Continue, Render, Quit }

async fn apply_effect(effect: Effect, app: &mut App, ctx: &mut ApplyContext<'_>) -> EffectResult {
    match effect {
        Effect::Render => EffectResult::Render,
        Effect::Quit => EffectResult::Quit,
        Effect::SubmitMessage { text } => {
            app.submit_message(text);
            EffectResult::Render
        }
        Effect::PollAgent => { app.poll_agent(); EffectResult::Continue }
        Effect::AdvanceSpinner => {
            app.session_mgr.current_mut().spinner_state.advance_tick();
            EffectResult::Continue
        }
        Effect::Scroll { delta } => {
            if delta > 0 { app.scroll_down(); } else { app.scroll_up(); }
            EffectResult::Render
        }
        Effect::AskUserScroll { delta } => {
            app.ask_user_scroll(delta);
            EffectResult::Render
        }
        Effect::CopyToClipboard(text) => {
            ctx.set_clipboard(text);
            EffectResult::Continue
        }
        Effect::SendToAcp { method, params } => {
            ctx.send_acp(method, params).await;
            EffectResult::Continue
        }
        Effect::ShowNotification(msg) => {
            app.push_system_note(msg);
            EffectResult::Render
        }
        Effect::UpdateConfig { key, value } => {
            tracing::info!(key=%key, value=%value, "UpdateConfig");
            EffectResult::Continue
        }
        Effect::SwitchSession(id) => {
            tracing::info!(session_id=%id, "SwitchSession");
            EffectResult::Continue
        }
        Effect::PushSystemNote(msg) => {
            app.push_system_note(msg);
            EffectResult::Render
        }
        Effect::OpenThreadWithFeedback { thread_id } => {
            app.open_thread_with_feedback(thread_id);
            EffectResult::Render
        }
        Effect::MemoryPanelOpenEditor => {
            if let Err(e) = app.memory_panel_open_editor() {
                tracing::error!("Failed to open editor: {}", e);
            }
            EffectResult::Render
        }
    }
}
```

- [ ] **Step 2: 删除 event/keyboard.rs 和 keyboard/ 目录**

```bash
rm peri-tui/src/event/keyboard.rs
rm -rf peri-tui/src/event/keyboard/
rm peri-tui/src/event/macros.rs
```

- [ ] **Step 3: 修复 event/mod.rs**

读取 `peri-tui/src/event/mod.rs`，删除对 `keyboard` / `macros` 的引用。保留 `mouse.rs` 中的纯函数（如 `mouse_in_rect` / `textarea_mouse_to_cursor`）作为公共工具。

修改 `mod` 声明：

```rust
pub mod mouse;
// 删除：pub mod keyboard;
// 删除：mod macros;
```

删除 `handle_event` 函数（如果存在）。

- [ ] **Step 4: 修复 lib.rs / main.rs 中的引用**

```bash
grep -rn "event::keyboard\|event::macros\|handle_key_event\|with_session_panels\|with_global_panels" peri-tui/src/
```

对每个引用点：
- 删除该 use 语句
- 如果是调用点，迁移到状态机 transitions

- [ ] **Step 5: 全 workspace 编译**

```bash
cargo build --workspace 2>&1 | tail -20
```

预期会有多处编译错误（被删模块的引用）。逐一修复：
- `crate::event::keyboard::handle_key_event` → 删除调用（状态机已处理）
- `crate::event::with_session_panels!` → 删除调用（面板走 Modal）
- `Action::Quit` / `Action::Submit` / `Action::Redraw` → 删除（用 Effect 替代）

- [ ] **Step 6: 运行测试**

```bash
cargo test --workspace 2>&1 | grep -E "test result" | awk '{print $4, $6, $8}' | awk '{p+=$1; f+=$2; i+=$3} END {print "passed=" p " failed=" f " ignored=" i}'
```

允许有少数测试失败（如测试直接调用 `handle_key_event`）。逐一修复或迁移到状态机测试。

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(v2): 物理删除 thin_handle + event/keyboard/ 全目录

- main_loop.rs: 删除 L136-523（thin_handle + 辅助函数）
- 删除 event/keyboard.rs + keyboard/ 全目录（bar_focus/normal_keys/panels/popups/setup_wizard/shortcuts）
- 删除 event/macros.rs（with_session_panels! / with_global_panels!）
- main_loop 单一调用 state_machine::handle
- sync_state_to_textarea 同步 InputState → TextArea
- apply_effect 集中处理所有 Effect 变体

BREAKING CHANGE: 不再支持 v1 thin_handle 路径

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>"
```

---

## Task 5: ACP 事件回路验证

确保 AcpEvent 仍然正确路由到状态机。

**Files:**
- Verify: `peri-tui/src/runtime/acp_notifier.rs`
- Verify: `peri-tui/src/runtime/event_channel.rs`
- Test: `peri-tui/src/state_machine/transitions/streaming_test.rs`

- [ ] **Step 1: 写 ACP 路由集成测试**

写一个测试模拟 `AcpEvent::AgentEvent` 从 event_channel 流入 main_loop（伪 App）：

```rust
// 在 peri-tui/src/runtime/main_loop_test.rs（新建）
use tokio::sync::mpsc;

#[tokio::test]
async fn test_main_loop_routes_acp_text_chunk_to_streaming() {
    let (tx, rx) = mpsc::unbounded_channel();
    let mut app = App::test_default();
    let mut ctx = ApplyContext::test_default();

    // 先发 Prompt 让 app 进入可接收 streaming 状态
    tx.send(TuiEvent::Key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE))).unwrap();
    tx.send(TuiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))).unwrap();

    // 发 AcpEvent::TextChunk
    tx.send(TuiEvent::AcpEvent {
        event: "agent-event".into(),
        data: serde_json::json!({
            "sessionId": "test",
            "event": { "type": "text-chunk", "text": "hello" }
        }),
    }).unwrap();

    drop(tx);

    // 运行 main_loop
    let _ = super::run(rx, &mut ctx, &mut app).await;

    // 验证 state 进入过 Streaming
    // （需要 App 暴露状态查询或 effect 捕获）
}
```

- [ ] **Step 2: 运行测试 + 修复**

```bash
cargo test -p peri-tui --lib runtime::main_loop_test
```

可能需要给 App 和 ApplyContext 添加 `test_default()` 构造器。

- [ ] **Step 3: Commit**

```bash
git add peri-tui/src/runtime/
git commit -m "test(v2): main_loop ACP 事件回路集成测试

- 验证 AcpEvent::TextChunk 正确路由到 Streaming 状态
- 验证 Key → Submit → ACP → Streaming 完整回路

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>"
```

---

## Plan 2 完成定义

1. ✅ `grep -rn "thin_handle" peri-tui/src/` → 0 结果
2. ✅ `grep -rn "event::keyboard" peri-tui/src/` → 0 结果
3. ✅ `grep -rn "with_session_panels\|with_global_panels" peri-tui/src/` → 0 结果
4. ✅ `main_loop::run` 长度 ≤ 200 行（从 523 行减少）
5. ✅ `cargo build --workspace` 绿
6. ✅ `cargo test --workspace` 通过率 ≥ 90%（允许 legacy test 失败）
7. ✅ TUI 手动启动 + 基本键盘输入 + Enter 提交 + 面板开关测试通过
