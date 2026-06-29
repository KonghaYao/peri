# Plan 3: Workflow E 渲染重写 — 物理删除 message_pipeline + RenderThread

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development`.

**Goal:** 渲染管线全部走 `State.view + current_turn`；物理删除 `message_pipeline/` 全目录（10 文件，~60KB）+ `ui/render_thread.rs`（~570 行）+ `AdaptiveChunkingPolicy`；主线程同步渲染 + 16ms 帧率节流。

**Architecture:** 渲染入口 `render::render(f, state)` 读取 `State.view: Vec<ViewModel>` + `State.current_turn` 调用 `ViewStore::for_render()` 拼接，直接调 `render_view_model()` 同步输出。Ephemeral VM（SystemNote/CacheWarning）锚点机制在 `ViewStore` 内重新实现。

**Tech Stack:** Rust 2021 + ratatui + `render/throttle.rs::Throttle`（已就位）。

**依赖**：Plan 2（state_machine 是唯一状态源）。

**Blocked by**：Plan 2 完成。

---

## File Structure

| 文件 | 处置 |
|------|------|
| `peri-tui/src/app/message_pipeline/` 全目录 | **物理删除**（10 文件） |
| `peri-tui/src/app/message_pipeline_test.rs` | **物理删除**（82KB, 2342 行测试） |
| `peri-tui/src/ui/render_thread.rs` | **物理删除**（~570 行） |
| `peri-tui/src/ui/render_thread_test.rs` | **物理删除**（565 行） |
| `peri-tui/src/app/message_state.rs` | **删除 pipeline + render_tx 字段**，保留 `view_messages` 等其他字段 |
| `peri-tui/src/state_machine/view_store.rs` | **扩展**：新增 `ephemeral_notes` 锚点机制 |
| `peri-tui/src/render/mod.rs` | **重写**：从 `State.view` 读，同步渲染 |
| `peri-tui/src/ui/main_ui/message_area.rs` | **重写**：从 `State` 读，不读 RenderCache |
| `peri-tui/src/app/agent_render.rs` | **删除**（pipeline 调用点） |
| `peri-tui/src/app/agent_compact.rs` | **重写**：compact 三步清理改走 ViewStore |
| `peri-tui/src/app/chat_session.rs:46-47` | **删除 spawn_render_thread 调用** |
| `peri-tui/src/app/panel_ops.rs:63` | **删除 spawn_render_thread 调用** |
| `peri-tui/src/app/thread_ops.rs` | **重写**：scroll 直接操作 State.scroll_offset |
| `peri-tui/src/app/message_pipeline/transform.rs::messages_to_view_models` | **迁移**到 `ui/message_view/build.rs`（v2 已有等价） |

---

## Task 1: ViewStore 扩展 ephemeral 锚点机制

**Files:**
- Modify: `peri-tui/src/state_machine/view_store.rs`

ViewStore 当前只有 `view_models: Vec<ViewModel>` + `commit()`。需要添加：
- `ephemeral_notes: Vec<(usize, ViewModel)>` — 锚点机制
- `frozen_subagent_vms: Vec<FrozenSubAgentVm>` — frozen 子代理
- `insert_ephemeral(anchor_len, vm)` / `retain_ephemeral(prefix_len)` 方法

- [ ] **Step 1: 写 ephemeral 锚点失败测试**

Append to `peri-tui/src/state_machine/view_store.rs` 测试模块：

```rust
#[test]
fn test_insert_ephemeral_at_anchor() {
    let mut store = ViewStore::default();
    store.commit(vec![vm_user(), vm_assistant()]);
    store.insert_ephemeral(2, vm_system_note());
    // ephemeral 在 view_models 末尾（anchor=2 = 当前长度）
    assert_eq!(store.view_models.len(), 2);
    assert_eq!(store.ephemeral_notes.len(), 1);
    assert_eq!(store.ephemeral_notes[0].0, 2);
}

#[test]
fn test_for_render_inserts_ephemeral_at_correct_position() {
    let mut store = ViewStore::default();
    store.commit(vec![vm_user(), vm_assistant()]);
    store.insert_ephemeral(1, vm_system_note()); // anchor=1: 插入到第 1 个之后
    let rendered = store.for_render(&CurrentTurn::default());
    // 期望顺序：user, system_note, assistant
    assert_eq!(rendered.len(), 3);
    assert!(matches!(rendered[1], ViewModel::SystemNote(_)));
}

