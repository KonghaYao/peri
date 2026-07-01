# Phase 2: 核心循环重构 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development

**Goal:** 拆分 main_loop::run 960 行巨型函数 + Effect 42→16 精简 + 快捷键分散化 + Render 去重优化

**Architecture:** 4 个独立 commit，按依赖顺序：拆分→精简→快捷键→优化。

**Tech Stack:** Rust 2021 + ratatui

---

### Task 2.1: main_loop::run 拆分为 7 个子函数

- [ ] 文件：`peri-tui/src/runtime/main_loop.rs` — 新增 PreEventSnapshot struct
- [ ] 提取 `capture_snapshot()` 函数（封装 L54-L128 的快照逻辑）
- [ ] 提取 `dispatch_sm()` 函数（封装 SM dispatch + Enter guard + Modal bypass）
- [ ] 提取 `dispatch_fallback()` 函数（封装 keyboard + ACP + mouse + session）
- [ ] 提取 `merge_effects()` 函数（SM + fallback 效果合并去重）
- [ ] 提取 `execute_effects()` 函数（原样保留 match 块，返回值含 quit flag）
- [ ] 提取 `sync_to_render()` 函数（rewind + drain + sync + render）
- [ ] 文件：`peri-tui/src/state_machine/state.rs` — 添加 `State::clone_for_id()` 方法
- [ ] 重写 `pub async fn run()`，从 960 行缩减至 ~60 行

- [ ] 验证：`cargo build -p peri-tui` && `cargo test -p peri-tui --lib -- main_loop`
- [ ] Commit：`refactor(Phase2.1): main_loop::run 拆分为 7 个子函数`（~60 行主循环）

### Task 2.2: Effect 枚举 42→16 精简

- [ ] 文件：`peri-tui/src/runtime/effect.rs` — 重新定义枚举为 16 变体
- [ ] **保留**：Render/SubmitMessage/PollAgent/Scroll/SendToAcp/CopyToClipboard/PasteText/ShowNotification/UpdateConfig/SwitchSession/OpenPanel/ClosePanel/CycleModel/CycleProvider/MemoryPanelOpenEditor/Quit
- [ ] **删除**：16 个输入编辑变体 + AdvanceSpinner + 3 个 Mouse + CyclePermissionMode + FocusBgBar + ToggleDiff + PollWorkflow + ClearTextSelection + PushSystemNote（合并到 ShowNotification）
- [ ] 文件：`peri-tui/src/runtime/apply_context.rs` — 删除 16 个 input effect arms + with_input 辅助函数
- [ ] 文件：`peri-tui/src/runtime/main_loop.rs` — 删除已删除 Effect 的 handler 分支
- [ ] 文件：`peri-tui/src/state_machine/transitions/idle.rs` — Tick 删除 AdvanceSpinner/PollWorkflow、Mouse 删除 click/drag/release、快捷键只 emit Render
- [ ] 文件：`peri-tui/src/state_machine/transitions/streaming.rs` — 同上
- [ ] 文件：`peri-tui/src/state_machine/transitions/modal.rs` — Tick 删除 AdvanceSpinner/PollWorkflow
- [ ] 文件：`peri-tui/src/state_machine/transitions/switching.rs` — PushSystemNote→ShowNotification
- [ ] 文件：`peri-tui/src/event/keyboard/normal_keys.rs` — 删除 input effect 发射，改为 textarea 直接处理
- [ ] 文件：所有 command/*.rs — PushSystemNote→ShowNotification（~24 处替换）

- [ ] 验证：`cargo build -p peri-tui` && `cargo test -p peri-tui --lib`（修复 ~30 个测试断言）
- [ ] Commit：`refactor(Phase2.2): Effect 42→16 变体精简，PushSystemNote→ShowNotification`

### Task 2.3: 快捷键决策分散化 + 删除 is_sm_handled_shortcut

- [ ] 文件：`peri-tui/src/state_machine/state.rs` — 新增 ShortcutClaim 三态枚举（SMOwns/FallbackOwns/Defer）
- [ ] 文件：`peri-tui/src/state_machine/transitions/idle.rs` — 新增 `owns_shortcut(key, snap) → ShortcutClaim`
- [ ] 文件：`peri-tui/src/state_machine/transitions/streaming.rs` — 新增 `owns_shortcut(key, snap) → ShortcutClaim`
- [ ] 文件：`peri-tui/src/runtime/main_loop.rs` — dispatch_fallback 中基于 claim 路由
- [ ] 文件：`peri-tui/src/runtime/main_loop.rs` — 删除 `is_sm_handled_shortcut()` 函数及 25 个测试
- [ ] 新增 6 个 owns_shortcut 测试
- [ ] 文件：`per-tui/CLAUDE.md` — 更新 TRAP 条目

- [ ] 验证：`cargo build -p peri-tui` && `cargo test -p peri-tui --lib`
- [ ] Commit：`refactor(Phase2.3): 快捷键分散化，删除 is_sm_handled_shortcut`

### Task 2.4: Render 去重优化 + 文档更新

- [ ] 文件：`peri-tui/src/runtime/main_loop.rs` — merge_effects 中 Render 去重从 Vec::contains 改为 iter().any()
- [ ] 文件：`peri-tui/src/runtime/main_loop.rs` — run() 开始处添加初始渲染
- [ ] 文件：`per-tui/CLAUDE.md` — 更新 Effect 计数为 16

- [ ] 验证：`cargo build -p peri-tui` && `cargo clippy -p peri-tui`
- [ ] Commit：`chore(Phase2.4): Render 去重优化 + 初始渲染 + 文档更新`
