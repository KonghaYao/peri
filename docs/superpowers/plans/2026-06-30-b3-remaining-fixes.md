# B3 Remaining Fixes — Panel Data + Effects + Bugs

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix 5 `empty()` panel constructors, 3 `/cost` command stub, McpPanel mouse coordinate bug, and the `input_widget.rs` unused import.

**Architecture:** Each panel gets a `from_app(&App)` constructor that extracts data from `ServiceRegistry` fields where available. Panels without a data source (Agent, Hooks) keep `empty()` but gain a skeleton `from_app()` that delegates to `empty()` for API consistency. The `/cost` command returns `Effect::OpenPanel(PanelKind::Status)`.

**Tech Stack:** Rust, ratatui, tokio, parking_lot

---

### Task 1: Fix `input_widget.rs` unused import

**Files:**
- Modify: `peri-tui/src/ui/input_widget.rs:298`

- [ ] **Step 1: Remove the unused import**

Line 298 in the test module has `use ratatui::style::Color;` which is unused. Delete it:

```rust
// Before (lines 296-299):
mod tests {
    use super::*;
    use ratatui::style::Color;
    use ratatui::widgets::{Borders, Padding};
```

```rust
// After:
mod tests {
    use super::*;
    use ratatui::widgets::{Borders, Padding};
```

- [ ] **Step 2: Build check**

Run: `cargo build -p peri-tui 2>&1 | tail -5`
Expected: no warnings about unused import `Color`

- [ ] **Step 3: Commit**

```bash
git add peri-tui/src/ui/input_widget.rs
git commit -m "chore: remove unused import in input_widget test module

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>"
```

---

### Task 2: Fix McpPanel mouse coordinate double-offset bug

**Files:**
- Modify: `peri-tui/src/panel/panels/mcp.rs:489-522`

**Background:** `handle_mouse` computes `relative_y = mouse.row - area.y` (already relative to panel top), but then in comparisons and index calculations adds/subtracts `area.y` again — applying the panel offset twice. Other panels (TasksPanel, AgentPanel, HooksPanel) use the correct single-offset pattern.

- [ ] **Step 1: Fix the ServerList branch**

In `handle_mouse`, `McpView::ServerList` arm, replace:

```rust
McpView::ServerList => {
    let relative_y = mouse.row.saturating_sub(area.y);
    let header = 3u16;
    if relative_y >= area.y + header
        && relative_y < area.y + area.height
    {
        let clicked = (relative_y - area.y - header) as usize;
        if clicked < self.servers.len() {
            self.cursor = clicked;
        }
    }
}
```

With:

```rust
McpView::ServerList => {
    let relative_y = mouse.row.saturating_sub(area.y);
    let header = 3u16;
    if relative_y >= header && relative_y < area.height {
        let clicked = (relative_y - header) as usize;
        if clicked < self.servers.len() {
            self.cursor = clicked;
        }
    }
}
```

- [ ] **Step 2: Fix the ServerDetail branch**

Same file, `McpView::ServerDetail` arm, replace:

```rust
McpView::ServerDetail { actions, .. } => {
    let inner_y = mouse.row.saturating_sub(area.y);
    let meta_lines: u16 = 7;
    if inner_y > area.y + meta_lines {
        let clicked = (inner_y - area.y - meta_lines) as usize;
        if clicked < actions.len() {
            self.detail_cursor = clicked;
        }
    }
}
```

With:

```rust
McpView::ServerDetail { actions, .. } => {
    let inner_y = mouse.row.saturating_sub(area.y);
    let meta_lines: u16 = 7;
    if inner_y > meta_lines {
        let clicked = (inner_y - meta_lines) as usize;
        if clicked < actions.len() {
            self.detail_cursor = clicked;
        }
    }
}
```

- [ ] **Step 3: Build and test**

Run: `cargo test -p peri-tui --lib mcp -- 2>&1 | tail -5`
Expected: all mcp tests pass

- [ ] **Step 4: Commit**

```bash
git add peri-tui/src/panel/panels/mcp.rs
git commit -m "fix(mcp): remove duplicate area.y offset in handle_mouse coordinate calculations

relative_y already subtracts area.y once; the comparison and index
calculations were subtracting it again, causing mouse clicks to miss
their targets.

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>"
```

---

### Task 3: Fix `/cost` command stub

**Files:**
- Modify: `peri-tui/src/command/session/cost.rs`