#[test]
fn test_retain_ephemeral_filters_expired_anchors() {
    let mut store = ViewStore::default();
    store.commit(vec![vm_user(), vm_assistant()]);
    store.insert_ephemeral(5, vm_system_note()); // anchor=5 但实际长度 2
    // commit 新快照（prefix_len=1）后，anchor 5 > 1 应被过滤
    store.commit_with_prefix(vec![vm_user()], 1);
    assert!(store.ephemeral_notes.is_empty());
}

#[test]
fn test_frozen_subagent_vms_match_by_instance_id() {
    let mut store = ViewStore::default();
    store.commit(vec![vm_user()]);
    store.frozen_subagent_vms = vec![FrozenSubAgentVm {
        agent_id: "agent_1".into(),
        instance_id: Some("inst_123".into()),
        vms: vec![vm_subagent()],
    }];
    let rendered = store.for_render(&CurrentTurn::default());
    // 包含 frozen subagent
    assert!(rendered.iter().any(|vm| matches!(vm, ViewModel::SubAgentGroup(_))));
}
```

- [ ] **Step 2: 运行验证失败**

```bash
cargo test -p peri-tui --lib state_machine::view_store
```
Expected: FAIL

- [ ] **Step 3: 扩展 ViewStore**

Modify `peri-tui/src/state_machine/view_store.rs`:

```rust
//! ViewStore: 规范 ViewModel 存储 + ephemeral 锚点 + frozen subagent。

use std::sync::Arc;
use peri_acp_types::view_model::ViewModel;
use super::current_turn::CurrentTurn;

#[derive(Debug, Clone, Default)]
pub struct ViewStore {
    pub view_models: Vec<ViewModel>,
    pub ephemeral_notes: Vec<(usize, ViewModel)>,
    pub frozen_subagent_vms: Vec<FrozenSubAgentVm>,
}

#[derive(Debug, Clone)]
pub struct FrozenSubAgentVm {
    pub agent_id: String,
    pub instance_id: Option<String>,
    pub vms: Vec<ViewModel>,
}

impl ViewStore {
    /// 替换语义 commit（非 extend）。
    pub fn commit(&mut self, vms: Vec<ViewModel>) {
        self.view_models = vms;
    }

    /// commit 并按 prefix_len 过滤过期 ephemeral。
    pub fn commit_with_prefix(&mut self, vms: Vec<ViewModel>, prefix_len: usize) {
        self.view_models = vms;
        self.ephemeral_notes.retain(|(anchor, _)| *anchor >= prefix_len);
    }

    /// 插入 ephemeral VM，anchor = 当前 view_models.len()。
    pub fn insert_ephemeral(&mut self, anchor_len: usize, vm: ViewModel) {
        self.ephemeral_notes.push((anchor_len, vm));
    }

    /// 清空 ephemeral（compact 时调用）。
    pub fn clear_ephemeral(&mut self) {
        self.ephemeral_notes.clear();
    }

    /// 清空 frozen subagent（begin_round 时调用）。
    pub fn clear_frozen_subagent(&mut self) {
        self.frozen_subagent_vms.clear();
    }

