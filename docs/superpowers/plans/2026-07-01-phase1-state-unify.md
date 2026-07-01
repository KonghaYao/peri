# Phase 1: 状态单源化 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development

**Goal:** 消除输入双数据源 + 消息三源分裂，InputState 成为唯一逻辑模型

**Architecture:** 分两阶段执行：1a（InputOp 枚举 + InputState::apply）→ 1b（keyboard fallback 迁移到 InputOp）

**Tech Stack:** Rust 2021 + ratatui + crossterm

**建议执行顺序:** Task 1→2→3→4→5→14（测试先于迁移）→8→6→7→11→15

---

### Task 1: 定义 InputOp 枚举

- [ ] 文件：`peri-tui/src/state_machine/input/mod.rs` — 新增定义：
```rust
#[derive(Debug, Clone, PartialEq)]
pub enum InputOp {
    InsertChar(char),
    DeletePrevChar,
    DeleteNextChar,
    DeletePrevWord,
    DeleteToLineStart,
    SelectAll,
    Clear,
    InsertNewline,
    SetText(String),
    InsertStr(String),
    MoveCursor(CursorDirection),
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CursorDirection { Left, Right, Up, Down, LineStart, LineEnd }
```
- [ ] 验证：`cargo check -p peri-tui`

### Task 2: 添加 InputState::apply 方法

- [ ] 文件：`peri-tui/src/state_machine/input/edit.rs` — 文件末尾新增 `impl InputState { pub fn apply(&mut self, op: InputOp) -> Vec<Effect> { ... } }`
- [ ] 每个变体对应现有编辑方法（InsertChar→type_char, DeletePrevChar→delete_prev_char 等）
- [ ] 验证：`cargo check -p peri-tui`

### Task 3: Effect 枚举替换 16 变体 → ApplyInputOp

- [ ] 文件：`peri-tui/src/runtime/effect.rs` — 将 16 输入 Effect 变体替换为单个：
```rust
ApplyInputOp(super::input::InputOp),
```
- [ ] 删除：TypeChar/DeletePrevChar/DeleteNextChar/DeletePrevWord/DeleteToLineStart/SelectAllInput/ClearInputBuffer/InsertNewline/CursorLeft/CursorRight/CursorLineStart/CursorLineEnd/ReplaceTextarea/InsertStr/CursorUp/CursorDown
- [ ] 验证：`cargo check -p peri-tui`（可能有其他引用待修复的编译错误，正常）

### Task 4: main_loop 处理 Effect::ApplyInputOp

- [ ] 文件：`peri-tui/src/runtime/main_loop.rs:403` 附近 — 在 Effect::Render 之后添加：
```rust
Effect::ApplyInputOp(op) => {
    match &mut state {
        State::Idle(idle) => { idle.input.apply(op); }
        State::Streaming(s) => { s.input.apply(op); }
        _ => tracing::warn!("ApplyInputOp in non-Idle/Streaming state"),
    }
    needs_render = true;
}
```
- [ ] 验证：`cargo check -p peri-tui`

### Task 5: 删除 apply_context.rs 中输入 Effect 的 16 个 arm

- [ ] 文件：`peri-tui/src/runtime/apply_context.rs` — 删除 16 个 Effect 变体的 match arm（TypeChar 到 CursorDown）
- [ ] 同时可删除 `with_input` 辅助函数（不再有调用者）
- [ ] 验证：`cargo check -p peri-tui`

### Task 14: 新增测试（在迁移前做好安全网）

- [ ] 文件：`peri-tui/src/state_machine/input/edit.rs` — 在 phase1_tests 添加 InputState::apply 的 12 个单元测试
- [ ] 文件：`peri-tui/src/state_machine/input/sync.rs` — 新增 4 个 to_textarea 边界测试：
  - test_to_textarea_empty_buffer_cursor
  - test_to_textarea_col_byte_oob_clamps
  - test_roundtrip_mixed_ascii_cjk_emoji
  - test_to_textarea_large_buffer
- [ ] 验证：`cargo test -p peri-tui --lib`

### Task 8: keyboard fallback 迁移到 InputOp

- [ ] 文件：`peri-tui/src/event/keyboard/normal_keys.rs` — 替换所有旧 Effect 变体为 `Effect::ApplyInputOp(InputOp::...)`
  - InsertNewline → ApplyInputOp(InsertNewline)
  - DeletePrevWord/DeleteToLineStart/DeletePrevChar/DeleteNextChar/SelectAll → 对应 InputOp
  - CursorLeft/Right/LineStart/LineEnd → MoveCursor(对应方向)
  - TypeChar(c) → ApplyInputOp(InsertChar(c))
  - InsertStr(text) → ApplyInputOp(InsertStr(text))
  - Ctrl+C ClearInputBuffer → ApplyInputOp(Clear)
- [ ] 添加 import：`use crate::state_machine::input::{InputOp, CursorDirection};`
- [ ] 文件：`peri-tui/src/event/keyboard.rs:154,159` — ReplaceTextarea/InsertStr → ApplyInputOp
- [ ] 验证：`cargo check -p peri-tui`

### Task 6: 删除 from_textarea 函数

- [ ] 文件：`peri-tui/src/state_machine/input/sync.rs` — 删除 from_textarea 函数体
- [ ] 文件：`peri-tui/src/state_machine/input/sync.rs` — 删除依赖 from_textarea 的 6 个测试
- [ ] 文件：`peri-tui/src/state_machine/input/mod.rs` — 删除 from_textarea 的 pub use
- [ ] 文件：`peri-tui/src/runtime/main_loop.rs:677` — ClosePanel 的 from_textarea 兜底改为 `InputState::default()`
- [ ] 验证：`cargo check -p peri-tui`

### Task 7: 2b 同步简化（mouse-only）

- [ ] 文件：`peri-tui/src/runtime/main_loop.rs:223` — effect_did_mutate_textarea 改为注释标注 mouse-only
- [ ] PasteText 处理器：删除 textarea 双写，只写 InputState
- [ ] 保留 MouseTextareaClick/Drag 处的 `effect_did_mutate_textarea = true`
- [ ] 验证：`cargo check -p peri-tui`

### Task 11: pending_v2_notes drain 移至 SM Tick

- [ ] 文件：`peri-tui/src/runtime/effect.rs` — 新增 Effect::DrainPendingNotes
- [ ] 文件：`peri-tui/src/state_machine/transitions/idle.rs:58` — Tick 处理添加 DrainPendingNotes
- [ ] 文件：`peri-tui/src/state_machine/transitions/streaming.rs:185` — 同上
- [ ] 文件：`peri-tui/src/runtime/main_loop.rs` — 添加 DrainPendingNotes handler，删除旧的 drain 块
- [ ] 更新 idle.rs 测试：test_tick 断言加 DrainPendingNotes
- [ ] 验证：`cargo test -p peri-tui --lib -- idle`

### Task 15: 最终验证

- [ ] `cargo build -p peri-tui`（零错误）
- [ ] `cargo clippy -p peri-tui -- -D warnings`
- [ ] `cargo test -p peri-tui --lib`（全部通过）
- [ ] 全局搜索残留引用：`grep -rn "Effect::TypeChar\|Effect::DeletePrevChar\|..." peri-tui/src/`（零匹配）
- [ ] Commit：`refactor(Phase1): unify input model — InputState single source of truth`