**Background:** `CostCommand::execute()` returns `vec![]`. It should open the StatusPanel (which shows cost/context tabs) using `Effect::OpenPanel(PanelKind::Status)`.

- [ ] **Step 1: Read the current file to confirm it hasn't changed**

The file at `peri-tui/src/command/session/cost.rs` currently has an empty `execute()` body returning `vec![]`.

- [ ] **Step 2: Add PanelKind import and return Effect::OpenPanel**

Replace the full file content:

```rust
use crate::{
    app::{App, PanelKind},
    command::Command,
    runtime::effect::Effect,
};

pub struct CostCommand;

impl Command for CostCommand {
    fn name(&self) -> &str {
        "cost"
    }

    fn description(&self, _lc: &crate::i18n::LcRegistry) -> String {
        _lc.tr("command-cost-description")
    }

    fn execute(&self, _app: &mut App, _args: &str) -> Vec<Effect> {
        vec![Effect::OpenPanel(PanelKind::Status)]
    }
}
```

- [ ] **Step 3: Build check**

Run: `cargo build -p peri-tui 2>&1 | tail -5`
Expected: Build succeeds

- [ ] **Step 4: Verify the command is registered**

Run: `grep -r "CostCommand" peri-tui/src/command/`
Expected: Found in `session/mod.rs` (registration) and `session/cost.rs` (definition)

- [ ] **Step 5: Commit**

```bash
git add peri-tui/src/command/session/cost.rs
git commit -m "fix(cost): return Effect::OpenPanel(Status) from /cost slash command

Previously returned empty vec, making /cost a no-op. Now opens the
Status panel which displays cost and context usage information.

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>"
```

---

### Task 4: Add `MemoryPanel::from_app()` constructor

**Files:**
- Modify: `peri-tui/src/panel/panels/memory.rs`
- Modify: `peri-tui/src/panel/registry.rs`

**Background:** `MemoryPanel::empty()` creates a panel with an empty `cwd` string, so both "Project" and "User" CLAUDE.md paths resolve to non-existent files. `from_app()` should read `app.services.cwd` and the user's home directory to initialize the panel with real paths.

- [ ] **Step 1: Add `from_app()` method to MemoryPanel**

In `peri-tui/src/panel/panels/memory.rs`, after the `empty()` method (after line 127), add:

```rust
    /// Construct a panel from live App data.
    ///
    /// Reads `cwd` from `app.services` and resolves the user's home
    /// directory for project-level and global CLAUDE.md paths.
    pub fn from_app(app: &crate::app::App) -> Self {
        let home_dir = dirs::home_dir();
        Self::new(app.services.cwd.clone(), home_dir)
    }
```

- [ ] **Step 2: Update registry to use `from_app()`**

In `peri-tui/src/panel/registry.rs`, change line 99 from:

```rust
PanelKind::Memory => Box::new(super::panels::memory::MemoryPanel::empty()),
```

To:

```rust
PanelKind::Memory => Box::new(super::panels::memory::MemoryPanel::from_app(app)),
```

- [ ] **Step 3: Build check**

Run: `cargo build -p peri-tui 2>&1 | tail -5`
Expected: Build succeeds

- [ ] **Step 4: Run memory panel tests**

Run: `cargo test -p peri-tui --lib memory -- 2>&1 | tail -5`
Expected: all memory tests pass

- [ ] **Step 5: Commit**

```bash
git add peri-tui/src/panel/panels/memory.rs peri-tui/src/panel/registry.rs
git commit -m "feat(memory): add from_app() constructor using real cwd and home_dir

Previously MemoryPanel::empty() used an empty cwd, making both Project
and User CLAUDE.md paths resolve to non-existent files. from_app()
reads cwd from ServiceRegistry and resolves the user's home directory.

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>"
```

---

### Task 5: Add `BetasPanel::from_app()` constructor

**Files:**
- Modify: `peri-tui/src/panel/panels/betas.rs`
- Modify: `peri-tui/src/panel/registry.rs`

**Background:** `BetasPanel::empty()` reads from the `BETA_KEYS` const (currently empty). Since there are no active beta features, `from_app()` just delegates to `empty()` for now, but having the method establishes the API for when beta features are added.

- [ ] **Step 1: Add `from_app()` method to BetasPanel**

In `peri-tui/src/panel/panels/betas.rs`, after the `empty()` method (after line 66), add:

```rust
    /// Construct a panel from live App data.
    ///
    /// Currently delegates to `empty()` since there are no active beta
    /// features. When beta keys are added to `BETA_KEYS`, this can read
    /// their actual enabled state from `app.services.peri_config`.
    pub fn from_app(_app: &crate::app::App) -> Self {
        Self::empty()
    }
```

- [ ] **Step 2: Update registry to use `from_app()`**

In `peri-tui/src/panel/registry.rs`, change line 101 from:

```rust
PanelKind::Betas => Box::new(super::panels::betas::BetasPanel::empty()),
```

To:

```rust
PanelKind::Betas => Box::new(super::panels::betas::BetasPanel::from_app(app)),
```

- [ ] **Step 3: Build and test**

Run: `cargo test -p peri-tui --lib betas -- 2>&1 | tail -5`
Expected: all betas tests pass

- [ ] **Step 4: Commit**

```bash
git add peri-tui/src/panel/panels/betas.rs peri-tui/src/panel/registry.rs
git commit -m "feat(betas): add from_app() constructor skeleton

Establishes the from_app() API for consistency with other panels.
Currently delegates to empty() since BETA_KEYS is empty.

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>"
```

---

### Task 6: Add `TasksPanel::from_app()` constructor

**Files:**
- Modify: `peri-tui/src/panel/panels/tasks.rs`
- Modify: `peri-tui/src/panel/registry.rs`

**Background:** `TasksPanel::empty()` creates a panel with no tasks. The `CronState` in `ServiceRegistry` has a `CronScheduler` that can list tasks via `list_tasks()`. We need to convert the `CronTask` runtime types to `CronTaskDto` DTOs. Check the `CronTaskDto` type to understand the mapping.

- [ ] **Step 1: Check `CronTaskDto` structure**

Run: `grep -A 15 "pub struct CronTaskDto" peri-acp-types/src/summary/mod.rs`
Expected output shows the fields: `id`, `schedule`, `prompt`, `next_fire`, `enabled`

- [ ] **Step 2: Check `CronTask` structure**

Run: `grep -A 15 "pub struct CronTask" peri-middlewares/src/cron/mod.rs`
Expected output shows the runtime type fields

- [ ] **Step 3: Add `from_app()` method to TasksPanel**

In `peri-tui/src/panel/panels/tasks.rs`, after the `empty()` method (after line 67), add:

```rust
    /// Construct a panel from live App data.
    ///
    /// Reads cron tasks from `app.services.cron.scheduler` and converts
    /// `CronTask` runtime types to panel-local `CronTaskDto` DTOs.
    /// Falls back to empty if the scheduler lock is not available.
    pub fn from_app(app: &crate::app::App) -> Self {
        let tasks: Vec<CronTaskDto> = app
            .services
            .cron
            .scheduler
            .try_lock()
            .map(|scheduler| {
                scheduler
                    .list_tasks()
                    .into_iter()
                    .map(|t| CronTaskDto {
                        id: t.id.clone(),
                        schedule: t.schedule.clone(),
                        prompt: t.prompt.clone(),
                        next_fire: t.next_fire.clone(),
                        enabled: t.enabled,
                    })
                    .collect()
            })
            .unwrap_or_default();

        if tasks.is_empty() {
            Self::empty()
        } else {
            Self::new(tasks)
        }
    }
```

NOTE: use `try_lock()` from `parking_lot::Mutex` — non-blocking, returns `None` if the lock is held. If the `CronState.scheduler` is `Arc<Mutex<CronScheduler>>`, access it via `app.services.cron.scheduler.try_lock()`.

Check the exact mutex wrapper by reading line 8 of `cron_state.rs`:
`pub scheduler: Arc<Mutex<CronScheduler>>`
— `parking_lot::Mutex`, so `try_lock()` is correct.

- [ ] **Step 4: Update registry to use `from_app()`**

In `peri-tui/src/panel/registry.rs`, change line 100 from:

```rust
PanelKind::Tasks => Box::new(super::panels::tasks::TasksPanel::empty()),
```

To:

```rust
PanelKind::Tasks => Box::new(super::panels::tasks::TasksPanel::from_app(app)),
```

- [ ] **Step 5: Build and test**

Run: `cargo test -p peri-tui --lib tasks -- 2>&1 | tail -5`
Expected: all tasks tests pass