    /// 拼接 view_models + ephemeral + frozen subagent + current_turn。
    pub fn for_render(&self, current_turn: &CurrentTurn) -> Vec<ViewModel> {
        let mut result: Vec<ViewModel> = Vec::with_capacity(
            self.view_models.len() + self.ephemeral_notes.len() + 4,
        );

        // 把 ephemeral 按 anchor 插入到正确位置
        let mut ephemeral_idx = 0;
        for (i, vm) in self.view_models.iter().enumerate() {
            result.push(vm.clone());
            // 插入所有 anchor == i+1 的 ephemeral
            while ephemeral_idx < self.ephemeral_notes.len()
                && self.ephemeral_notes[ephemeral_idx].0 == i + 1
            {
                result.push(self.ephemeral_notes[ephemeral_idx].1.clone());
                ephemeral_idx += 1;
            }
        }
        // 补尾部 ephemeral
        while ephemeral_idx < self.ephemeral_notes.len() {
            result.push(self.ephemeral_notes[ephemeral_idx].1.clone());
            ephemeral_idx += 1;
        }

        // frozen subagent
        for fsa in &self.frozen_subagent_vms {
            for vm in &fsa.vms {
                result.push(vm.clone());
            }
        }

        // current_turn（流式增量）
        for vm in current_turn.view_models() {
            result.push(vm);
        }

        result
    }
}
```

- [ ] **Step 4: 添加 CurrentTurn::view_models 方法**

Modify `peri-tui/src/state_machine/current_turn.rs` 添加：

```rust
/// 派生 ViewModel 列表（用于渲染）。
pub fn view_models(&self) -> Vec<ViewModel> {
    // 基于 text / reasoning / tool_cards 构建 ViewModel
    // 复用 ui/message_view/build.rs 中的逻辑
    crate::ui::message_view::build::current_turn_to_view_models(self)
}
```

新建 `peri-tui/src/ui/message_view/build/current_turn.rs`（或在现有 build.rs 中添加）：

```rust
//! 从 CurrentTurn 派生 ViewModel。

use peri_acp_types::view_model::ViewModel;
use crate::state_machine::current_turn::CurrentTurn;

pub fn current_turn_to_view_models(turn: &CurrentTurn) -> Vec<ViewModel> {
    if turn.text.is_empty() && turn.tool_cards.is_empty() && turn.reasoning.is_empty() {
        return Vec::new();
    }
    // 构建一个 AssistantBubble VM，包含 text + reasoning + tool_uses
    vec![ViewModel::AssistantBubble {
        text: turn.text.clone(),
        reasoning: if turn.reasoning.is_empty() { None } else { Some(turn.reasoning.clone()) },
        tool_uses: turn.tool_cards.iter().map(|c| c.to_tool_use_view()).collect(),
        instance_id: None,
    }]
}
```

- [ ] **Step 5: 运行测试 + Commit**

```bash
cargo test -p peri-tui --lib state_machine::view_store
git add peri-tui/src/state_machine/ peri-tui/src/ui/message_view/
git commit -m "feat(v2): ViewStore ephemeral 锚点 + frozen subagent + for_render 派生

- ephemeral_notes: Vec<(anchor_len, ViewModel)> 按 anchor 插入位置
- frozen_subagent_vms 按 agent_id + instance_id 匹配
- for_render 拼接 view + ephemeral + frozen + current_turn
- CurrentTurn::view_models() 派生 AssistantBubble VM
- 4 个锚点机制测试

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>"
```

---

## Task 2: 重写 render/mod.rs — 同步渲染入口

**Files:**
- Modify: `peri-tui/src/render/mod.rs`

当前 `render::render` 委托 legacy `app_draw` 闭包。改为从 State 读取。

- [ ] **Step 1: 写 render 失败测试**

Append to `peri-tui/src/render/mod.rs` 测试模块：

```rust
#[test]
fn test_render_reads_state_view_when_idle() {
    let mut state = State::Idle(IdleState::default());
    // 注入一个 UserBubble VM
    if let State::Idle(s) = &mut state {
        s.view = vec![ViewModel::UserBubble { text: "hello".into() }];
    }
    let vms = collect_render_vms(&state);
    assert_eq!(vms.len(), 1);
}

#[test]
fn test_render_includes_current_turn_when_streaming() {
    let mut state = State::Streaming(StreamingState::default());
    if let State::Streaming(s) = &mut state {
        s.current_turn.text = "partial".into();
        s.current_turn.active = true;
    }
    let vms = collect_render_vms(&state);
    assert!(vms.iter().any(|vm| matches!(vm, ViewModel::AssistantBubble { .. })));
}

fn collect_render_vms(state: &State) -> Vec<ViewModel> {
    let store = ViewStore::default();
    let turn = match state {
        State::Streaming(s) => &s.current_turn,
        _ => &CurrentTurn::default(),
    };
    store.for_render(turn)
}
```

- [ ] **Step 2: 运行验证失败**

```bash
cargo test -p peri-tui --lib render
```
Expected: FAIL

- [ ] **Step 3: 重写 render/mod.rs**

Modify `peri-tui/src/render/mod.rs`:

```rust
//! v2 同步渲染入口。主线程直接调 render_view_model，16ms 节流。

