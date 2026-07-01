# ratatui-kit 迁移: Phase 4 事件系统统一

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 用 ratatui-kit `use_event_handler` + `InputLayer` 替代双路径事件分发，state_machine 保留为 ACP 桥接层

**Architecture:** 四层事件模型 (Global → Root → Input → Modal)；用户输入全走 `use_event_handler`，ACP 事件走独立 `acp_bridge` task + `Atom<AcpStateSnapshot>.write()` 触发重渲染；state_machine 降为纯数据管道（ACP event → State → Atom）；Effect 枚举逐步退役，首轮压缩到 ~12 变体

**Tech Stack:** ratatui-kit 0.6 `use_event_handler` + `EventScope` + `EventPriority` + `use_input_layer`, Rust 2024, tokio

---

## 前置分析

### 当前双路径架构（main_loop.rs:67-164）

```
TuiEvent → 
  1a. dispatch_sm(state, event)     → state_machine::handle → (State, Vec<Effect>)
  1b. dispatch_fallback(event, ...) → keyboard::handle_key_event → Effects
  1c. merge_effects + execute_effects + 渲染
```

### 快捷键归属（idle.rs `owns_shortcut`）

| 快捷键 | 归属 | Effect |
|--------|------|--------|
| Ctrl+T | SM | `CycleModel` |
| Ctrl+Shift+T | SM | `CycleProvider` |
| Ctrl+P | SM | `OpenPanel(Model)` |
| BackTab | Fallback 执行 | `CyclePermissionMode` |
| Enter (plain) | SM | `SubmitMessage` |
| Ctrl+B | Fallback 执行 | `FocusBgBar` |
| Ctrl+O | Fallback 执行 | `ToggleDiff` |

### Fallback 独占快捷键（normal_keys.rs ~400 行）

| 快捷键 | 行为 |
|--------|------|
| Ctrl+C | 3 级：清空输入 → 中断 agent → 双击退出 |
| Esc | 关闭 popup / @mention / slash_hint / 双击 rewind |
| Up/Down | @mention 导航 → hint 导航 → history 浏览 → textarea 光标 |
| Tab | prediction 接受 → @mention 注入 → hint 循环 |
| Enter (@mention/slash) | 路径注入 / hint 完成 / slash command 派发 |
| Shift+Enter | 插入换行 |
| Ctrl+U | 删除到行首 / 向上翻页 |
| Ctrl+D | 向下翻页 |
| Ctrl+W | 删前一个单词 |
| Ctrl+A | 全选 |
| Ctrl+N | 新建会话 |
| Ctrl+V | 粘贴图片/文本 |
| Left/Right/Home/End | textarea 光标移动 |
| Backspace/Delete | textarea 删除 |
| 普通字符 | textarea 插入 + @mention 检测 |

---

## Phase 4 实现

### Task 4a: 创建 Atom 定义 + acp_bridge

**Files:**
- Create: `peri-tui/src/kit/atoms.rs`
- Create: `peri-tui/src/kit/acp_bridge.rs`
- Modify: `peri-tui/src/kit/mod.rs`

- [ ] **Step 1: 创建 `kit/atoms.rs`**

```rust
//! 全局 Atom 定义 — 替代部分 Effect 变体。
//!
//! ratatui-kit Atom<T> 是 Copy 句柄的全局状态容器。声明为 static，
//! 在组件中通过 use_atom(&ATOM) 订阅。写入自动唤醒订阅组件。

use ratatui_kit::prelude::Atom;
use peri_acp_types::view_model::ViewModel;
use std::time::Instant;

/// ACP 状态快照（轻量投影，不含大对象）
#[derive(Debug, Clone)]
pub struct AcpStateSnapshot {
    pub variant: u8,           // 0=Idle, 1=Streaming, 2=Modal, 3=Switching
    pub view_count: usize,
    pub is_loading: bool,
    pub popup_active: bool,
    pub wizard_active: bool,
    pub at_mention_active: bool,
    pub slash_hint_active: bool,
}

impl Default for AcpStateSnapshot {
    fn default() -> Self {
        Self {
            variant: 0,
            view_count: 0,
            is_loading: false,
            popup_active: false,
            wizard_active: false,
            at_mention_active: false,
            slash_hint_active: false,
        }
    }
}

/// Session ViewModels 快照
#[derive(Debug, Clone, Default)]
pub struct ViewModelsSnapshot {
    pub committed: Vec<ViewModel>,
    pub current_turn: Vec<ViewModel>,
}

// ── 全局 Atom 声明 ──

pub static ACP_STATE: Atom<AcpStateSnapshot> = Atom::new(AcpStateSnapshot::default);
pub static VIEW_MODELS: Atom<ViewModelsSnapshot> = Atom::new(ViewModelsSnapshot::default);
pub static SCROLL_OFFSET: Atom<u16> = Atom::new(|| 0);

/// 状态栏瞬时高亮计时器
pub static MODEL_HIGHLIGHT_UNTIL: Atom<Option<Instant>> = Atom::new(|| None);
pub static PROVIDER_HIGHLIGHT_UNTIL: Atom<Option<Instant>> = Atom::new(|| None);
pub static MODE_HIGHLIGHT_UNTIL: Atom<Option<Instant>> = Atom::new(|| None);

/// @mention / slash_hint / popup 激活状态
pub static AT_MENTION_ACTIVE: Atom<bool> = Atom::new(|| false);
pub static SLASH_HINT_ACTIVE: Atom<bool> = Atom::new(|| false);
pub static POPUP_ACTIVE: Atom<bool> = Atom::new(|| false);
```