- [ ] **Step 6: Commit**

```bash
git add peri-tui/src/panel/panels/tasks.rs peri-tui/src/panel/registry.rs
git commit -m "feat(tasks): add from_app() constructor loading cron tasks from CronState

Reads cron tasks from CronScheduler via try_lock() and converts
CronTask runtime types to CronTaskDto DTOs. Falls back to empty
if the scheduler lock is unavailable.

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>"
```

---

### Task 7: Add `AgentPanel::from_app()` and `HooksPanel::from_app()` skeletons

**Files:**
- Modify: `peri-tui/src/panel/panels/agent.rs`
- Modify: `peri-tui/src/panel/panels/hooks.rs`
- Modify: `peri-tui/src/panel/registry.rs`

**Background:** Agent and Hooks panels get their data asynchronously via ACP queries, not from `ServiceRegistry` fields. There is no immediate data source to populate them at construction time. However, adding `from_app()` methods that delegate to `empty()` establishes the API for future use and makes the registry consistent.

- [ ] **Step 1: Add `from_app()` to AgentPanel**

In `peri-tui/src/panel/panels/agent.rs`, after the `empty()` method, add:

```rust
    /// Construct a panel from live App data.
    ///
    /// Agent data arrives asynchronously via ACP queries (scan results
    /// from `.claude/agents/`). Currently delegates to `empty()` with
    /// data populated later via `set_agents()` when ACP results arrive.
    pub fn from_app(_app: &crate::app::App) -> Self {
        Self::empty()
    }
```

- [ ] **Step 2: Add `from_app()` to HooksPanel**

In `peri-tui/src/panel/panels/hooks.rs`, after the `empty()` method, add:

```rust
    /// Construct a panel from live App data.
    ///
    /// Hook data arrives asynchronously via ACP queries. Currently
    /// delegates to `empty()` with data populated later via
    /// `set_hooks()` when ACP results arrive.
    pub fn from_app(_app: &crate::app::App) -> Self {
        Self::empty()
    }
```

- [ ] **Step 3: Update registry**

In `peri-tui/src/panel/registry.rs`, change lines 89-90 from:

```rust
PanelKind::Agent => Box::new(super::panels::agent::AgentPanel::empty()),
PanelKind::Hooks => Box::new(super::panels::hooks::HooksPanel::empty()),
```

To:

```rust
PanelKind::Agent => Box::new(super::panels::agent::AgentPanel::from_app(app)),
PanelKind::Hooks => Box::new(super::panels::hooks::HooksPanel::from_app(app)),
```

- [ ] **Step 4: Build and test**

Run: `cargo test -p peri-tui --lib -- agent hooks 2>&1 | tail -5`
Expected: all agent and hooks tests pass

- [ ] **Step 5: Verify registry has zero `empty()` calls**

Run: `grep -n 'empty()' peri-tui/src/panel/registry.rs`
Expected: zero results — all 14 panels now use `from_app(app)`

- [ ] **Step 6: Run full test suite**

Run: `cargo test -p peri-tui --lib 2>&1 | tail -10`
Expected: all tests pass

- [ ] **Step 7: Commit**

```bash
git add peri-tui/src/panel/panels/agent.rs peri-tui/src/panel/panels/hooks.rs peri-tui/src/panel/registry.rs
git commit -m "feat(agent,hooks): add from_app() skeleton constructors

Agent and Hooks data arrive asynchronously via ACP queries, so
from_app() delegates to empty() for now. This makes the registry
fully consistent — all 14 panels now use from_app(app) constructors.

Co-Authored-By: glm-5.2 <zai-org@claude-code-best.win>"
```

---

## Remaining Issues (Not Yet Planned)

The following were identified in the investigation but are deferred to future plans because they require design decisions or are complex multi-module changes:

1. **Effect stubs** (`ShowNotification`, `UpdateConfig`, `MemoryPanelOpenEditor`) — need real implementations:
   - `ShowNotification`: Push a system note to the message view or show a transient notification popup
   - `UpdateConfig`: Persist key=value config changes through `peri_config`, sync to ACP server
   - `MemoryPanelOpenEditor`: Spawn `$EDITOR` on the selected memory file, watch for changes

2. **StatusPanel real-time data** — currently shows static data from `from_app()` snapshot. Needs a mechanism to refresh during panel lifetime (polling or ACP push).