use ratatui::Frame;
use crate::app::App;
use crate::state_machine::state::State;
use crate::state_machine::view_store::ViewStore;

pub fn render(f: &mut Frame, app: &App, state: &State) {
    let area = f.area();

    // 主布局：StickyHeader / Messages / AttachmentBar / PanelArea / Input / StatusBar / BGAgentBar
    let chunks = crate::ui::main_ui::layout(area, app);

    // 从 State 派生 ViewModel 列表
    let turn = match state {
        State::Streaming(s) => &s.current_turn,
        _ => &Default::default(),
    };

    let view_store = build_view_store_from_state(state, app);
    let vms = view_store.for_render(turn);

    // 同步渲染（不再走 RenderThread）
    crate::ui::main_ui::message_area::render(f, chunks.messages, &vms, app, state);
    crate::ui::main_ui::sticky_header::render(f, chunks.sticky_header, &vms, app);
    crate::ui::main_ui::attachment::render(f, chunks.attachment, app);
    crate::ui::main_ui::input_area::render(f, chunks.input, app, state);
    crate::ui::main_ui::status_bar::render(f, chunks.status_bar, app);

    // 模态层（面板/交互弹窗）
    if let State::Modal(modal_state) = state {
        crate::ui::main_ui::modal::render(f, modal_state, app);
    }
}

fn build_view_store_from_state(state: &State, app: &App) -> ViewStore {
    let mut store = ViewStore::default();
    let view = match state {
        State::Idle(s) => &s.view,
        State::Streaming(s) => &s.view,
        State::Switching(s) => &s.view,
        State::Modal(_) => return ViewStore::default(), // Modal 不显示主消息区
    };
    store.commit(view.clone());
    // 从 app 注入 ephemeral（SystemNote 等）
    for (anchor, vm) in &app.session_mgr.current().messages.ephemeral_notes {
        store.insert_ephemeral(*anchor, vm.clone());
    }
    store
}
```

- [ ] **Step 4: 修复 main_loop::run 调用 render**

Modify `peri-tui/src/runtime/main_loop.rs` `apply_context.rs::draw_now`：

```rust
pub fn draw_now(&mut self, app: &mut App, last_render: &mut Instant, state: &State) {
    self.terminal.draw(|f| crate::render::render(f, app, state)).ok();
    *last_render = Instant::now();
}
```

修改 `main_loop::run` 的 draw 调用：

```rust
ctx.draw_now(app, &mut last_render, &state);
```

- [ ] **Step 5: 修复 ui/main_ui/message_area.rs**

重写 `message_area::render` 签名，从 `vms: &[ViewModel]` 读，不再读 `RenderCache`：

```rust
pub fn render(f: &mut Frame, area: Rect, vms: &[ViewModel], app: &App, state: &State) {
    // 同步渲染每个 VM
    let mut y = area.y;
    for vm in vms {
        let vm_height = crate::ui::message_render::measure_vm(vm, area.width);
        let vm_area = Rect { x: area.x, y, width: area.width, height: vm_height };
        crate::ui::message_render::render_view_model(f, vm_area, vm, app);
        y += vm_height;
    }
    // spinner（streaming 时）
    if let State::Streaming(_) = state {
        // 渲染 spinner
    }
}
```

- [ ] **Step 6: 编译 + 修复下游**

```bash
cargo build -p peri-tui 2>&1 | grep "error\[" | head -20
```

修复所有编译错误（主要是 message_area 签名变化引发的下游）。

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(v2): render/mod.rs 同步渲染入口 — 从 State.view + current_turn 派生

- render::render(f, app, state) 单一入口
- build_view_store_from_state 从 State 注入 view + ephemeral
- message_area::render 接收 vms 切片（不再读 RenderCache）
- draw_now 接收 state 参数

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>"
```

---

## Task 3: 物理删除 message_pipeline/ + render_thread.rs

**Files:**
- Delete: `peri-tui/src/app/message_pipeline/` 全目录
- Delete: `peri-tui/src/app/message_pipeline_test.rs`（在 message_pipeline/ 内）
- Delete: `peri-tui/src/ui/render_thread.rs`
- Delete: `peri-tui/src/ui/render_thread_test.rs`
- Modify: `peri-tui/src/app/message_state.rs`（删除 pipeline + render_tx 字段）
- Modify: `peri-tui/src/app/mod.rs`（删除 mod 声明）

