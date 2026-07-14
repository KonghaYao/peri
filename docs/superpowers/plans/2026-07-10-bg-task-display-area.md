# 后台显示区域 (BgTaskArea) 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 AppShell 根层 StatusBar 下方新增后台显示区域，展示活跃的 bg subagent / bg shell / workflow 任务，每行格式 `◎ agent_type desc current_tool · N tools`。

**Architecture:** 新增独立 atom `BG_DISPLAY`（`Vec<BgDisplayEntry>`）和 `BG_AGENT_IDS`（`HashSet<String>`），由 `dispatch_and_notify` 统一写入；新建 ratatui-kit 函数组件 `BgTaskArea` 从 atom 读取并渲染。与现有 `BG_TASKS` / StatusBar 计数 / Tasks 面板互不干扰。

**Tech Stack:** Rust + ratatui-kit 0.10 (AtomStatic, #[component], element! macro) + std::time::Instant + std::collections::HashSet

**设计文档:** `TUI-TOOLCALL.md` 第 10 节

---

## 文件清单

| 文件 | 操作 | 职责 |
|------|------|------|
| `peri-tui/src/kit/acp_types.rs` | Modify | 给 `SubagentStarted` 加 `is_background` 字段 + 反序列化 |
| `peri-tui/src/kit/acp_notifier.rs` | Modify | `convert_agent_event` 传递 `is_background` |
| `peri-tui/src/kit/atoms.rs` | Modify | 新增 `BgDisplayEntry` + `BG_DISPLAY` + `BG_AGENT_IDS` atom |
| `peri-tui/src/kit/acp_events.rs` | Modify | `dispatch_and_notify` 中追加 BG_DISPLAY 写入路径 |
| `peri-tui/src/kit/bg_task_area.rs` | **Create** | BgTaskArea 组件 |
| `peri-tui/src/kit/app_shell.rs` | Modify | 在 StatusBar 下方插入 BgTaskArea |
| `peri-tui/src/kit/mod.rs` | Modify | 注册 `pub mod bg_task_area;` |

---

### Task 1: 给 SubagentStarted 添加 is_background 字段

**Files:**
- Modify: `peri-tui/src/kit/acp_types.rs:719-722` (struct)
- Modify: `peri-tui/src/kit/acp_types.rs:842-849` (deserialization)
- Modify: `peri-tui/src/kit/acp_notifier.rs:73-80` (convert_agent_event)

- [ ] **Step 1: 在 AcpEventData::SubagentStarted 中加 is_background 字段**

打开 `peri-tui/src/kit/acp_types.rs`，找到第 719 行：

```rust
    /// `"subagent-started"` -- sub-agent created, TUI opens a collapsible group.
    SubagentStarted {
        agent_id: String,
        agent_name: String,
    },
```

改为：

```rust
    /// `"subagent-started"` -- sub-agent created, TUI opens a collapsible group.
    SubagentStarted {
        agent_id: String,
        agent_name: String,
        is_background: bool,
    },
```

- [ ] **Step 2: 更新反序列化逻辑**

找到 `acp_types.rs:842-849` 的 `"subagent-started"` 分支：

```rust
            "subagent-started" => {
                let agent_id = data["agent_id"].as_str().unwrap_or("").to_string();
                let agent_name = data["agent_name"].as_str().unwrap_or("").to_string();
                AcpEventData::SubagentStarted {
                    agent_id,
                    agent_name,
                }
            }
```

改为：

```rust
            "subagent-started" => {
                let agent_id = data["agent_id"].as_str().unwrap_or("").to_string();
                let agent_name = data["agent_name"].as_str().unwrap_or("").to_string();
                let is_background = data["is_background"].as_bool().unwrap_or(false);
                AcpEventData::SubagentStarted {
                    agent_id,
                    agent_name,
                    is_background,
                }
            }
```

- [ ] **Step 3: 更新 convert_agent_event 传递 is_background**

打开 `peri-tui/src/kit/acp_notifier.rs`，找到第 73-80 行：

```rust
        AcpEvent::SubagentStarted {
            agent_name,
            instance_id,
            ..
        } => Some(AcpEventData::SubagentStarted {
            agent_id: instance_id,
            agent_name,
        }),
```

改为（不再用 `..` 丢弃 is_background）：

```rust
        AcpEvent::SubagentStarted {
            agent_name,
            instance_id,
            is_background,
        } => Some(AcpEventData::SubagentStarted {
            agent_id: instance_id,
            agent_name,
            is_background,
        }),
```

- [ ] **Step 4: 更新 dispatch_and_notify 中 SubagentStarted 匹配**

打开 `peri-tui/src/kit/acp_events.rs`，找到第 421-424 行：

```rust
        SubagentStarted {
            agent_id,
            agent_name,
        } => {
```

改为：

```rust
        SubagentStarted {
            agent_id,
            agent_name,
            is_background: _,
        } => {
```

- [ ] **Step 5: 编译验证**

```bash
cargo build -p peri-tui 2>&1 | head -20
```

预期：编译通过（可能有一个未使用变量 `is_background` 的 warning，Task 3 会消费它）。

- [ ] **Step 6: Commit**

```bash
git add peri-tui/src/kit/acp_types.rs peri-tui/src/kit/acp_notifier.rs peri-tui/src/kit/acp_events.rs
git commit -m "feat(tui): add is_background field to SubagentStarted event data"
```

---

### Task 2: 创建 BG_DISPLAY 和 BG_AGENT_IDS atom

**Files:**
- Modify: `peri-tui/src/kit/atoms.rs` (在 BG_TASKS 旁边追加)

- [ ] **Step 1: 添加 BgDisplayEntry 结构体和两个 atom**

打开 `peri-tui/src/kit/atoms.rs`，在第 328 行 `BG_TASKS` 定义之后，`NOTIFICATION` 定义之前，插入：

```rust
// ── Background Display Area (后台显示区域) ────────────────────────────────────

/// 后台显示区域条目（由 bg-task-* + subagent tool 事件维护）
#[derive(Debug, Clone)]
pub struct BgDisplayEntry {
    /// 唯一标识：task_id 或 agent_id（bg-task-started 的 task_id，SubagentStarted 的 instance_id）
    pub id: String,
    /// 任务类型标签："coder" / "explorer" / "bg-shell" / "workflow"
    pub agent_type: String,
    /// 任务描述（来自 BgTaskEntry.summary）
    pub desc: String,
    /// 当前执行的工具名（None 为空闲态）
    pub current_tool: Option<String>,
    /// 已完成工具调用计数
    pub tool_count: u32,
    /// false → 3s 倒计时中，到期后渲染层移除
    pub is_active: bool,
    /// 失败标志
    pub is_error: bool,
    /// 完成时间（3s 倒计时起点）
    pub completed_at: Option<Instant>,
}

/// 后台显示区域条目列表（仅活跃 + 3s 缓冲中的任务）
pub static BG_DISPLAY: AtomStatic<Vec<BgDisplayEntry>> = AtomStatic::new(|| Vec::new());

/// 后台 agent_id 集合——用于判断 tool 事件是否属于后台任务
/// key = SubagentStarted.instance_id (is_background=true)
pub static BG_AGENT_IDS: AtomStatic<std::collections::HashSet<String>> =
    AtomStatic::new(|| std::collections::HashSet::new());
```

- [ ] **Step 2: 编译验证**

```bash
cargo build -p peri-tui 2>&1 | head -20
```

预期：编译通过（atoms 定义尚未被引用，仅 unused warning）。

- [ ] **Step 3: Commit**

```bash
git add peri-tui/src/kit/atoms.rs
git commit -m "feat(tui): add BG_DISPLAY and BG_AGENT_IDS atoms for bg task area"
```

---

### Task 3: 在 dispatch_and_notify 中追加 BG_DISPLAY 写入

**Files:**
- Modify: `peri-tui/src/kit/acp_events.rs:60-630`

- [ ] **Step 1: 更新 SubagentStarted 分支——注册后台 agent_id**

找到 `acp_events.rs` 第 421-431 行（Task 1 Step 4 已改）：

```rust
        SubagentStarted {
            agent_id,
            agent_name,
            is_background: _,
        } => {
            state
                .current_turn
                .start_subagent(agent_id.clone(), agent_name.clone());
            state.variant = 1;
            state.phase = SessionPhase::PromptRunning;
            push_view_models(state);
            push_acp_state(state);
        }
```

改为：

```rust
        SubagentStarted {
            agent_id,
            agent_name,
            is_background,
        } => {
            state
                .current_turn
                .start_subagent(agent_id.clone(), agent_name.clone());
            // 仅后台 subagent 注册到 BG_AGENT_IDS
            if *is_background {
                BG_AGENT_IDS.state().write().insert(agent_id.clone());
            }
            state.variant = 1;
            state.phase = SessionPhase::PromptRunning;
            push_view_models(state);
            push_acp_state(state);
        }
```

- [ ] **Step 2: 更新 ToolStarted 分支——后台子 agent 工具更新**

找到 `acp_events.rs` 第 113-138 行的 `ToolStarted(ts)` 分支。在现有的 subagent 工具路由代码块末尾（`push_acp_state(state);` 之前），追加后台任务工具更新：

```rust
        ToolStarted(ts) => {
            if let Some(agent_id) = ts.agent_id.as_deref() {
                // 现有逻辑：路由到 SubAgentAccumulator.child_turn
                let routed = state.current_turn.start_subagent_tool(
                    agent_id,
                    ToolCardAccumulator::new(
                        ts.tool_id.clone(),
                        ts.tool_name.clone(),
                        ts.input_summary.clone(),
                    ),
                );
                if !routed {
                    tracing::trace!(agent_id, tool_id = %ts.tool_id, "kit bridge: subagent tool start has no active group");
                }
                // 新逻辑：更新后台显示区域
                if BG_AGENT_IDS.state().read().contains(agent_id) {
                    let mut display = BG_DISPLAY.state().write();
                    if let Some(entry) = display.iter_mut().find(|e| e.id == agent_id) {
                        entry.current_tool = Some(ts.tool_name.clone());
                    }
                }
                state.variant = 1;
                state.phase = SessionPhase::PromptRunning;
                push_view_models(state);
            } else {
                state.current_turn.start_tool(ToolCardAccumulator::new(
                    ts.tool_id.clone(),
                    ts.tool_name.clone(),
                    ts.input_summary.clone(),
                ));
                state.variant = 1;
                state.phase = SessionPhase::PromptRunning;
                push_view_models(state);
            }
            push_acp_state(state);
        }
```

- [ ] **Step 3: 更新 ToolEnded 分支——后台子 agent 工具完成**

找到 `acp_events.rs` 第 141-163 行的 `ToolEnded(te)` 分支。同样在 `push_acp_state(state);` 之前追加：

```rust
        ToolEnded(te) => {
            if let Some(agent_id) = te.agent_id.as_deref() {
                // 现有逻辑
                let routed = state.current_turn.end_subagent_tool(
                    agent_id,
                    &te.tool_id,
                    te.output_summary.clone(),
                    te.is_error,
                );
                if !routed {
                    tracing::trace!(agent_id, tool_id = %te.tool_id, "kit bridge: subagent tool end has no active group");
                }
                // 新逻辑：更新后台显示区域——清除 current_tool，递增 tool_count
                if BG_AGENT_IDS.state().read().contains(agent_id) {
                    let mut display = BG_DISPLAY.state().write();
                    if let Some(entry) = display.iter_mut().find(|e| e.id == agent_id) {
                        entry.current_tool = None;
                        entry.tool_count += 1;
                    }
                }
                state.variant = 1;
                state.phase = SessionPhase::PromptRunning;
                push_view_models(state);
            } else {
                state
                    .current_turn
                    .end_tool(&te.tool_id, te.output_summary.clone(), te.is_error);
                state.variant = 1;
                state.phase = SessionPhase::PromptRunning;
                push_view_models(state);
            }
            push_acp_state(state);
        }
```

- [ ] **Step 4: 更新 SubagentStopped 分支——清理 BG_AGENT_IDS**

找到 `acp_events.rs` 第 433-439 行：

```rust
        SubagentStopped { agent_id } => {
            state.current_turn.stop_subagent(agent_id);
            state.variant = 1;
            state.phase = SessionPhase::PromptRunning;
            push_view_models(state);
            push_acp_state(state);
        }
```

改为：

```rust
        SubagentStopped { agent_id } => {
            state.current_turn.stop_subagent(agent_id);
            // 清理后台 agent_id 注册
            BG_AGENT_IDS.state().write().remove(agent_id);
            state.variant = 1;
            state.phase = SessionPhase::PromptRunning;
            push_view_models(state);
            push_acp_state(state);
        }
```

- [ ] **Step 5: 更新 BG_TASKS 事件分支——同步 BG_DISPLAY**

找到 `acp_events.rs` 第 596-628 行的 `§4.7 Background Tasks` 区域。

**BgTaskSnapshot**（第 596-598 行）追加 BG_DISPLAY 全量替换：

```rust
        BgTaskSnapshot(tasks) => {
            BG_TASKS.state().write().clone_from(tasks);
            // 新逻辑：从快照构造 BG_DISPLAY 条目
            let entries: Vec<BgDisplayEntry> = tasks
                .iter()
                .map(|t| BgDisplayEntry {
                    id: t.task_id.clone(),
                    agent_type: t.kind.clone(),
                    desc: t.summary.clone(),
                    current_tool: None,
                    tool_count: 0,
                    is_active: true,
                    is_error: false,
                    completed_at: None,
                })
                .collect();
            BG_DISPLAY.state().write().clone_from(&entries);
        }
```

**BgTaskStarted**（第 599-601 行）追加 push：

```rust
        BgTaskStarted(task) => {
            BG_TASKS.state().write().push(task.clone());
            // 新逻辑：创建后台显示条目
            BG_DISPLAY.state().write().push(BgDisplayEntry {
                id: task.task_id.clone(),
                agent_type: task.kind.clone(),
                desc: task.summary.clone(),
                current_tool: None,
                tool_count: 0,
                is_active: true,
                is_error: false,
                completed_at: None,
            });
        }
```

**BgTaskCompleted**（第 602-624 行）追加标记完成：

```rust
        BgTaskCompleted {
            task_id,
            success,
            duration_ms,
        } => {
            BG_TASKS.state().write().retain(|t| t.task_id != *task_id);
            // 新逻辑：标记后台显示条目为完成（3s 倒计时）
            let now = Instant::now();
            let mut display = BG_DISPLAY.state().write();
            if let Some(entry) = display.iter_mut().find(|e| e.id == *task_id) {
                entry.is_active = false;
                entry.is_error = !*success;
                entry.completed_at = Some(now);
            }
            // 现有通知逻辑不变
            let msg = if *success {
                format!(
                    "[✓] {} 完成 ({:.0}s)",
                    task_id,
                    *duration_ms as f64 / 1000.0
                )
            } else {
                format!(
                    "[✗] {} 失败 ({:.0}s)",
                    task_id,
                    *duration_ms as f64 / 1000.0
                )
            };
            NOTIFICATION.state().write().replace(Notification {
                message: msg,
                until: Instant::now() + Duration::from_millis(1500),
            });
        }
```

**BgTaskCancelled**（第 626-628 行）追加标记失败：

```rust
        BgTaskCancelled { task_id, .. } => {
            BG_TASKS.state().write().retain(|t| t.task_id != *task_id);
            // 新逻辑：标记后台显示条目为失败
            let now = Instant::now();
            let mut display = BG_DISPLAY.state().write();
            if let Some(entry) = display.iter_mut().find(|e| e.id == *task_id) {
                entry.is_active = false;
                entry.is_error = true;
                entry.completed_at = Some(now);
            }
        }
```

- [ ] **Step 6: 编译验证**

```bash
cargo build -p peri-tui 2>&1 | head -30
```

预期：编译通过。可能需要补齐 `use` 导入（atoms 中的 BgDisplayEntry / BG_DISPLAY / BG_AGENT_IDS 已在 acp_events.rs scope 中因为 `use crate::kit::atoms::*`）。

- [ ] **Step 7: Commit**

```bash
git add peri-tui/src/kit/acp_events.rs
git commit -m "feat(tui): wire BG_DISPLAY atom writes into dispatch_and_notify"
```

---

### Task 4: 创建 BgTaskArea 组件

**Files:**
- Create: `peri-tui/src/kit/bg_task_area.rs`

- [ ] **Step 1: 创建组件文件**

```rust
//! 后台任务显示区域组件。
//!
//! 位于 AppShell 根层 StatusBar 下方，展示活跃的 bg subagent / bg shell / workflow 任务。
//! 每行格式：`◎ agent_type desc current_tool · N tools`。
//! 最大 5 行，超出显示 `… N more`。纯展示，不响应键盘/鼠标。

use crate::kit::atoms::{self, BgDisplayEntry};
use ratatui_kit::{
    prelude::*,
    ratatui::{
        layout::{Constraint, Direction},
        style::{Color, Style, Stylize},
        text::{Line, Span},
        widgets::Paragraph,
    },
};
use std::time::Instant;

/// 后台显示区域最大可见行数
const MAX_VISIBLE_ROWS: usize = 5;

/// 完成后保留时长（秒）
const DONE_KEEP_SECS: u64 = 3;

/// 状态符号
mod status_symbol {
    pub const IDLE: &str = "\u{25CE}";       // ◎
    pub const RUNNING: &str = "\u{25CF}";    // ●
    pub const DONE: &str = "\u{2714}";       // ✔
    pub const ERROR: &str = "\u{2717}";      // ✗
}

#[component]
pub fn BgTaskArea(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let display = hooks.use_atom(&atoms::BG_DISPLAY);
    // 订阅渲染心跳，确保闪烁动画持续更新
    let _heartbeat = hooks.use_atom(&atoms::RENDER_HEARTBEAT);

    let entries = display.read();
    let now = Instant::now();
    // 渲染计数器用于运行中条目闪烁
    let render_count = atoms::RENDER_CALL_COUNT.load(std::sync::atomic::Ordering::Relaxed);

    // 过滤过期条目（is_active=false 且 elapsed > 3s）
    let mut active: Vec<&BgDisplayEntry> = entries
        .iter()
        .filter(|e| {
            e.is_active
                || e.completed_at.map_or(true, |t| now.duration_since(t).as_secs() < DONE_KEEP_SECS)
        })
        .collect();

    if active.is_empty() {
        // 无条目 → 高度 0，不渲染
        return element! {
            View(
                flex_direction: Direction::Vertical,
                width: Constraint::Fill(1),
                height: Constraint::Length(0),
            ) {}
        };
    }

    // 排序：活跃在前，完成/失败在后
    active.sort_by_key(|e| (!e.is_active, e.completed_at));

    let visible_count = active.len().min(MAX_VISIBLE_ROWS);
    let overflow_count = active.len().saturating_sub(MAX_VISIBLE_ROWS);

    // 构建可见行
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(visible_count + 1);

    for entry in active.iter().take(MAX_VISIBLE_ROWS) {
        let line = render_entry_line(entry, render_count);
        lines.push(line);
    }

    // 溢出行
    if overflow_count > 0 {
        lines.push(Line::from(Span::styled(
            format!("… {} more", overflow_count),
            Style::default().fg(Color::Gray).add_modifier(ratatui::style::Modifier::DIM),
        )));
    }

    let height = lines.len() as u16;

    element! {
        View(
            flex_direction: Direction::Vertical,
            width: Constraint::Fill(1),
            height: Constraint::Length(height),
        ) {
            Text(text: Paragraph::new(lines))
        }
    }
}

/// 渲染单行：`◎ agent_type  desc  current_tool · N tools`
fn render_entry_line(entry: &BgDisplayEntry, render_count: u64) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(5);

    // 1. 状态符号
    let (symbol, color, blink) = entry_state(entry, render_count);
    let mut symbol_style = Style::default().fg(color);
    if blink {
        symbol_style = symbol_style.add_modifier(ratatui::style::Modifier::HIDDEN);
    }
    spans.push(Span::styled(symbol.to_string(), symbol_style));
    spans.push(Span::raw(" "));

    // 2. agent_type（dim 色）
    spans.push(Span::styled(
        entry.agent_type.clone(),
        Style::default().fg(Color::Gray).add_modifier(ratatui::style::Modifier::DIM),
    ));
    spans.push(Span::raw("  "));

    // 3. desc（弹性，尾部截断占剩余空间）
    // 注：ratatui 不做自动截断，这里直接放全部 desc，让终端处理
    spans.push(Span::raw(entry.desc.clone()));

    // 4. tool_call（仅当有 current_tool 时显示）
    if let Some(ref tool) = entry.current_tool {
        spans.push(Span::raw("  "));
        let tool_text = if entry.tool_count > 0 {
            format!("{} · {} tools", tool, entry.tool_count)
        } else {
            tool.clone()
        };
        spans.push(Span::styled(
            tool_text,
            Style::default().fg(Color::White),
        ));
    } else if entry.tool_count > 0 && entry.current_tool.is_none() && !entry.is_active {
        // 已完成且无当前工具 → 显示工具计数
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("· {} tools", entry.tool_count),
            Style::default().fg(Color::Green),
        ));
    }

    Line::from(spans)
}

/// 判定条目的状态符号、颜色、是否闪烁
fn entry_state(entry: &BgDisplayEntry, render_count: u64) -> (&'static str, Color, bool) {
    if entry.is_error {
        // 失败：红色 ✗，不闪
        return (status_symbol::ERROR, Color::Red, false);
    }
    if !entry.is_active {
        // 完成：绿色 ✔，不闪
        return (status_symbol::DONE, Color::Green, false);
    }
    if entry.current_tool.is_some() {
        // 运行中：白色 ●，800ms 闪烁
        let blink = (render_count / 16) % 2 == 0;
        return (status_symbol::RUNNING, Color::White, blink);
    }
    // 空闲：黄色 ◎，不闪
    (status_symbol::IDLE, Color::Yellow, false)
}
```

- [ ] **Step 2: 编译验证**

```bash
cargo build -p peri-tui 2>&1 | head -30
```

预期：编译通过（组件尚未被引用）。

- [ ] **Step 3: Commit**

```bash
git add peri-tui/src/kit/bg_task_area.rs
git commit -m "feat(tui): add BgTaskArea component for background task display"
```

---

### Task 5: 集成到 AppShell

**Files:**
- Modify: `peri-tui/src/kit/app_shell.rs:70-74`

- [ ] **Step 1: 在 AppShell 中引用 BgTaskArea**

打开 `peri-tui/src/kit/app_shell.rs`，在现有 import 区域追加：

```rust
use crate::kit::bg_task_area::BgTaskArea;
```

- [ ] **Step 2: 在 StatusBar 下方插入 BgTaskArea**

找到 `app_shell.rs` 第 70-73 行：

```rust
                    View(
                        flex_direction: Direction::Vertical,
                        width: Constraint::Fill(1),
                        height: Constraint::Fill(1),
                    ) {
                        SessionColumn()
                        StatusBar()
                    }
```

改为：

```rust
                    View(
                        flex_direction: Direction::Vertical,
                        width: Constraint::Fill(1),
                        height: Constraint::Fill(1),
                    ) {
                        SessionColumn()
                        StatusBar()
                        BgTaskArea()
                    }
```

- [ ] **Step 3: 编译验证**

```bash
cargo build -p peri-tui 2>&1 | head -30
```

预期：编译通过。

- [ ] **Step 4: Commit**

```bash
git add peri-tui/src/kit/app_shell.rs
git commit -m "feat(tui): integrate BgTaskArea below StatusBar in AppShell"
```

---

### Task 6: 注册模块

**Files:**
- Modify: `peri-tui/src/kit/mod.rs`

- [ ] **Step 1: 添加 pub mod 声明**

在 `mod.rs` 中找到模块声明区域（字母序，`app_shell.rs` 之后）：

```rust
pub mod app_shell;
```

之后插入：

```rust
pub mod bg_task_area;
```

- [ ] **Step 2: 编译验证**

```bash
cargo build -p peri-tui 2>&1 | head -30
```

预期：编译通过。

- [ ] **Step 3: Commit**

```bash
git add peri-tui/src/kit/mod.rs
git commit -m "chore(tui): register bg_task_area module"
```

---

### Task 7: 全量构建 + 测试

- [ ] **Step 1: 全量构建**

```bash
cargo build --workspace 2>&1 | tail -5
```

预期：编译通过，无错误。

- [ ] **Step 2: 运行现有测试**

```bash
cargo test -p peri-tui --lib 2>&1 | tail -10
```

预期：所有现有测试通过。

- [ ] **Step 3: 运行 ACP 层测试**

```bash
cargo test -p peri-acp --lib 2>&1 | tail -10
```

预期：所有现有测试通过。

- [ ] **Step 4: 运行 agent 层测试**

```bash
cargo test -p peri-agent --lib 2>&1 | tail -10
```

预期：所有现有测试通过。

- [ ] **Step 5: 运行 middlewares 层测试**

```bash
cargo test -p peri-middlewares --lib 2>&1 | tail -10
```

预期：所有现有测试通过。

- [ ] **Step 6: 代码风格检查**

```bash
cargo fmt --check --all && cargo clippy --workspace -- -D warnings 2>&1 | tail -20
```

预期：fmt 通过，clippy 无 warning。

---

## 验收标准

1. **后台任务可见**：在 Peri TUI 中触发后台任务（`/bg` 命令或 Agent 工具 background 模式），StatusBar 下方出现任务条目行
2. **状态转换正确**：任务经历 `◎ 空闲 → ● 运行 → ◎ 空闲 → ✔ 完成` 或 `✗ 失败` 状态转换
3. **工具名和计数**：工具执行时显示工具名，完成后计数递增
4. **3 秒后消失**：任务完成后条目保留 3 秒，然后消失
5. **溢出处理**：超过 5 条任务时最后一行显示 `… N more`
6. **与现有系统共存**：StatusBar 计数、Tasks 面板、消息区 SubAgentGroup 气泡不受影响
