# v2 架构剩余 Gap 审计

> 审计日期：2026-06-29 | 分支：`feature/v2-architecture`
> 全 workspace 测试：3203 通过，0 失败

---

## 总览

| Gap | 名称 | 优先级 | 需删除文件 | 需修改文件 | 阻断关系 |
|-----|------|--------|-----------|-----------|---------|
| 1 | Plan 2: main_loop cutover | CRITICAL | 6 | 2-3 | 阻断 Gap 2 |
| 2 | Plan 3 / Workflow E: P5 渲染重写 | CRITICAL | 13 | 5-7 | 被 Gap 1 阻断 |
| 3 | Plan 4: P4b 类型隔离剩余违规 | HIGH | 0 | ~15 | 无依赖 |
| 4 | B3 MigrateInput 遗留项 | MEDIUM | 0 | 3-4 | 无依赖 |

---

## Gap 1 — Plan 2: main_loop cutover（CRITICAL）

### 当前状态

B3 Cutover 实现了**双路径架构**：`state_machine::handle`（纯函数）+ `keyboard::handle_key_event`（兜底），通过 `is_sm_handled_shortcut()` 过滤避免双重执行。

真正的单路径化（删除 keyboard 模块，所有事件走 state machine）尚未执行。CLAUDE.md 将此列为未完成项。

### 需删除的文件（6 个，~1,200 行）

```
peri-tui/src/event/keyboard.rs               (~264 行)
peri-tui/src/event/keyboard/bar_focus.rs      (~89 行)
peri-tui/src/event/keyboard/normal_keys.rs    (~300 行)
peri-tui/src/event/keyboard/popups.rs         (~138 行)
peri-tui/src/event/keyboard/setup_wizard.rs   (~82 行)
peri-tui/src/event/keyboard/shortcuts.rs      (~200 行)
```

### 需修改的文件

| 文件 | 改动 |
|------|------|
| `peri-tui/src/runtime/main_loop.rs` | 删除 keyboard 分发分支（step 1b）；所有事件走 state machine |
| `peri-tui/src/event/mod.rs` | 清理 paste/scroll/mouse 中向 keyboard 模块的路由 |

### 风险评估

keyboard 模块 Effect-ization 需要 **50+ 新 Effect 变体**（每击键、textarea 操作、popup 交互都需 Effect 通路）。这是整个 v2 迁移中风险最高的单点。当前双路径是故意的工程权衡。

### 阻断

此 gap 阻断 Gap 2（P5 渲染重写）。Plan 文档规定："Blocked by: Plan 2 (state_machine is the only state source)"。

---

## Gap 2 — Plan 3 / Workflow E: P5 渲染重写（CRITICAL）

### 当前状态

消息区渲染仍使用 legacy `MessagePipeline` + 双线程 `RenderThread` + `RenderCache` + `AdaptiveChunkingPolicy`。

### 已完成的部分

- `ViewStore` + `CurrentTurn` + `InputState` 骨架就位
- `CurrentTurn.view_models()` 真实化——从 streaming 数据构建 `AssistantBubble`（含 ReasoningBlock）+ `ToolCard`（commit `5200ab98`）
- Modal panel 渲染已接入 v2 `ModalState::Panel` overlay（commit `44c7965e`）
- `InputState ↔ TextArea` sync bridge 已实现（commit `24aa07f9`）
- P5 skeleton `render/mod.rs` 存在

### 需删除的文件（13 个，~143KB）

```
peri-tui/src/app/message_pipeline/mod.rs                 (423 行)
peri-tui/src/app/message_pipeline/lifecycle.rs            (6.4KB)
peri-tui/src/app/message_pipeline/reconcile.rs            (14.8KB)
peri-tui/src/app/message_pipeline/state.rs                (3.0KB)
peri-tui/src/app/message_pipeline/streaming.rs            (4.2KB)
peri-tui/src/app/message_pipeline/subagent.rs             (9.5KB)
peri-tui/src/app/message_pipeline/throttle.rs             (7.1KB)
peri-tui/src/app/message_pipeline/tools.rs                (5.6KB)
peri-tui/src/app/message_pipeline/transform.rs            (5.4KB)
peri-tui/src/app/message_pipeline/message_pipeline_test.rs (82KB, 2342 测试)
peri-tui/src/ui/render_thread.rs                          (574 行)
peri-tui/src/ui/render_thread_test.rs                     (565 行)
peri-tui/src/app/agent_render.rs                          (195 行)
```

### 需修改的文件（5-7 个）

| 文件 | 改动 |
|------|------|
| `peri-tui/src/render/mod.rs` | 实现 sync 渲染：从 `State.view + current_turn` 读取，不读 MessagePipeline |
| `peri-tui/src/ui/main_ui/message_area.rs` | 从 vms slice 渲染，不读 RenderCache |
| `peri-tui/src/app/message_state.rs` | 删除 pipeline + render_tx 字段 |
| `peri-tui/src/app/chat_session.rs` | 删除 `spawn_render_thread()` 调用 |
| `peri-tui/src/app/panel_ops.rs` | 清理渲染线程引用 |
| `peri-tui/src/app/agent_compact.rs` | 改用 ViewStore 操作替代 pipeline 操作 |
| `peri-tui/src/app/mod.rs` | 删除 `AdaptiveChunkingPolicy` / `ChunkingMode` / `DrainPlan` 的 `pub use` |

### 阻断

被 Gap 1 阻断。需 main_loop 单路径化后，state machine 成为唯一状态源，渲染才能切到 `State.view + current_turn`。

