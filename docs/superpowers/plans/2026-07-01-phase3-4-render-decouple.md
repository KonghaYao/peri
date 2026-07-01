# Phase 3+4: 渲染统一 & 解耦加固 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development

**Goal:** Phase 3 — 删除 v1 渲染代码 + render/ 入口统一 + markdown 宽度修复 + 缓存优化<br>
Phase 4 — 面板 unsafe 修复 + session 切换统一 + app/ 模块重组 + 弹窗解耦

**Tech Stack:** Rust 2021 + ratatui

---

# Phase 3: 渲染统一

### Task 3.1: 删除 v1 message_render.rs

- [ ] 文件：`peri-tui/src/ui/mod.rs` — 删除 `pub mod message_render;` 声明
- [ ] 文件：`peri-tui/src/ui/message_render.rs` — `git rm`
- [ ] 文件：`peri-tui/src/ui/message_render_test.rs` — `git rm`
- [ ] 文件：`peri-tui/src/ui/headless_test.rs:699` — render_view_model 引用替换为 v2 render_v2_vm
- [ ] 验证：`cargo build -p peri-tui` && `cargo test -p peri-tui --lib`
- [ ] Commit：`phase3(3.1): remove v1 message_render.rs, migrate test refs to v2`

### Task 3.2: render/ 成为真正的渲染入口

- [ ] 文件：`peri-tui/src/render/mod.rs` — 新增 `draw(terminal, app, state, status_probe)` 函数（迁移 draw_now 的 ~80 行逻辑）
- [ ] 文件：`peri-tui/src/runtime/apply_context.rs` — draw_now 改为浅转发：`crate::render::draw(...)`
- [ ] 文件：`peri-tui/src/render/mod.rs` — 删除 FIXME/P5 skeleton 注释
- [ ] 验证：`cargo build -p peri-tui` && `cargo test -p peri-tui --lib`
- [ ] Commit：`phase3(3.2): migrate draw_now from apply_context to render/mod.rs`

### Task 3.3: Markdown 宽度修复

- [ ] 文件：`peri-tui/src/render/view_render.rs` — render_v2_vm 的 `_width` 改为 `width`，传递给 render_user_bubble/render_assistant_bubble
- [ ] 文件：`peri-tui/src/render/view_render.rs` — `parse_markdown_default(text)` → `parse_markdown(text, width)`
- [ ] 验证：`cargo build -p peri-tui`
- [ ] Commit：`phase3(3.3): pass width param to parse_markdown in v2 renderer`

### Task 3.4: 渲染缓存优化

- [ ] 文件：`peri-tui/src/state_machine/view_store.rs` — ViewStore 添加 render_version: u64 + apply_view_commit 递增
- [ ] 文件：`peri-tui/src/state_machine/state.rs` — State 添加 view_models_version() 方法
- [ ] 文件：`peri-tui/src/runtime/apply_context.rs` — draw_if_needed 增加 last_version 检查，version 不变则跳过
- [ ] 文件：`peri-tui/src/ui/main_ui/message_area.rs` — spinner/todo 行作为独立 footer，不参与全量缓存重建
- [ ] 验证：`cargo build -p peri-tui`
- [ ] Commit：`phase3(3.4): add view version cache optimization`

### Task 3.5: 渲染验证

- [ ] 文件：新建 `peri-tui/src/render/render_snapshot_test.rs` — TestBackend::buffer() 逐像素断言
- [ ] 文件：`peri-tui/src/app/agent_render.rs` — 保持 seed_v2_* 为 #[cfg(test)]，新测试改为从 State { view } 构造
- [ ] 验证：`cargo test -p peri-tui --lib -- render`
- [ ] Commit：`phase3(3.5): add TestBackend snapshot tests for v2 renderer`

---

# Phase 4: 解耦 & 加固

### Task 4.1: 面板 unsafe 修复