- [ ] **Step 2: 创建 `kit/acp_bridge.rs`**

```rust
//! ACP 事件 → Atom 桥接后台 task。
//!
//! 从 EventRx 接收 ACP 事件，经 state_machine 转换后写入全局 Atom。

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::state_machine::{self, state::{IdleState, State}, Event as SmEvent, AcpEventData};
use crate::runtime::event_channel::{EventRx, TuiEvent};
use crate::app::App;

use super::atoms::{AcpStateSnapshot, ACP_STATE, VIEW_MODELS};
use super::mod;

fn snapshot_state(state: &State, app: &App) -> AcpStateSnapshot {
    AcpStateSnapshot {
        variant: match state {
            State::Idle(_) => 0, State::Streaming(_) => 1,
            State::Modal(_) => 2, State::Switching(_) => 3,
        },
        view_count: state.view_models().len(),
        is_loading: matches!(state, State::Streaming(s) if s.current_turn.active),
        popup_active: app.is_interaction_popup_active(),
        wizard_active: app.global_ui.setup_wizard.is_some(),
        at_mention_active: app.session_mgr.current().ui.at_mention.active,
        slash_hint_active: app.session_mgr.current().ui.slash_hint.active,
    }
}

pub fn spawn_acp_bridge(
    mut rx: EventRx,
    mut app: App,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut state: State = State::Idle(IdleState::default());

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                event = rx.recv() => {
                    let event = match event {
                        Some(e) => e,
                        None => break,
                    };

                    match event {
                        TuiEvent::AcpEvent { event: event_name, data } => {
                            let sm_event = build_sm_event(&event_name, &data);
                            let (new_state, _effects) = state_machine::handle(
                                std::mem::replace(&mut state, State::Idle(IdleState::default())),
                                sm_event,
                            );
                            state = new_state;

                            // 写入 Atom → 触发组件重渲染
                            ACP_STATE.set(Some(snapshot_state(&state, &app)));
                            VIEW_MODELS.set(ViewModelsSnapshot {
                                committed: state.view_models().to_vec(),
                                current_turn: vec![],  // streaming 增量由外部更新
                            });
                        }

                        TuiEvent::AcpDisconnected | TuiEvent::Shutdown => break,

                        // Key/Mouse 不再通过此通道
                        _ => {}
                    }
                }
            }
        }
    })
}

fn build_sm_event(event_name: &str, data: &serde_json::Value) -> SmEvent {
    use peri_acp::event::AcpEvent;
    match serde_json::from_value::<AcpEvent>(data.clone()) {
        Ok(ev) => match ev {
            AcpEvent::ViewCommit(vc) => SmEvent::AcpEvent(AcpEventData::ViewCommit(vc)),
            AcpEvent::TextChunk(tc) => SmEvent::AcpEvent(AcpEventData::TextChunk(tc)),
            AcpEvent::ReasoningChunk(rc) => SmEvent::AcpEvent(AcpEventData::ReasoningChunk(rc)),
            AcpEvent::ToolStarted(ts) => SmEvent::AcpEvent(AcpEventData::ToolStarted(ts)),
            AcpEvent::ToolEnded(te) => SmEvent::AcpEvent(AcpEventData::ToolEnded(te)),
            AcpEvent::TurnDone => SmEvent::AcpEvent(AcpEventData::TurnDone),
            AcpEvent::TurnInterrupted(ti) => SmEvent::AcpEvent(AcpEventData::TurnInterrupted(ti)),
            other => SmEvent::AcpEvent(AcpEventData::Unknown {
                raw: format!("unhandled: {:?}", other),
            }),
        },
        Err(e) => SmEvent::AcpEvent(AcpEventData::Unknown {
            raw: format!("deser-failed: {}", e),
        }),
    }
}
```

- [ ] **Step 3: 修改 `kit/mod.rs`**

```rust
pub mod atoms;
pub mod acp_bridge;
```

- [ ] **验证**: `cargo check -p peri-tui`

