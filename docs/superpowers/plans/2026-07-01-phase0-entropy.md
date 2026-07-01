# Phase 0: 熵减 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development

**Goal:** 删除 peri-tui 中所有死代码/死字段/冗余结构，降低后续重构干扰

**Architecture:** 纯删除操作 + 少量重构（ViewStore→自由函数、build_runtime 提取）。每个步骤独立可验证。

**Tech Stack:** Rust 2021 + ratatui + crossterm

**执行顺序:** 先做无依赖的删除操作（0f-0j），再做中等影响面（0b-0e），最后做影响面最大的 ViewStore 重构（0a）。

---

### Task 0f: 删除 render/throttle.rs

- [ ] 文件：`peri-tui/src/render/throttle.rs` — 整文件删除。与 apply_context 的 draw_if_needed 重复。
- [ ] 文件：`peri-tui/src/render/mod.rs` — 删除 `pub mod throttle;` 和注释掉的 use 行
- [ ] 命令：`git rm peri-tui/src/render/throttle.rs` 后编辑 mod.rs
- [ ] 验证：`cargo build -p peri-tui`
- [ ] Commit：`chore(entropy): delete render/throttle.rs (dead code, replaced by draw_if_needed)`

### Task 0g: 修复 thread_ops.rs 重复 alloc_collect()

- [ ] 文件：`peri-tui/src/app/thread_ops.rs:217-218` — 删除第 218 行重复的 `crate::alloc_config::alloc_collect();`
- [ ] 验证：`cargo build -p peri-tui`
- [ ] Commit：`chore(entropy): remove duplicate alloc_collect() call in new_thread()`

### Task 0h: 删除 open_thread_browser 死代码

- [ ] 文件：`peri-tui/src/app/thread_ops.rs:221-244` — 删除整个方法（创建 ThreadBrowser 后立即 drop）
- [ ] 文件：`peri-tui/src/app/thread_ops.rs` — 如果没有其他引用，删除 `use crate::thread::ThreadBrowser;`
- [ ] 验证：`cargo build -p peri-tui`
- [ ] Commit：`chore(entropy): delete dead open_thread_browser()`

### Task 0i: 清理 theme.rs — 删除 SUB_AGENT_BG

- [ ] 文件：`peri-tui/src/ui/theme.rs:73` — 删除 `SUB_AGENT_BG`（未被任何生产代码引用）
- [ ] 文件：`peri-tui/src/ui/theme.rs:52` — **保留** `POPUP_BG`（field_textarea.rs:24 仍使用）
- [ ] 验证：`cargo build -p peri-tui`
- [ ] Commit：`chore(entropy): delete unused SUB_AGENT_BG from theme.rs`

### Task 0j: 提取 build_runtime() 公共函数

- [ ] 文件：`peri-tui/src/main.rs` — 新增辅助函数：
```rust
fn build_runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .thread_stack_size(4 * 1024 * 1024)
        .enable_all()
        .build()
        .map_err(Into::into)
}
```
- [ ] 文件：`peri-tui/src/main.rs` — 替换 7 处重复的 tokio runtime 创建为 `build_runtime()?`
- [ ] 验证：`cargo build -p peri-tui`
- [ ] Commit：`chore(entropy): extract build_runtime() from 7 duplicate tokio runtime constructions`

### Task 0e: 简化 handle_acp_event

- [ ] 文件：`peri-tui/src/runtime/main_loop.rs:1105-1113` — 三个分支返回同一值，简化为：
```rust
let (_updated, _should_break, _should_return) = app.handle_acp_notification(notif, view_slice);
vec![Effect::Render]
```
- [ ] 验证：`cargo build -p peri-tui`
- [ ] Commit：`chore(entropy): simplify handle_acp_event (3 branches return same Effect::Render)`

### Task 0b: 删除 message_convert.rs

- [ ] 文件：`peri-tui/src/app/message_convert.rs` — 整文件删除（`#[allow(dead_code)]`）
- [ ] 文件：`peri-tui/src/app/mod.rs` — 删除 `mod message_convert;` 声明
- [ ] 验证：`cargo build -p peri-tui`
- [ ] Commit：`chore(entropy): delete dead app/message_convert.rs`

### Task 0c: 删除 PanelStateStub

- [ ] 文件：`peri-tui/src/panel/registry.rs` — 删除 PanelStateStub 结构体 + impl + 测试
- [ ] 文件：`peri-tui/src/panel/mod.rs` — 删除 `PanelStateStub` re-export
- [ ] 文件：`peri-tui/src/state_machine/transitions/modal.rs` — 在 tests 模块添加 inline TestPanel 替代
- [ ] 验证：`cargo build -p peri-tui` && `cargo test -p peri-tui --lib -- transitions::modal`
- [ ] Commit：`chore(entropy): delete PanelStateStub (all panels have concrete impls)`

### Task 0d: 删除 InputState 中 at_mention/slash_completion 字段

- [ ] 文件：`peri-tui/src/state_machine/input/mod.rs` — 删除字段 + Default + clear_buffer 清理
- [ ] 文件：`peri-tui/src/state_machine/transitions/idle.rs` — 删除 3 处引用（FileSuggestions 处理、Enter 防御、Esc 清除）
- [ ] 验证：`cargo build -p peri-tui` && `cargo test -p peri-tui --lib`
- [ ] Commit：`chore(entropy): remove dead InputState.at_mention/slash_completion fields`

### Task 0a: 删除 ViewStore 结构体

- [ ] 文件：`peri-tui/src/state_machine/view_store.rs` — 删除 ViewStore 结构体 + impl，保留自由函数（last_user_bubble_index / has_tool_cards_after / merge_preserving_local_notes）
- [ ] 文件：`peri-tui/src/state_machine/view_store.rs` — for_render 转为新自由函数 `view_for_render`
- [ ] 文件：`peri-tui/src/state_machine/mod.rs` — 更新 re-export
- [ ] 文件：`peri-tui/src/state_machine/view_store.rs` — 删除 ViewStore 依赖的测试，保留自由函数测试
- [ ] 验证：`cargo build -p peri-tui` && `cargo test -p peri-tui --lib -- view_store`
- [ ] Commit：`chore(entropy): delete ViewStore struct (state holds Vec<ViewModel> directly)`

## 验证清单

| 步骤 | 验证命令 |
|------|---------|
| 0f-0j | `cargo build -p peri-tui` |
| 0e | `cargo build -p peri-tui` |
| 0b | `cargo build -p peri-tui` |
| 0c | `cargo build -p peri-tui && cargo test -p peri-tui --lib -- transitions::modal` |
| 0d | `cargo build -p peri-tui && cargo test -p peri-tui --lib` |
| 0a | `cargo build -p peri-tui && cargo test -p peri-tui --lib -- view_store` |
| 最终 | `cargo test -p peri-tui --lib`（全部通过） |