---

## Gap 3 — Plan 4: P4b 类型隔离剩余违规（HIGH）

### 当前状态

Workflow D 完成了大量类型转换工作（DTO 化、`dto_convert.rs` 桥梁层）。14 个 v2 面板已零 `peri_middlewares` 导入。剩余 ~20 处违规分布在 ~15 个非面板源文件中。

CLAUDE.md 将此归为"合理运行时依赖"或"HITL/AskUser 通道"等分类，但严格按 P4b 目标仍属违规。

### 剩余违规清单

| 文件 | 违规导入 |
|------|---------|
| `app/service_registry.rs` | `peri_agent::interaction::ChannelState` |
| `app/service_registry.rs` | `peri_middlewares::*` (re-export) |
| `app/agent_ops/rewind.rs` | `peri_agent::messages::{BaseMessage, ContentBlock}` |
| `app/events.rs` | `peri_agent::interaction::{InteractionContext, InteractionResponse}` |
| `app/agent_compact.rs` | `peri_agent::messages::BaseMessage` |
| `app/cron_state.rs` | `peri_middlewares::cron::CronScheduler` |
| `app/ask_user_prompt.rs` | `peri_middlewares::ask_user::*` |
| `app/mod.rs` | `BaseMessage`, `HitlDecision`, `McpClientPool`, `McpInitStatus`, `OAuthFlowEvent` |
| `app/agent_ops_interaction.rs` | 多处 `peri_agent` + `peri_middlewares` |
| `app/hitl_prompt.rs` | `peri_middlewares::prelude::*` |
| `app/agent_comm.rs` | `AgentCancellationToken`, `BaseMessage` |
| `app/oauth_prompt.rs` | `peri_middlewares::mcp::parse_code_from_url` |
| `ui/main_ui/popups/hitl.rs` | `peri_middlewares::hitl::BatchItem` |
| `ui/main_ui/popups/ask_user_height.rs` | `peri_middlewares::ask_user::AskUserQuestionData` |
| `panel/panels/thread_browser.rs` | `peri_agent::thread::ThreadMeta` |
| `panel/panels/cron.rs` | `peri_middlewares::cron::CronTask` |
| `command/core/gc.rs` | `peri_agent::messages::{BaseMessage, ContentBlock, MessageContent}` |
| `command/session/plugin_command.rs` | `peri_middlewares::plugin::{CommandEntry, CommandSource}` |

**共 ~20-22 处违规，分布在 ~15 个文件。**

### 说明

部分违规在 `message_pipeline/`、`state_machine/`、`ui/message_view/` 中，这些文件会被 Gap 2 自然清理。其余是真正的运行时依赖（HITL 通道 oneshot channel、MCP 客户端池、Cron 调度器等），迁移需要额外的 DTO 定义。

### 阻断

无依赖，可独立执行。

---

## Gap 4 — B3 MigrateInput 遗留项（MEDIUM）

### 当前状态

- `InputState` 结构完整（lines、cursor、selection、history、prediction、at_mention、slash_completion、attachments）
- `from_textarea()` / `to_textarea()` sync bridge 已实现并接入 main_loop
- 双向同步已工作：effects 后 `TextArea → InputState`，渲染前 `InputState → TextArea`

### 未完成部分

当前是"双向同步"模式，不是 plan 文档要求的"单一数据源"模式。

| 文件 | 问题 |
|------|------|
| `app/ui_state.rs` | 仍持有 `textarea: TextArea<'static>` 字段 |
| `app/field_textarea.rs` | 统一输入组件仍基于 TextArea |
| `event/keyboard/normal_keys.rs` | ~25 处直接操作 `ui.textarea` |
| `event/mod.rs` | paste/mouse 路径直接操作 `ui.textarea` |

### 说明

完全迁入 `State::Idle.input` 需要 keyboard 模块 Effect-ization（Gap 1）。在此之前，双向同步是合理的过渡方案——`State::Idle.input` 作为"次要副本"与 `UiState.textarea` 保持同步。

### 阻断

无依赖，但要达到 plan 文档要求的"单一数据源"需等待 Gap 1。

---

## 依赖关系图

```
Gap 3 (P4b 类型隔离) ←── 独立执行

Gap 4 (B3 MigrateInput) ←── 双向同步已可用
    └─ 完全迁移需等待 ──→ Gap 1

Gap 1 (main_loop cutover)
    ├─ keyboard Effect-ization: 50+ Effect 变体
    └─ 阻断 ──→ Gap 2 (P5 渲染重写)
                     ├─ 删除 message_pipeline/ (10 文件, 143KB)
                     ├─ 删除 render_thread.rs
                     ├─ 渲染入口切到 State.view + current_turn
                     └─ 单线程同步渲染
```

---

## 执行建议

按依赖顺序：

1. **Gap 3 优先**（独立，低风险）——逐个文件替换 ~20 处 import 违规。只需在 `peri-acp-types` 或 `dto_convert.rs` 中补充 DTO 定义。

2. **Gap 1 评估**——决定是否推进 keyboard Effect-ization。如果推进，需在 `Effect` 枚举中新增 50+ 变体（每个 textarea 操作、popup 交互都需要 Effect 通路）。这是高风险大工程。

3. **Gap 2**——在 Gap 1 完成后执行。删除 legacy pipeline/render_thread，重写渲染入口。

4. **Gap 4**——在 Gap 1 完成后自然解决（keyboard 模块被删除后，`UiState.textarea` 可以直接删除）。