---

### Task 4b: 创建事件处理器 — Global + Root 层

**Files:**
- Create: `peri-tui/src/kit/event_handlers.rs`
- Modify: `peri-tui/src/kit/mod.rs`

**核心**：从 `normal_keys.rs` (~400 行) 提取完整的快捷键 handler，映射到 ratatui-kit 的 `EventScope` 和 `EventPriority`。

- [ ] **Step 1: 创建 `kit/event_handlers.rs`**

```rust
//! 事件处理器 — Global + Root 层 use_event_handler 注册。
//!
//! 替代 `event/keyboard/normal_keys.rs` 的键盘 fallback。
//! 分为 Global Layer（不可阻断）和 Root Layer（被子层阻断）。

use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers},
    prelude::*,
};

use super::atoms::{
    ACP_STATE, SCROLL_OFFSET, MODEL_HIGHLIGHT_UNTIL, PROVIDER_HIGHLIGHT_UNTIL,
    MODE_HIGHLIGHT_UNTIL, AT_MENTION_ACTIVE, SLASH_HINT_ACTIVE,
};
use crate::app::App;
use crate::state_machine::State;
use std::time::Instant;

/// Global Layer: 不可阻断的快捷键（Ctrl+C quit, Resize, Ctrl+O）
pub fn register_global_handlers(
    hooks: &mut Hooks,
    exit: Handler<'static, ()>,
) {
    hooks.use_event_handler(
        EventScope::Global,
        EventPriority::Normal,
        move |event| {
            let Event::Key(key) = event else { return EventResult::Ignored };
            if key.kind != KeyEventKind::Press { return EventResult::Ignored }

            match key.code {
                // Ctrl+C: quit (3 级优先由 app 层 quit_pending_since 管理)
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    exit(());
                    EventResult::Consumed
                }
                // Ctrl+O: toggle diff（写 ACP_STATE diff_visible 字段 → Phase 5 在 App 层实现）
                KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    EventResult::Consumed
                }
                _ => EventResult::Ignored,
            }
        },
    );
}

/// Root Layer: 可被子层（Input/Modal）阻断的快捷键。
/// 
/// 从 normal_keys.rs 迁移：
///   - Ctrl+T/M/P/K 轮换
///   - Esc rewind/popup/panel
///   - Up/Down history scroll
///   - Enter 提交
///   - Backspace/Delete textarea 编辑
///   - 普通字符 textarea 插入
pub fn register_root_handlers(
    hooks: &mut Hooks,
    app: &mut App,
    state: &mut State,
) {
    hooks.use_event_handler(
        EventScope::Current,
        EventPriority::Normal,
        move |event| {
            let Event::Key(key) = event else { return EventResult::Ignored };
            if key.kind != KeyEventKind::Press { return EventResult::Ignored }

            match key.code {
                // ── 模型/提供者/权限 轮换 ──
                KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                        // Ctrl+Shift+T: cycle provider
                        PROVIDER_HIGHLIGHT_UNTIL.set(Some(Instant::now() + std::time::Duration::from_secs(2)));
                    } else {
                        // Ctrl+T: cycle model
                        MODEL_HIGHLIGHT_UNTIL.set(Some(Instant::now() + std::time::Duration::from_secs(2)));
                    }
                    EventResult::Consumed
                }
                KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    // Ctrl+P: open model panel
                    POPUP_ACTIVE.set(true);
                    EventResult::Consumed
                }
                KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    // Ctrl+K: cycle permission mode
                    MODE_HIGHLIGHT_UNTIL.set(Some(Instant::now() + std::time::Duration::from_secs(2)));
                    EventResult::Consumed
                }

                // ── Esc: 关 popup / rewind ──
                KeyCode::Esc => {
                    if AT_MENTION_ACTIVE.read().unwrap_or(&false) {
                        AT_MENTION_ACTIVE.set(false);
                        EventResult::Consumed
                    } else if SLASH_HINT_ACTIVE.read().unwrap_or(&false) {
                        SLASH_HINT_ACTIVE.set(false);
                        EventResult::Consumed
                    } else {
                        // rewind 双击检测 (Phase 5 由 App 层管理)
                        EventResult::Ignored
                    }
                }

                // ── Enter: 提交消息 ──
                KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    // Phase 5: 读取 InputState text，调用 AcpTuiClient.send()
                    // 当前阶段返回 Ignored，由 keyboard fallback 兜底
                    EventResult::Ignored
                }

                // ── Shift/Alt+Enter: 换行 ──
                KeyCode::Enter => {
                    EventResult::Ignored  // Phase 5: textarea insert_newline
                }

                // ── 方向键: history 浏览 → textarea 光标 ──
                KeyCode::Up | KeyCode::Down => {
                    // Phase 5: SM 处理 history scroll + textarea 光标
                    EventResult::Ignored
                }

                // ── 其他按键: 交由 keyboard fallback 兜底 (Phase 6 全接管) ──
                _ => EventResult::Ignored,
            }
        },
    );
}
```