- [ ] 文件：`peri-tui/src/panel/read_context.rs` — PanelReadContext 的 services 字段改为 owned ServiceRegistrySnapshot（移除 &'a）
- [ ] 文件：`peri-tui/src/runtime/apply_context.rs:424` — build_v2_panel_read_context：直接 clone ServiceRegistrySnapshot，删除 thread_local + unsafe
- [ ] 文件：`peri-tui/src/runtime/main_loop.rs:1254` — build_panel_read_context：同样改为 owned
- [ ] 验证：`grep -rn "unsafe" peri-tui/src/runtime/apply_context.rs peri-tui/src/runtime/main_loop.rs`（输出为空）
- [ ] Commit：`phase4(4.1): replace thread_local unsafe with owned ServiceRegistrySnapshot`

### Task 4.2: Session 切换统一

- [ ] 文件：`peri-tui/src/app/mod.rs` — App 新增 `async fn switch_session(id, timeout) → Result`
  - 顺序：cancel → new_session（先建新）→ close_session（后关旧）→ cleanup
- [ ] 文件：`peri-tui/src/app/chat_session.rs` — ChatSession::drop 通过 channel 发送 close 通知
- [ ] 文件：`peri-tui/src/runtime/main_loop.rs` — Effect::SwitchSession 调用 switch_session + 失败回退
- [ ] 文件：`peri-tui/src/app/thread_ops.rs` — new_session/open_thread 改为调用 switch_session
- [ ] 验证：`cargo build -p peri-tui`
- [ ] Commit：`phase4(4.2): unify session switch with timeout + safe order (new→close)`

### Task 4.3: app/ 模块重组

- [ ] 创建子目录：`app/state/`, `app/events/`, `app/agent/`, `app/ui/`, `app/service/`
- [ ] git mv 迁移文件（保持 git 历史）：
  - state: ui_state.rs / global_ui_state.rs / service_registry.rs / session_manager.rs
  - events: agent_events_bg.rs / agent_events_oauth.rs
  - agent: agent_comm.rs→comm.rs / agent_submit.rs→submit.rs / agent_compact.rs→compact.rs / agent_render.rs→render.rs / agent_ops/→ops/
  - ui: interaction.rs / hint_ops.rs→hints.rs / at_mention/
  - service: command_system.rs / history_ops.rs→history.rs / provider.rs
- [ ] 文件：`peri-tui/src/app/mod.rs` — 更新所有 mod 声明为新路径
- [ ] 验证：`cargo build -p peri-tui` && 所有 `crate::app::Xxx` 引用不变
- [ ] Commit：`phase4(4.3): reorganize app/ into state/events/agent/ui/service submodules`

### Task 4.4: 弹窗解耦

- [ ] 文件：`peri-tui/src/ui/main_ui/mod.rs:356-404` — active_panel_height 中对 ModalKind::Interaction 走 handler.desired_height()
- [ ] 文件：`peri-tui/src/state_machine/handlers/` — 各 Handler 完善 desired_height 实现（已有 trait 方法，检查所有实现是否真实返回高度）
- [ ] 验证：`cargo build -p peri-tui`
- [ ] Commit：`phase4(4.4): delegate active_panel_height to Handler::desired_height`

### Task 4.5: 其他修复

- [ ] 文件：`peri-tui/src/app/mod.rs` — 新增 App::update_config() 统一配置入口（替代 main_loop 中的内联操作）
- [ ] 文件：`peri-tui/src/app/thread_ops.rs` — 滚动步长改为从 PeriConfig 读取配置项
- [ ] 文件：`peri-tui/src/panel/panels/mcp.rs` — 确认 PanelEffect::SendToAcp 映射完整
- [ ] 文件：`peri-tui/src/panel/panels/memory.rs` — Enter 键构造 Effect::MemoryPanelOpenEditor
- [ ] 验证：`cargo build -p peri-tui` && `cargo test -p peri-tui --lib`
- [ ] Commit：`phase4(4.5): config single entry point, scroll step config, MCP/Memory panel fixes`