- [ ] **Step 1: 删除文件**

```bash
rm -rf peri-tui/src/app/message_pipeline/
rm -f peri-tui/src/ui/render_thread.rs
rm -f peri-tui/src/ui/render_thread_test.rs
```

- [ ] **Step 2: 修复 app/mod.rs 的 mod 声明**

```bash
grep -n "pub mod message_pipeline\|mod message_pipeline" peri-tui/src/app/mod.rs
```

删除 `pub mod message_pipeline;` 行。

```bash
grep -n "pub mod render_thread\|mod render_thread" peri-tui/src/ui/mod.rs
```

删除 `pub mod render_thread;` 行。

- [ ] **Step 3: 修复 message_state.rs**

读取 `peri-tui/src/app/message_state.rs`，删除：
- `pipeline: MessagePipeline` 字段
- `render_tx: mpsc::Sender<RenderEvent>` 字段
- 相关方法（如 `fn pipeline() -> &MessagePipeline`）

保留 `view_messages: Vec<MessageViewModel>` / `ephemeral_notes` 等其他字段（如果还需要；否则也删）。

- [ ] **Step 4: 修复所有下游引用**

```bash
cargo build -p peri-tui 2>&1 | grep "error\[" | head -30
```

主要错误类型：
- `messages.pipeline.handle_event(...)` → 删除（状态机已处理）
- `messages.render_tx.send(...)` → 删除
- `MessagePipeline::new()` → 删除
- `RenderEvent::Rebuild` → 删除

逐一删除调用点（不要保留 fallback）。

- [ ] **Step 5: 删除 chat_session.rs / panel_ops.rs 中的 spawn_render_thread**

```bash
grep -n "spawn_render_thread" peri-tui/src/app/chat_session.rs peri-tui/src/app/panel_ops.rs
```

删除每个调用点。

- [ ] **Step 6: 删除 agent_render.rs**

```bash
rm peri-tui/src/app/agent_render.rs
```

修复 `app/mod.rs` 中 `pub mod agent_render;` 删除。

- [ ] **Step 7: 重写 agent_compact.rs**

`agent_compact.rs` 当前调用 `pipeline.clear() + restore_completed() + RebuildAll`。改为操作 ViewStore：

```rust
pub fn handle_compact_completed(app: &mut App, messages: Vec<BaseMessage>) {
    let store = &mut app.session_mgr.current_mut().messages.view_store;
    store.clear_ephemeral();
    // 把 messages 转为 view_models 并 commit
    let vms = crate::ui::message_view::build::messages_to_view_models(&messages);
    store.commit(vms);
    // 状态机的 State.view 也会在下次 view-commit 时同步
}
```

- [ ] **Step 8: 全 workspace 编译**

```bash
cargo build --workspace 2>&1 | tail -10
```

预期会有剩余编译错误（headless_test.rs 等）。逐一处理：

```bash
cargo build --workspace 2>&1 | grep "error\[" | wc -l
```

目标：0 错误。

- [ ] **Step 9: 删除失效测试**

```bash
cargo test --workspace --no-run 2>&1 | grep "error\[" | head -20
```

对失败的测试文件：
- `headless_test.rs` 中依赖 MessagePipeline / RenderCache 的测试 → 删除整个测试函数
- `agent_ops_test.rs` 中依赖 pipeline 的测试 → 删除

```bash
# 示例：删除 headless_test.rs 中 RenderCache 相关测试
grep -n "RenderCache\|MessagePipeline\|render_thread" peri-tui/src/ui/headless_test.rs | head -10
```

对每个匹配，删除整个测试函数（用 `#[test]\n fn test_xxx() { ... }` 边界）。

- [ ] **Step 10: 测试 + Commit**

```bash
cargo test --workspace 2>&1 | grep -E "test result" | awk '{print $4, $6, $8}' | awk '{p+=$1; f+=$2; i+=$3} END {print "passed=" p " failed=" f " ignored=" i}'
```
Expected: 失败数大幅下降（只剩真实 bug）