- [ ] **验证**: `cargo check -p peri-tui`

---

### Task 4c: Input Layer — @mention / slash completion

**Files:**
- Create: `peri-tui/src/kit/mention_popup.rs`
- Create: `peri-tui/src/kit/slash_completion.rs`

使用 `use_input_layer(open=true, blocks_lower=true)` + `SearchInput` 替代 `update_at_mention_detection`。

- [ ] **Step 1: 创建 `kit/mention_popup.rs`**

```rust
//! @mention 文件提醒弹出组件。
//!
//! 当用户在输入框中输入 "@" 时激活，按路径前缀过滤文件列表。
//! 在 InputLayer 中运行 (blocks_lower=true)。

use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind},
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::{Style, Stylize},
        text::Line,
    },
};

use crate::ui::theme;

#[component]
pub fn MentionPopup(
    mut hooks: Hooks,
    prefix: String,
    items: Vec<String>,
    on_select: Handler<'static, String>,
    on_cancel: Handler<'static, ()>,
) -> impl Into<AnyElement<'static>> {
    hooks.use_input_layer(true, true);  // blocks_lower when popup open

    hooks.use_event_handler(EventScope::Current, EventPriority::High, move |event| {
        let Event::Key(key) = event else { return EventResult::Ignored };
        if key.kind != KeyEventKind::Press { return EventResult::Ignored }
        match key.code {
            KeyCode::Esc => { on_cancel(()); EventResult::Consumed }
            KeyCode::Enter => {
                // Phase 5: 将选中项注入 textarea，关闭 popup
                EventResult::Consumed
            }
            _ => EventResult::Ignored,
        }
    });

    Border(
        flex_direction: Direction::Vertical,
        border_style: Style::new().fg(theme::THINKING),
        top_title: Line::from(format!(" @{} ", prefix)).fg(theme::THINKING).bold(),
        width: Constraint::Length(50),
        height: Constraint::Length((items.len() + 2).min(10) as u16),
    ) {
        for (i, item) in items.iter().enumerate() {
            View(key: i, height: Constraint::Length(1)) {
                Text(text: Line::from(item.clone()).fg(theme::TEXT))
            }
        }
    }
}
```

- [ ] **验证**: `cargo check -p peri-tui`

---

### Task 4d: AppShell 集成

**Files:**
- Modify: `peri-tui/src/kit/app_shell.rs`（若不存在则创建）
- Modify: `peri-tui/src/kit/mod.rs`
- Modify: `peri-tui/src/runtime/main_loop.rs`

- [ ] **Step 1: 创建/修改 `kit/app_shell.rs`**

```rust
//! AppShell — ratatui-kit 根组件。
//! 集成 Global + Root handlers, Modal overlay, SessionColumn。

use ratatui_kit::{
    crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers},
    prelude::*,
    ratatui::layout::Direction,
};

use crate::app::App;
use crate::state_machine::State;
use super::event_handlers;

#[component]
pub fn AppShell(
    mut hooks: Hooks,
    app: &mut App,
    state: &mut State,
) -> impl Into<AnyElement<'static>> {
    let mut exit = hooks.use_exit();

    // ── 注册事件处理器 ──
    event_handlers::register_global_handlers(&mut hooks, exit.clone());
    event_handlers::register_root_handlers(&mut hooks, app, state);

    // ── 条件渲染 ──
    if app.global_ui.setup_wizard.is_some() {
        // Setup Wizard (Phase 5)
        return element!(Fragment {});
    }

    View(flex_direction: Direction::Vertical) {
        // Phase 5: SessionColumn 完整集成
        // Phase 2: 弹窗 Modal overlay
    }
}
```

- [ ] **Step 2: main_loop 简化**

```rust
// main_loop.rs: 删除 dispatch_fallback 调用
// 删除 merge_effects、execute_effects
// 简化为纯 ACP 事件处理循环 + 渲染
```

- [ ] **验证**: `cargo check -p peri-tui`

---

## 验收标准

- [ ] `kit/atoms.rs` + `kit/acp_bridge.rs` + `kit/event_handlers.rs` 创建且编译通过
- [ ] `kit/mention_popup.rs` 创建且编译通过
- [ ] `kit/app_shell.rs` 集成所有 handlers
- [ ] `main_loop.rs` 键盘 fallback 分支已删除（SM + use_event_handler 接管）
- [ ] `cargo check -p peri-tui` 通过
- [ ] 所有 v1 keyboard 快捷键功能正常（Ctrl+T/M/P/K, Esc rewind, Enter 提交 等）