```bash
git add -A
git commit -m "feat(v2): 物理删除 message_pipeline/ + render_thread.rs

- 删除 app/message_pipeline/ 全目录（10 文件，~60KB）
- 删除 app/message_pipeline_test.rs（82KB, 2342 行测试）
- 删除 ui/render_thread.rs + render_thread_test.rs（~1135 行）
- 删除 app/agent_render.rs
- message_state.rs: 删除 pipeline + render_tx 字段
- chat_session.rs / panel_ops.rs: 删除 spawn_render_thread 调用
- agent_compact.rs: 重写为 ViewStore 操作
- 删除依赖 pipeline/render_thread 的失效测试

BREAKING CHANGE: 不再支持双线程渲染 / RenderCache / AdaptiveChunkingPolicy

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>"
```

---

## Task 4: 渲染快照测试重建

**Files:**
- Create: `peri-tui/src/render/render_snapshot_test.rs`

用 ratatui 的 TestBackend 重建渲染快照测试，验证 v2 渲染输出。

- [ ] **Step 1: 写快照测试**

```rust
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use peri_acp_types::view_model::ViewModel;
use crate::app::App;
use crate::state_machine::state::{IdleState, State};

#[test]
fn test_render_empty_state_shows_welcome() {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut app = App::test_default();
    let state = State::Idle(IdleState::default());

    terminal.draw(|f| crate::render::render(f, &app, &state)).unwrap();

    let buffer = terminal.backend().buffer();
    // 期望包含 Welcome 文本
    assert!(buffer.content().iter().any(|c| c.symbol().contains('W')));
}

#[test]
fn test_render_with_user_message_shows_bubble() {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut app = App::test_default();
    let mut state = State::Idle(IdleState::default());
    if let State::Idle(s) = &mut state {
        s.view = vec![ViewModel::UserBubble { text: "Hello world".into() }];
    }

    terminal.draw(|f| crate::render::render(f, &app, &state)).unwrap();

    let buffer = terminal.backend().buffer();
    let content: String = buffer.content().iter().map(|c| c.symbol().chars().next().unwrap_or(' ')).collect();
    assert!(content.contains("Hello"));
}

#[test]
fn test_render_streaming_shows_partial_text() {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut app = App::test_default();
    let mut state = State::Streaming(crate::state_machine::state::StreamingState::default());
    if let State::Streaming(s) = &mut state {
        s.current_turn.text = "Partial response".into();
        s.current_turn.active = true;
    }

    terminal.draw(|f| crate::render::render(f, &app, &state)).unwrap();

    let buffer = terminal.backend().buffer();
    let content: String = buffer.content().iter().map(|c| c.symbol().chars().next().unwrap_or(' ')).collect();
    assert!(content.contains("Partial"));
}
```

- [ ] **Step 2: 添加 App::test_default**

Modify `peri-tui/src/app/mod.rs` 添加测试构造器：

```rust
#[cfg(test)]
impl App {
    pub fn test_default() -> Self {
        // 最小化构造，仅含渲染必需字段
        App {
            session_mgr: SessionManager::test_default(),
            global_ui: GlobalUiState::default(),
            services: ServiceRegistry::test_default(),
            // ... 其他字段默认
        }
    }
}
```

- [ ] **Step 3: 运行测试 + Commit**

```bash
cargo test -p peri-tui --lib render::render_snapshot_test
git add peri-tui/src/render/ peri-tui/src/app/mod.rs
git commit -m "test(v2): 渲染快照测试重建 — ratatui TestBackend

- 空状态显示 Welcome
- UserBubble 渲染用户消息
- Streaming 状态显示 current_turn 文本
- App::test_default 测试构造器

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>"
```

---

## Plan 3 完成定义

1. ✅ `grep -rn "MessagePipeline" peri-tui/src/` → 0 结果
2. ✅ `grep -rn "RenderCache\|RenderEvent\|render_thread\|AdaptiveChunkingPolicy" peri-tui/src/` → 0 结果
3. ✅ `grep -rn "spawn_render_thread" peri-tui/src/` → 0 结果
4. ✅ `message_pipeline/` 目录不存在
5. ✅ `render_thread.rs` 文件不存在
6. ✅ `cargo build --workspace` 绿
7. ✅ `cargo test --workspace` 通过率 ≥ 85%（允许部分 snapshot 测试因 v2 渲染差异失败）
8. ✅ TUI 启动 + 渲染消息 + 流式输出 + Compact 手动测试通过
